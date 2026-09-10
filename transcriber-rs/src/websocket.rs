use std::collections::HashMap;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::messages::{AudioEvent, MetricsEvent, TranscriptEvent, WsCommand};

// ── OpenAI Realtime Protocol Types ──────────────────────────────────

#[derive(Serialize)]
struct SessionUpdate {
    r#type: &'static str,
    session: SessionConfig,
}

#[derive(Serialize)]
struct SessionConfig {
    r#type: &'static str,
    audio: AudioConfig,
}

#[derive(Serialize)]
struct AudioConfig {
    input: AudioInput,
}

#[derive(Serialize)]
struct AudioInput {
    transcription: InputAudioTranscription,
}

#[derive(Serialize)]
struct InputAudioTranscription {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    delay: Option<String>,
}

#[derive(Serialize)]
struct AudioBufferAppend {
    r#type: &'static str,
    audio: String,
}

/// Server → Client: Generic event envelope for all message types.
#[derive(Deserialize, Debug)]
struct ServerEvent {
    r#type: String,
    #[serde(default)]
    item_id: Option<String>,
    #[serde(default)]
    transcript: Option<String>,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    item: Option<ItemData>,
    #[serde(default)]
    error: Option<ErrorData>,
}

#[derive(Deserialize, Debug)]
struct ItemData {
    id: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ErrorData {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

// ── WebSocket Connection ────────────────────────────────────────────

// Transcription-only sessions use `?intent=transcription`. Connecting with
// `?model=gpt-realtime` instead opens a *realtime* (speech-to-speech) session,
// and the server then rejects our `session.update` with session.type:"transcription"
// as "Passing a transcription session update event to a realtime session is not allowed."
// The actual transcription engine is selected via session.update → audio.input.transcription.model.
const WS_URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";

/// Result of a single WebSocket connection attempt.
pub enum ConnectionResult {
    /// Connection was established then lost — reconnect (reset backoff)
    Reconnect,
    /// Connection failed to establish — reconnect (backoff=true for server errors)
    ConnectFailed { backoff: bool },
    /// Connection ended normally, do not reconnect
    Done,
}

/// Run a single WebSocket connection.
///
/// Called from main's reconnection loop. Takes `&mut` references to channels
/// so they survive across reconnections. Returns whether to reconnect.
///
/// This function:
/// 1. Connects to the OpenAI Realtime API
/// 2. Sends the transcription_session.update config
/// 3. Loops: forwards audio from ws_cmd_rx to the WS, dispatches server events
/// 4. Returns when disconnected (caller decides whether to reconnect)
pub async fn run_connection(
    api_key: &str,
    model: &str,
    delay: &str,
    ws_cmd_rx: &mut mpsc::Receiver<WsCommand>,
    audio_event_tx: &mpsc::Sender<AudioEvent>,
    transcript_tx: &mpsc::Sender<TranscriptEvent>,
    metrics_tx: &mpsc::Sender<MetricsEvent>,
    cancel: &CancellationToken,
) -> ConnectionResult {
    match do_connection(
        api_key,
        model,
        delay,
        ws_cmd_rx,
        audio_event_tx,
        transcript_tx,
        metrics_tx,
        cancel,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            let err_str = e.to_string();
            error!("WebSocket error: {err_str}");
            metrics_tx.send(MetricsEvent::WebSocketError).await.ok();
            // Local failures (DNS, timeout, refused) → retry fast
            // Server failures (rate limit, 4xx, 5xx) → backoff
            let is_local = err_str.contains("name resolution")
                || err_str.contains("timed out")
                || err_str.contains("Connection refused")
                || err_str.contains("No route to host")
                || err_str.contains("Network is unreachable");
            if is_local {
                ConnectionResult::ConnectFailed { backoff: false }
            } else {
                ConnectionResult::ConnectFailed { backoff: true }
            }
        }
    }
}

/// Writer-task queue depth. Small on purpose: the big backlog lives in the
/// audio router's SendQueue (send_queue.rs) where it survives reconnects;
/// anything queued here or in the ws_cmd channel is lost when the
/// connection dies.
const WRITER_QUEUE_SIZE: usize = 8;

async fn do_connection(
    api_key: &str,
    model: &str,
    delay: &str,
    ws_cmd_rx: &mut mpsc::Receiver<WsCommand>,
    audio_event_tx: &mpsc::Sender<AudioEvent>,
    transcript_tx: &mpsc::Sender<TranscriptEvent>,
    metrics_tx: &mpsc::Sender<MetricsEvent>,
    cancel: &CancellationToken,
) -> anyhow::Result<ConnectionResult> {
    use tokio_tungstenite::tungstenite::http::Request;

    let request = Request::builder()
        .uri(WS_URL)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Host", "api.openai.com")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())?;

    let (ws_stream, _response) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| anyhow::anyhow!("WebSocket connection timed out after 10s"))??;
    let (mut ws_write, mut ws_read) = ws_stream.split();
    // Committed items awaiting completion: item_id -> (created_at, local-VAD
    // utterance duration). Populated at `conversation.item.created`, which the
    // server emits synchronously in commit order — so pairing the duration
    // FIFO there is safe. Do NOT pair at `.completed`: completions can arrive
    // arbitrarily late and out of order (observed 299s late on 2026-07-14,
    // which desynced the old positional pairing and mis-tagged a real 30s
    // utterance as 160ms → dropped as a "speed hallucination").
    let mut pending_items: HashMap<String, (std::time::Instant, Option<u64>)> = HashMap::new();
    // FIFO of utterance durations measured by local VAD, waiting to be
    // attached to the next `conversation.item.created` from the server. The
    // server assigns item_ids only after we commit, so we can't pre-associate
    // duration with item_id at commit time.
    let mut pending_durations: std::collections::VecDeque<Option<u64>> =
        std::collections::VecDeque::new();

    // Drain stale commands from the previous connection. Their audio is
    // gone from the (new) server-side buffer anyway; log the breakdown —
    // a drained Commit means an utterance boundary was lost to the
    // reconnect (its audio merges into the next commit).
    let mut drained_audio = 0u64;
    let mut drained_commits = 0u64;
    while let Ok(cmd) = ws_cmd_rx.try_recv() {
        match cmd {
            WsCommand::SendAudio { .. } => drained_audio += 1,
            WsCommand::Commit { .. } => drained_commits += 1,
        }
    }
    if drained_audio > 0 || drained_commits > 0 {
        info!(
            "Drained {drained_audio} stale audio chunks + {drained_commits} commits from previous connection"
        );
    }

    metrics_tx.send(MetricsEvent::ConnectionSuccess).await.ok();
    // Notify audio_buffer that API timestamps reset with the new session
    audio_event_tx.send(AudioEvent::SessionReset).await.ok();
    // Notify transcript task to clear ordering/completion state
    transcript_tx.send(TranscriptEvent::SessionReset).await.ok();
    println!(
        "[INFO] WebSocket connection established (transcription mode, model: {model})"
    );

    // Send session config (GA shape: type:"session.update", session.type:"transcription",
    // audio.input.transcription.model). Whisper rejects turn_detection so the
    // client must endpoint with local VAD + manual commits.
    let session_update = SessionUpdate {
        r#type: "session.update",
        session: SessionConfig {
            r#type: "transcription",
            audio: AudioConfig {
                input: AudioInput {
                    transcription: InputAudioTranscription {
                        model: model.to_string(),
                        delay: Some(delay.to_string()),
                    },
                },
            },
        },
    };
    ws_write
        .send(Message::Text(serde_json::to_string(&session_update)?.into()))
        .await?;
    info!("Transcription session config sent (model: {model}, delay: {delay})");

    // ── Writer task ──────────────────────────────────────────────────
    // All further socket writes happen off-loop so a stalled TCP send
    // (congested uplink) can't freeze the reader loop: pings keep being
    // scheduled, server events keep flowing, and — critically — the pong
    // timeout below keeps being checked, which is what detects a wedged
    // socket and forces a reconnect. Before this split, a blocked inline
    // write froze the whole select loop for the duration of the stall
    // (26.5s observed on 2026-07-18) with the timeout never firing.
    let (data_tx, mut data_rx) = mpsc::channel::<Message>(WRITER_QUEUE_SIZE);
    // Dedicated ping lane, polled first, so keepalives never queue behind
    // buffered audio frames.
    let (ping_tx, mut ping_rx) = mpsc::channel::<Message>(2);
    let mut writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                msg = ping_rx.recv() => {
                    match msg {
                        Some(msg) => {
                            if let Err(e) = ws_write.send(msg).await {
                                warn!("WebSocket ping write failed: {e}");
                                return;
                            }
                        }
                        // Reader loop ended — flush remaining frames, then
                        // close the socket gracefully.
                        None => {
                            while let Some(msg) = data_rx.recv().await {
                                if ws_write.send(msg).await.is_err() {
                                    return;
                                }
                            }
                            let _ = ws_write.close().await;
                            return;
                        }
                    }
                }
                msg = data_rx.recv() => {
                    match msg {
                        Some(msg) => {
                            if let Err(e) = ws_write.send(msg).await {
                                warn!("WebSocket write failed: {e}");
                                return;
                            }
                        }
                        // None = lane drained AND closed (teardown): nothing
                        // left to flush, close the socket gracefully.
                        None => {
                            let _ = ws_write.close().await;
                            return;
                        }
                    }
                }
            }
        }
    });

    // Ping keepalive with silence timeout. `last_inbound` (any received
    // message) is the liveness signal — NOT pongs alone: this server pongs
    // lazily during active streaming, so pong-age is not a health measure.
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(5));
    ping_interval.tick().await;
    let mut last_ping: Option<std::time::Instant> = None;
    let mut last_pong = std::time::Instant::now();
    let mut last_inbound = std::time::Instant::now();
    let ping_timeout = std::time::Duration::from_secs(15);

    // Check pong timeout every 5s
    let mut pong_check = tokio::time::interval(std::time::Duration::from_secs(5));
    pong_check.tick().await;

    let mut result = ConnectionResult::Reconnect;

    // One-slot forwarding: hold at most one decoded command while waiting
    // for writer-queue space, so a stalled writer exerts backpressure into
    // the ws_cmd channel (and from there into the router's SendQueue)
    // instead of freezing this loop.
    let mut forward_slot: Option<Message> = None;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("WebSocket cancelled");
                result = ConnectionResult::Done;
                break;
            }

            _ = ping_interval.tick() => {
                // try_send: a full ping lane means the writer is wedged
                // mid-write; the enqueue-time stamp of the still-unsent
                // ping will trip the pong timeout below. Stamping at
                // enqueue (not at socket write) is deliberate — it makes
                // the timeout measure end-to-end write health.
                //
                // Only re-stamp once the previous ping was answered: an
                // outstanding ping's stamp must survive so pong LOSS
                // (dead inbound with healthy writes) also trips the
                // timeout — otherwise 5s refreshes keep elapsed() < 10s
                // forever on a half-open connection.
                if ping_tx.try_send(Message::Ping(vec![].into())).is_ok()
                    && last_ping.is_none_or(|t| last_pong >= t)
                {
                    last_ping = Some(std::time::Instant::now());
                }
            }

            _ = pong_check.tick() => {
                if connection_silent(last_ping, last_inbound, ping_timeout) {
                    warn!(
                        "Connection silent: ping unanswered for {}s with no other inbound traffic — reconnecting",
                        ping_timeout.as_secs(),
                    );
                    break;
                }
            }

            // Pull the next router command once the previous one has been
            // handed to the writer.
            cmd = ws_cmd_rx.recv(), if forward_slot.is_none() => {
                match cmd {
                    Some(cmd) => {
                        forward_slot = Some(command_to_message(cmd, &mut pending_durations)?);
                    }
                    None => {
                        info!("Command channel closed");
                        result = ConnectionResult::Done;
                        break;
                    }
                }
            }

            // Hand the held command to the writer once it has queue space.
            permit = data_tx.reserve(), if forward_slot.is_some() => {
                match permit {
                    Ok(permit) => permit.send(forward_slot.take().expect("guarded by is_some")),
                    Err(_) => {
                        warn!("Writer task gone — reconnecting");
                        break;
                    }
                }
            }

            // Read server events
            msg = ws_read.next() => {
                if let Some(Ok(_)) = &msg {
                    last_inbound = std::time::Instant::now();
                }
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Raw event dump for protocol debugging. Set DUMP_WS_EVENTS=1.
                        if std::env::var("DUMP_WS_EVENTS").is_ok() {
                            let truncated: String = text.chars().take(2000).collect();
                            info!("WS_RAW: {truncated}");
                        }
                        let event_result = handle_server_event(
                            &text,
                            transcript_tx,
                            metrics_tx,
                            &mut pending_items,
                            &mut pending_durations,
                        ).await;

                        match event_result {
                            EventAction::Continue => {}
                            EventAction::SessionCreated => {
                                info!("Session created");
                                println!("[INFO] Session created successfully");
                            }
                            EventAction::SessionUpdated => {
                                info!("Session configured — ready for transcription");
                                println!("[INFO] Session configuration updated");
                                println!("[INFO] Local VAD active — utterances committed on detected silence");
                                println!("[INFO] Speak into your microphone. Transcription will be typed and logged.");
                                println!("[INFO] The session will stay active indefinitely - silence is OK!");
                                println!("[INFO] Press Ctrl+C to stop.\n");
                            }
                            EventAction::SessionExpired => {
                                metrics_tx.send(MetricsEvent::SessionExpiration).await.ok();
                                break; // Reconnect
                            }
                            EventAction::Error { code, message } => {
                                error!("API error: {code}: {message}");
                                metrics_tx.send(MetricsEvent::ApiError).await.ok();
                            }
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        if let Some(ping_time) = last_ping {
                            // Only the first pong after a stamp pairs with
                            // that ping; and lazy pongs (>5s — the gateway
                            // deprioritizes them under streaming load)
                            // measure server scheduling, not network. Skip
                            // both so the EMA stays a network signal.
                            let rtt_ms = ping_time.elapsed().as_millis() as u64;
                            if last_pong < ping_time && rtt_ms <= 5000 {
                                debug!("Ping RTT: {rtt_ms}ms");
                                metrics_tx.send(MetricsEvent::PingRtt { millis: rtt_ms }).await.ok();
                            }
                        }
                        last_pong = std::time::Instant::now();
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let code = frame.as_ref().map(|f| f.code);
                        info!("WebSocket closed: {code:?}");
                        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
                        if code == Some(CloseCode::Normal) {
                            result = ConnectionResult::Done;
                        }
                        break;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket read error: {e}");
                        break;
                    }
                    None => {
                        info!("WebSocket stream ended");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    // Let the writer flush queued frames and send a close frame; abort if
    // the socket is too wedged to finish in time.
    drop(data_tx);
    drop(ping_tx);
    if tokio::time::timeout(std::time::Duration::from_secs(3), &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }

    Ok(result)
}

/// Convert a router command into its wire message. For commits, records the
/// local-VAD utterance duration in the FIFO that `conversation.item.created`
/// pops — this MUST happen exactly once per commit forwarded, in forward
/// order, or every later item inherits the wrong duration (the desync class
/// from INCIDENTS.md 2026-07-14).
fn command_to_message(
    cmd: WsCommand,
    pending_durations: &mut std::collections::VecDeque<Option<u64>>,
) -> anyhow::Result<Message> {
    match cmd {
        WsCommand::SendAudio { audio_b64 } => {
            let msg = serde_json::to_string(&AudioBufferAppend {
                r#type: "input_audio_buffer.append",
                audio: audio_b64,
            })?;
            Ok(Message::Text(msg.into()))
        }
        WsCommand::Commit { committed_audio_ms } => {
            pending_durations.push_back(committed_audio_ms);
            Ok(Message::Text(
                r#"{"type":"input_audio_buffer.commit"}"#.into(),
            ))
        }
    }
}

/// Reconnect only when the connection is truly silent: an outstanding ping
/// has gone unanswered for `timeout` AND nothing else has arrived since it
/// was enqueued. Pong-only liveness is WRONG for this server: OpenAI's
/// gateway answers pings lazily during active streaming (pongs observed
/// 5-20s behind while transcriptions round-trip in ~500ms), so a healthy
/// connection under dictation routinely has >10s-old unanswered pings
/// (2026-07-18 evening regression — see INCIDENTS.md).
fn connection_silent(
    last_ping: Option<std::time::Instant>,
    last_inbound: std::time::Instant,
    timeout: std::time::Duration,
) -> bool {
    last_ping.is_some_and(|ping| ping.elapsed() > timeout && last_inbound < ping)
}

/// Action to take after processing a server event.
enum EventAction {
    Continue,
    SessionCreated,
    SessionUpdated,
    SessionExpired,
    Error { code: String, message: String },
}

async fn handle_server_event(
    text: &str,
    transcript_tx: &mpsc::Sender<TranscriptEvent>,
    metrics_tx: &mpsc::Sender<MetricsEvent>,
    pending_items: &mut HashMap<String, (std::time::Instant, Option<u64>)>,
    pending_durations: &mut std::collections::VecDeque<Option<u64>>,
) -> EventAction {
    let event: ServerEvent = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to parse server event: {e}, raw: {text}");
            return EventAction::Continue;
        }
    };

    debug!(event_type = %event.r#type, "Server event");

    match event.r#type.as_str() {
        "session.created" => EventAction::SessionCreated,
        "session.updated" => EventAction::SessionUpdated,

        // GA renamed "conversation.item.created" → "conversation.item.added"
        // (+ separate "conversation.item.done"). Accept both so old sessions
        // and any straggler events still register.
        "conversation.item.created" | "conversation.item.added" => {
            match event.item.as_ref().and_then(|i| i.id.as_ref()) {
                Some(id) if !pending_items.contains_key(id) => {
                    let duration_ms = pending_durations.pop_front().flatten();
                    pending_items.insert(id.clone(), (std::time::Instant::now(), duration_ms));
                    transcript_tx
                        .send(TranscriptEvent::ItemCreated {
                            item_id: id.clone(),
                        })
                        .await
                        .ok();
                }
                // Seeing both .created and .added for one item — a second
                // pop here would desync the duration FIFO.
                Some(_) => {}
                None => {
                    // A creation event with no item id can never be matched
                    // by a .completed — discard its queued duration so later
                    // items don't inherit the wrong one (same desync class as
                    // INCIDENTS.md 2026-07-14).
                    let stale = pending_durations.pop_front().flatten();
                    warn!(
                        "conversation.item event without item id — discarding queued duration {stale:?}"
                    );
                }
            }
            EventAction::Continue
        }

        "conversation.item.input_audio_transcription.completed"
        | "response.audio_transcript.done" => {
            metrics_tx
                .send(MetricsEvent::RealtimeTranscription)
                .await
                .ok();
            let transcript = event.transcript.clone().unwrap_or_default();
            if event.item_id.is_none() && !transcript.is_empty() {
                info!("Realtime transcription: {transcript} [item_id=unknown]");
            }
            if let Some(item_id) = &event.item_id {
                // Duration was paired with this item_id at item.created, so a
                // late or out-of-order completion still gets its own duration.
                let duration_ms = match pending_items.remove(item_id) {
                    Some((created, duration_ms)) => {
                        let rtt_ms = created.elapsed().as_millis() as u64;
                        info!("Transcription RTT: {rtt_ms}ms for {item_id}");
                        metrics_tx.send(MetricsEvent::TranscriptionRtt {
                            millis: rtt_ms,
                        }).await.ok();
                        duration_ms
                    }
                    None => None,
                };
                if !transcript.is_empty() {
                    info!("Realtime transcription: {transcript} [item_id={item_id}]");
                }
                transcript_tx
                    .send(TranscriptEvent::RealtimeCompleted {
                        item_id: item_id.clone(),
                        transcript,
                        duration_ms,
                    })
                    .await
                    .ok();
            }
            EventAction::Continue
        }

        "conversation.item.input_audio_transcription.delta"
        | "response.audio_transcript.delta" => {
            if let Some(delta) = &event.delta {
                if !delta.is_empty() {
                    transcript_tx
                        .send(TranscriptEvent::RealtimeDelta {
                            delta: delta.clone(),
                        })
                        .await
                        .ok();
                }
            }
            EventAction::Continue
        }

        "error" => {
            if let Some(err) = &event.error {
                if err.code == "session_expired" {
                    warn!("Session expired: {}", err.message);
                    return EventAction::SessionExpired;
                }
                // A rejected commit never produces item.created, which would
                // leave its duration stuck at the FIFO head and shift every
                // later item's duration (the desync class from INCIDENTS.md
                // 2026-07-14). Errors don't reconnect (see EventAction::Error
                // handling), so discard the orphaned duration here.
                if err.code == "input_audio_buffer_commit_empty" {
                    let stale = pending_durations.pop_front().flatten();
                    warn!(
                        "Commit rejected ({}) — discarding queued duration {stale:?} to keep pairing aligned",
                        err.code
                    );
                }
                return EventAction::Error {
                    code: err.code.clone(),
                    message: err.message.clone(),
                };
            }
            EventAction::Continue
        }

        _ => {
            debug!("Unhandled event type: {}", event.r#type);
            EventAction::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn send(
        json: &str,
        transcript_tx: &mpsc::Sender<TranscriptEvent>,
        metrics_tx: &mpsc::Sender<MetricsEvent>,
        pending_items: &mut HashMap<String, (std::time::Instant, Option<u64>)>,
        pending_durations: &mut std::collections::VecDeque<Option<u64>>,
    ) {
        handle_server_event(json, transcript_tx, metrics_tx, pending_items, pending_durations)
            .await;
    }

    fn created(id: &str) -> String {
        format!(r#"{{"type":"conversation.item.created","item":{{"id":"{id}"}}}}"#)
    }

    fn completed(id: &str, text: &str) -> String {
        format!(
            r#"{{"type":"conversation.item.input_audio_transcription.completed","item_id":"{id}","transcript":"{text}"}}"#
        )
    }

    fn drain_completions(
        rx: &mut mpsc::Receiver<TranscriptEvent>,
    ) -> Vec<(String, Option<u64>)> {
        let mut got = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let TranscriptEvent::RealtimeCompleted {
                item_id,
                duration_ms,
                ..
            } = ev
            {
                got.push((item_id, duration_ms));
            }
        }
        got
    }

    /// Replay of the 2026-07-14 incident (INCIDENTS.md: "Real speech dropped
    /// when one completion arrives late"). One stalled completion must not
    /// shift other items' durations. With the old positional FIFO, item B
    /// ("Um") got A's 19934ms, C (a real 95-word utterance) got B's 160ms —
    /// tripping the >=10 wps speed-hallucination filter and dropping it.
    #[tokio::test]
    async fn late_completion_gets_its_own_duration() {
        let (ttx, mut trx) = mpsc::channel(64);
        let (mtx, mut _mrx) = mpsc::channel(256);
        let mut items = HashMap::new();
        let mut durs = std::collections::VecDeque::new();

        // Commit A (~20s utterance); its completion will stall for minutes.
        durs.push_back(Some(19934));
        send(&created("item_A"), &ttx, &mtx, &mut items, &mut durs).await;
        // Commit B ("Um", 160ms of speech).
        durs.push_back(Some(160));
        send(&created("item_B"), &ttx, &mtx, &mut items, &mut durs).await;
        // B completes while A is still stalled.
        send(&completed("item_B", "Um"), &ttx, &mtx, &mut items, &mut durs).await;
        // Commit C (30s max-buffer utterance); completes while A still stalled.
        durs.push_back(Some(30000));
        send(&created("item_C"), &ttx, &mtx, &mut items, &mut durs).await;
        send(&completed("item_C", "ninety five real words"), &ttx, &mtx, &mut items, &mut durs).await;
        // A's completion finally arrives, out of order.
        send(&completed("item_A", "blast radius"), &ttx, &mtx, &mut items, &mut durs).await;

        assert_eq!(
            drain_completions(&mut trx),
            vec![
                ("item_B".to_string(), Some(160)),   // old code: 19934 (A's)
                ("item_C".to_string(), Some(30000)), // old code: 160 -> dropped
                ("item_A".to_string(), Some(19934)), // old code: 30000
            ]
        );
        assert!(items.is_empty(), "all items completed");
        assert!(durs.is_empty(), "all durations claimed");
    }

    /// A duplicate .added after .created for the same item must not pop a
    /// second duration (that would re-desync the pairing).
    #[tokio::test]
    async fn duplicate_creation_event_does_not_double_pop() {
        let (ttx, mut trx) = mpsc::channel(64);
        let (mtx, mut _mrx) = mpsc::channel(256);
        let mut items = HashMap::new();
        let mut durs = std::collections::VecDeque::new();

        durs.push_back(Some(1000));
        durs.push_back(Some(2000));
        send(&created("item_A"), &ttx, &mtx, &mut items, &mut durs).await;
        // GA server may emit .added alongside .created for the same item.
        let added = r#"{"type":"conversation.item.added","item":{"id":"item_A"}}"#;
        send(added, &ttx, &mtx, &mut items, &mut durs).await;
        send(&created("item_B"), &ttx, &mtx, &mut items, &mut durs).await;
        send(&completed("item_A", "a"), &ttx, &mtx, &mut items, &mut durs).await;
        send(&completed("item_B", "b"), &ttx, &mtx, &mut items, &mut durs).await;

        assert_eq!(
            drain_completions(&mut trx),
            vec![
                ("item_A".to_string(), Some(1000)),
                ("item_B".to_string(), Some(2000)),
            ]
        );
    }

    /// Replay of the 2026-07-18 evening regression: a transcription
    /// completed 287ms before the pong timeout killed the connection.
    /// Any inbound traffic since the outstanding ping proves liveness —
    /// lazy pongs alone must never trigger a reconnect.
    #[test]
    fn inbound_traffic_vetoes_pong_timeout() {
        let now = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(15);

        // Ping enqueued 20s ago, never answered — but a completion arrived
        // 300ms ago. Alive: do not reconnect.
        let stale_ping = Some(now - std::time::Duration::from_secs(20));
        let fresh_inbound = now - std::time::Duration::from_millis(300);
        assert!(!connection_silent(stale_ping, fresh_inbound, timeout));

        // Same stale ping and NOTHING received since it was enqueued —
        // truly silent: reconnect. (The original incident's wedge.)
        let silent_since = now - std::time::Duration::from_secs(25);
        assert!(connection_silent(stale_ping, silent_since, timeout));

        // Outstanding ping is recent — not silent yet, even with old inbound.
        let fresh_ping = Some(now - std::time::Duration::from_secs(5));
        assert!(!connection_silent(fresh_ping, silent_since, timeout));

        // No ping outstanding — nothing to time out against.
        assert!(!connection_silent(None, silent_since, timeout));
    }

    /// Forwarding a Commit must record its duration exactly once and emit
    /// the commit frame; forwarding audio must not touch the duration FIFO.
    /// This pairing invariant is what keeps late completions correctly
    /// attributed (INCIDENTS.md 2026-07-14).
    #[tokio::test]
    async fn command_forwarding_preserves_duration_pairing() {
        let mut durs = std::collections::VecDeque::new();

        let msg = command_to_message(
            WsCommand::SendAudio { audio_b64: "QUJD".into() },
            &mut durs,
        )
        .unwrap();
        assert!(durs.is_empty(), "audio must not touch the duration FIFO");
        let Message::Text(text) = msg else { panic!("expected text frame") };
        assert!(text.contains("input_audio_buffer.append"));
        assert!(text.contains("QUJD"));

        let msg = command_to_message(
            WsCommand::Commit { committed_audio_ms: Some(1234) },
            &mut durs,
        )
        .unwrap();
        assert_eq!(durs, [Some(1234)], "exactly one duration per commit");
        let Message::Text(text) = msg else { panic!("expected text frame") };
        assert!(text.contains("input_audio_buffer.commit"));
    }

    /// A server-rejected commit produces no item.created; its orphaned
    /// duration must be discarded on the error event, or every later item
    /// inherits the wrong duration for the rest of the session.
    #[tokio::test]
    async fn rejected_commit_discards_orphaned_duration() {
        let (ttx, mut trx) = mpsc::channel(64);
        let (mtx, mut _mrx) = mpsc::channel(256);
        let mut items = HashMap::new();
        let mut durs = std::collections::VecDeque::new();

        // Commit A is rejected server-side (no item.created will follow).
        durs.push_back(Some(500));
        let err = r#"{"type":"error","error":{"code":"input_audio_buffer_commit_empty","message":"buffer too small"}}"#;
        send(err, &ttx, &mtx, &mut items, &mut durs).await;
        // Commit B succeeds and must get its own duration, not A's.
        durs.push_back(Some(7000));
        send(&created("item_B"), &ttx, &mtx, &mut items, &mut durs).await;
        send(&completed("item_B", "b"), &ttx, &mtx, &mut items, &mut durs).await;

        assert_eq!(
            drain_completions(&mut trx),
            vec![("item_B".to_string(), Some(7000))]
        );
    }
}
