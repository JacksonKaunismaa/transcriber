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
    input_audio_transcription: InputAudioTranscription,
}

#[derive(Serialize)]
struct InputAudioTranscription {
    model: String,
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
    audio_start_ms: Option<u64>,
    #[serde(default)]
    audio_end_ms: Option<u64>,
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
    ws_cmd_rx: &mut mpsc::Receiver<WsCommand>,
    audio_event_tx: &mpsc::Sender<AudioEvent>,
    transcript_tx: &mpsc::Sender<TranscriptEvent>,
    metrics_tx: &mpsc::Sender<MetricsEvent>,
    cancel: &CancellationToken,
) -> ConnectionResult {
    match do_connection(
        api_key,
        model,
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

async fn do_connection(
    api_key: &str,
    model: &str,
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
        .header("OpenAI-Beta", "realtime=v1")
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

    // Drain stale audio commands from previous connection
    let mut drained = 0;
    while ws_cmd_rx.try_recv().is_ok() {
        drained += 1;
    }
    if drained > 0 {
        info!("Drained {drained} stale audio commands from previous connection");
    }

    metrics_tx.send(MetricsEvent::ConnectionSuccess).await.ok();
    // Notify audio_buffer that API timestamps reset with the new session
    audio_event_tx.send(AudioEvent::SessionReset).await.ok();
    // Notify transcript task to clear ordering/completion state
    transcript_tx.send(TranscriptEvent::SessionReset).await.ok();
    println!(
        "[INFO] WebSocket connection established (transcription mode, model: {model})"
    );

    // Send session config
    let session_update = SessionUpdate {
        r#type: "transcription_session.update",
        session: SessionConfig {
            input_audio_transcription: InputAudioTranscription {
                model: model.to_string(),
            },
        },
    };
    ws_write
        .send(Message::Text(serde_json::to_string(&session_update)?.into()))
        .await?;
    info!("Transcription session config sent (model: {model})");

    // Ping keepalive with pong timeout (matches Python: ping_interval=20, ping_timeout=10)
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(20));
    ping_interval.tick().await;
    let mut last_ping: Option<std::time::Instant> = None;
    let mut last_pong = std::time::Instant::now();
    let ping_timeout = std::time::Duration::from_secs(10);

    // Check pong timeout every 5s (more frequently than ping_timeout to catch it promptly)
    let mut pong_check = tokio::time::interval(std::time::Duration::from_secs(5));
    pong_check.tick().await;

    let mut result = ConnectionResult::Reconnect;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("WebSocket cancelled");
                let _ = ws_write.close().await;
                result = ConnectionResult::Done;
                break;
            }

            _ = ping_interval.tick() => {
                if let Err(e) = ws_write.send(Message::Ping(vec![].into())).await {
                    warn!("Ping failed: {e}");
                    break;
                }
                last_ping = Some(std::time::Instant::now());
            }

            _ = pong_check.tick() => {
                if let Some(ping_time) = last_ping {
                    if ping_time.elapsed() > ping_timeout && last_pong < ping_time {
                        warn!("Pong timeout (no pong within {}s of last ping)", ping_timeout.as_secs());
                        break;
                    }
                }
            }

            // Forward audio from Audio Router → WebSocket
            cmd = ws_cmd_rx.recv() => {
                match cmd {
                    Some(WsCommand::SendAudio { audio_b64 }) => {
                        let msg = serde_json::to_string(&AudioBufferAppend {
                            r#type: "input_audio_buffer.append",
                            audio: audio_b64,
                        })?;
                        if let Err(e) = ws_write.send(Message::Text(msg.into())).await {
                            warn!("Failed to send audio: {e}");
                            break;
                        }
                    }
                    Some(WsCommand::SendSessionConfig { model }) => {
                        let msg = serde_json::to_string(&SessionUpdate {
                            r#type: "transcription_session.update",
                            session: SessionConfig {
                                input_audio_transcription: InputAudioTranscription { model },
                            },
                        })?;
                        ws_write.send(Message::Text(msg.into())).await?;
                    }
                    None => {
                        info!("Command channel closed");
                        result = ConnectionResult::Done;
                        break;
                    }
                }
            }

            // Read server events
            msg = ws_read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let event_result = handle_server_event(
                            &text,
                            audio_event_tx,
                            transcript_tx,
                            metrics_tx,
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
                                println!("[INFO] Fallback transcription enabled (2.5s timeout)");
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

    Ok(result)
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
    audio_event_tx: &mpsc::Sender<AudioEvent>,
    transcript_tx: &mpsc::Sender<TranscriptEvent>,
    metrics_tx: &mpsc::Sender<MetricsEvent>,
) -> EventAction {
    let event: ServerEvent = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to parse server event: {e}");
            return EventAction::Continue;
        }
    };

    debug!(event_type = %event.r#type, "Server event");

    match event.r#type.as_str() {
        "session.created" => EventAction::SessionCreated,
        "session.updated" => EventAction::SessionUpdated,

        "conversation.item.created" => {
            if let Some(item) = &event.item {
                if let Some(id) = &item.id {
                    transcript_tx
                        .send(TranscriptEvent::ItemCreated {
                            item_id: id.clone(),
                        })
                        .await
                        .ok();
                }
            }
            EventAction::Continue
        }

        "input_audio_buffer.speech_started" => {
            if let Some(item_id) = &event.item_id {
                audio_event_tx
                    .send(AudioEvent::SpeechStarted {
                        item_id: item_id.clone(),
                        audio_start_ms: event.audio_start_ms.unwrap_or(0),
                    })
                    .await
                    .ok();
            }
            EventAction::Continue
        }

        "input_audio_buffer.speech_stopped" => {
            if let Some(item_id) = &event.item_id {
                audio_event_tx
                    .send(AudioEvent::SpeechStopped {
                        item_id: item_id.clone(),
                        audio_end_ms: event.audio_end_ms.unwrap_or(0),
                    })
                    .await
                    .ok();
            }
            EventAction::Continue
        }

        "conversation.item.input_audio_transcription.completed"
        | "response.audio_transcript.done" => {
            metrics_tx
                .send(MetricsEvent::RealtimeTranscription)
                .await
                .ok();
            if let Some(transcript) = &event.transcript {
                if !transcript.is_empty() {
                    info!("Realtime transcription: {transcript}");
                    if let Some(item_id) = &event.item_id {
                        // Cancel pending fallback timer in audio_buffer
                        audio_event_tx
                            .send(AudioEvent::ItemCompleted {
                                item_id: item_id.clone(),
                            })
                            .await
                            .ok();
                        transcript_tx
                            .send(TranscriptEvent::RealtimeCompleted {
                                item_id: item_id.clone(),
                                transcript: transcript.clone(),
                            })
                            .await
                            .ok();
                    }
                }
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
