use std::collections::VecDeque;

use base64::Engine;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use voice_activity_detector::VoiceActivityDetector;

use crate::messages::{AudioChunk, AudioEvent, MetricsEvent, WsCommand};

const SAMPLE_RATE: u32 = 24000;
const BYTES_PER_SAMPLE: u32 = 2;

/// Silero v5 expects 512 samples at 16kHz per inference (32ms).
const SILERO_FRAME_16K: usize = 512;

/// 32ms at the capture rate (24kHz) — what we need to accumulate before
/// resampling to a single 512-sample silero frame at 16kHz.
const SILERO_FRAME_24K: usize = (SAMPLE_RATE as usize * 32) / 1000; // 768

/// Start threshold: a frame with prob above this counts toward triggering
/// `SpeechStarted`. Lowered from 0.5 → 0.4 on 2026-05-22 after misses with
/// peak ~0.48-0.49. See INCIDENTS.md → "Soft speech missed by Silero VAD".
const SPEECH_START_THRESHOLD: f32 = 0.4;

/// Continue threshold (hysteresis): once `in_speech`, the silence counter
/// only advances on frames with prob *below* this. Frames between
/// START and CONTINUE are "neutral" and hold the current state — preventing
/// brief mid-utterance dips into the 0.2-0.4 range from prematurely
/// committing. This is the asymmetric scheme silero's own utilities use
/// (parameter name `neg_threshold`), recommended at 0.15 alongside a 0.4
/// start threshold for quiet-audio environments.
const SPEECH_CONTINUE_THRESHOLD: f32 = 0.15;

/// Speech start: 3 consecutive 32ms speech-verdicts = 96ms — quick enough to
/// catch short utterances.
const SPEECH_START_FRAMES: u32 = 3;

/// Speech stop: 30 consecutive 32ms silence-verdicts = 960ms — slightly above
/// the typical inter-sentence pause to tolerate brief breaths/breaths inside
/// long words.
const SILENCE_STOP_FRAMES: u32 = 30;

/// Max time between commits — bounds the blast radius if VAD somehow misses
/// an end-of-speech (the next utterance gets pre-pended otherwise).
const MAX_BUFFER_SECONDS: u64 = 30;
const MAX_BUFFER_BYTES: u64 =
    SAMPLE_RATE as u64 * BYTES_PER_SAMPLE as u64 * MAX_BUFFER_SECONDS;

/// Server-required minimum committed audio (it rejects sub-100ms commits with
/// `input_audio_buffer_commit_empty`). Use a small margin.
const MIN_COMMIT_MS: u64 = 150;
const MIN_COMMIT_BYTES: u64 =
    (SAMPLE_RATE as u64 * BYTES_PER_SAMPLE as u64 * MIN_COMMIT_MS) / 1000;

/// A chunk gap larger than this implies audio capture was paused (mute/unmute
/// via SIGUSR1). cpal normally delivers chunks every ~43ms (1024 frames /
/// 24kHz); 500ms is comfortably above the noise floor.
const PAUSE_GAP_THRESHOLD: std::time::Duration = std::time::Duration::from_millis(500);

/// Raw audio byte rate at the capture format (24kHz, 16-bit mono).
const BYTES_PER_SEC: u64 = SAMPLE_RATE as u64 * BYTES_PER_SAMPLE as u64;

/// Cap on audio buffered locally while the websocket can't keep up (network
/// backpressure / reconnect). Two minutes rides out long congestion stalls
/// (observed: 26.5s on 2026-07-18) with late-but-complete transcription;
/// past it the oldest entries are dropped and counted in metrics.
const MAX_SEND_BACKLOG_MS: u64 = 120_000;

/// How much pre-speech audio to retain so that the first word of an utterance
/// isn't clipped (silero needs ~3 frames = 96ms before declaring speech, so
/// the first ~100ms of any utterance happens BEFORE we know it's speech).
/// 1 second is generous; covers slow speech-starts and consonants/whispers.
const PREFIX_BUFFER_MS: u64 = 1000;
const PREFIX_BUFFER_BYTES: usize =
    (SAMPLE_RATE as usize * BYTES_PER_SAMPLE as usize * PREFIX_BUFFER_MS as usize) / 1000;

/// Rolling local buffer of recent audio, kept independent of VAD state. This
/// is the storage layer the whisper-1 REST fallback would slice from if
/// revived (see INCIDENTS.md). Keeping it filled now means that *if* the
/// post-deploy monitoring shows the realtime path drops commits, we can wire
/// up the REST fallback without retroactively losing audio.
///
/// 30s holds enough audio for any reasonable utterance (typical max is ~20s
/// for the MAX_BUFFER_SECONDS safety commit) plus margin.
const FALLBACK_BUFFER_SECONDS: u64 = 30;

struct VadState {
    /// Silero model instance. Manages LSTM state across `predict()` calls.
    vad: VoiceActivityDetector,
    in_speech: bool,
    consecutive_speech: u32,
    consecutive_silence: u32,
    /// Peak silero probability observed during current speech burst — diagnostic.
    peak_prob: f32,
    /// PCM bytes appended to the server buffer since last commit. We must not
    /// commit if < MIN_COMMIT_BYTES.
    bytes_since_commit: u64,
    /// 24kHz i16 samples awaiting a 768-sample silero frame.
    sample_buf: Vec<i16>,
    /// Last time a chunk arrived. Large gap implies mute → on resume we
    /// clear pending state. (No longer used for stale-audio flush since
    /// we now gate sending by VAD state, so the server buffer doesn't
    /// accumulate during pauses.)
    last_chunk_at: Option<std::time::Instant>,
    /// Rolling buffer of the most recent ~PREFIX_BUFFER_MS of audio bytes
    /// while not in_speech. On speech_started, flushed to server so that
    /// the lead-in (consonants, first word) isn't lost — silero needs ~96ms
    /// to declare in_speech and we want the audio from BEFORE that point too.
    prefix_buf: VecDeque<u8>,
    /// Rolling buffer of the most recent ~FALLBACK_BUFFER_SECONDS of audio,
    /// timestamped per chunk. Filled regardless of VAD state. Currently
    /// unused by any read path — kept ready in case the post-deploy
    /// monitoring (INCIDENTS.md) shows we need to revive the whisper-1
    /// REST fallback for commits that never get `.completed` back. Without
    /// this, reviving the fallback would require retroactively re-capturing
    /// audio that's already been dropped.
    fallback_buf: VecDeque<(std::time::Instant, Vec<u8>)>,

    // ── Diagnostics for "why isn't speech_stopped firing?" ───────────────
    /// When the current speech burst started — for elapsed time in heartbeats.
    speech_start_instant: Option<std::time::Instant>,
    /// Highest consecutive_silence reached during the current burst (resets
    /// whenever a speech frame interrupts). Logged at speech_stopped — tells
    /// us how close silero got to the SILENCE_STOP_FRAMES threshold without
    /// crossing it.
    peak_silence_in_burst: u32,
    /// Count of times consecutive_silence was ≥5 then got reset by a speech
    /// frame in the current burst. High = silero flickering through threshold.
    silence_streaks_broken: u32,
    /// Per-second heartbeat window stats — reset every time we emit one.
    last_heartbeat_at: Option<std::time::Instant>,
    hb_frames: u32,
    /// Frames with prob > SPEECH_START_THRESHOLD (0.4).
    hb_speech_frames: u32,
    /// Frames with prob > 0.2 (loose "almost speech" threshold) — helps
    /// distinguish "silero saw nothing" from "silero saw something marginal".
    hb_frames_above_low: u32,
    hb_min_prob: f32,
    hb_max_prob: f32,
    /// Max `consecutive_speech` reached during the window. If this is 2 but
    /// SPEECH_START_FRAMES is 3, silero was a single frame from declaring
    /// speech and we missed it — that's a "lower SPEECH_START_FRAMES" signal.
    hb_max_consecutive_speech: u32,
    /// Count of "flickers": consecutive_speech rose ≥1 then reset without
    /// triggering. High count = silero seeing intermittent speech-like signal.
    hb_speech_flickers: u32,

    /// Live-snapshot writer for the Quickshell debug overlay. Always on —
    /// writes are ~30 KB/s to a tmpfs path under $XDG_RUNTIME_DIR.
    debug: crate::vad_debug::VadDebugWriter,
}

impl VadState {
    fn new() -> Self {
        let vad = VoiceActivityDetector::builder()
            .sample_rate(16000)
            .chunk_size(SILERO_FRAME_16K)
            .build()
            .expect("Silero VAD initialisation failed — model bundled with the crate, should not fail");
        let debug = crate::vad_debug::VadDebugWriter::new(
            SPEECH_START_THRESHOLD,
            SPEECH_CONTINUE_THRESHOLD,
            SPEECH_START_FRAMES,
            SILENCE_STOP_FRAMES,
        );
        Self {
            vad,
            in_speech: false,
            consecutive_speech: 0,
            consecutive_silence: 0,
            peak_prob: 0.0,
            bytes_since_commit: 0,
            sample_buf: Vec::with_capacity(SILERO_FRAME_24K * 2),
            last_chunk_at: None,
            prefix_buf: VecDeque::with_capacity(PREFIX_BUFFER_BYTES),
            fallback_buf: VecDeque::new(),
            speech_start_instant: None,
            peak_silence_in_burst: 0,
            silence_streaks_broken: 0,
            last_heartbeat_at: None,
            hb_frames: 0,
            hb_speech_frames: 0,
            hb_frames_above_low: 0,
            hb_min_prob: 1.0,
            hb_max_prob: 0.0,
            hb_max_consecutive_speech: 0,
            hb_speech_flickers: 0,
            debug,
        }
    }

    fn reset_after_commit(&mut self) {
        self.bytes_since_commit = 0;
        self.in_speech = false;
        self.consecutive_speech = 0;
        self.consecutive_silence = 0;
        self.peak_prob = 0.0;
        self.speech_start_instant = None;
        self.peak_silence_in_burst = 0;
        self.silence_streaks_broken = 0;
        // Don't clear prefix_buf — we want to retain recent audio in case
        // a new utterance starts immediately after this commit. The rolling
        // cap will trim it naturally.
    }

    /// Append bytes to the rolling fallback buffer, evicting chunks older
    /// than FALLBACK_BUFFER_SECONDS. No read path consumes this yet — see
    /// the field doc.
    fn push_fallback(&mut self, now: std::time::Instant, bytes: Vec<u8>) {
        self.fallback_buf.push_back((now, bytes));
        let cutoff = now - std::time::Duration::from_secs(FALLBACK_BUFFER_SECONDS);
        while self
            .fallback_buf
            .front()
            .is_some_and(|(t, _)| *t < cutoff)
        {
            self.fallback_buf.pop_front();
        }
    }

    /// Append bytes to the rolling prefix buffer, evicting old samples to stay
    /// within PREFIX_BUFFER_BYTES.
    fn push_prefix(&mut self, bytes: &[u8]) {
        self.prefix_buf.extend(bytes.iter().copied());
        while self.prefix_buf.len() > PREFIX_BUFFER_BYTES {
            self.prefix_buf.pop_front();
        }
    }

    /// Drain the prefix buffer into a Vec, ready for sending.
    fn drain_prefix(&mut self) -> Vec<u8> {
        self.prefix_buf.drain(..).collect()
    }

    fn reset_heartbeat_window(&mut self) {
        self.hb_frames = 0;
        self.hb_speech_frames = 0;
        self.hb_frames_above_low = 0;
        self.hb_min_prob = 1.0;
        self.hb_max_prob = 0.0;
        self.hb_max_consecutive_speech = 0;
        self.hb_speech_flickers = 0;
    }
}

/// Linear-interpolation resample 768 samples @ 24kHz → 512 samples @ 16kHz.
/// Anti-aliasing is overkill for VAD signal; the human ear cares more than
/// silero does about this resampling quality.
fn resample_24k_to_16k(input: &[i16]) -> Vec<i16> {
    debug_assert_eq!(input.len(), SILERO_FRAME_24K);
    let mut out = Vec::with_capacity(SILERO_FRAME_16K);
    for j in 0..SILERO_FRAME_16K {
        let pos = j as f32 * 1.5; // 768 / 512 = 1.5
        let i = pos as usize;
        let frac = pos - i as f32;
        let a = input[i] as f32;
        let b = if i + 1 < input.len() {
            input[i + 1] as f32
        } else {
            a
        };
        out.push((a * (1.0 - frac) + b * frac) as i16);
    }
    out
}

enum VadTransition {
    SpeechStarted(f32),
    /// (peak_probability_during_burst)
    SpeechStopped(f32),
}

fn step_vad(state: &mut VadState, prob: f32) -> Option<VadTransition> {
    // Three-zone classification with hysteresis:
    //   prob > START      → speech frame (advances start counter, resets silence)
    //   prob < CONTINUE   → silence frame (advances silence counter, resets speech-streak)
    //   otherwise         → neutral (holds current state — does NOT advance silence,
    //                       but does reset an in-progress start streak so a single
    //                       neutral frame breaks pre-trigger speech accumulation)
    let is_speech_frame = prob > SPEECH_START_THRESHOLD;
    let is_silence_frame = prob < SPEECH_CONTINUE_THRESHOLD;
    let prev_consecutive_speech = state.consecutive_speech;

    // Heartbeat window stats (collected continuously, emitted periodically).
    // `hb_speech_frames` stays anchored to the START threshold so the metric
    // means the same thing regardless of in_speech state.
    state.hb_frames += 1;
    if is_speech_frame {
        state.hb_speech_frames += 1;
    }
    if prob > 0.2 {
        state.hb_frames_above_low += 1;
    }
    state.hb_min_prob = state.hb_min_prob.min(prob);
    state.hb_max_prob = state.hb_max_prob.max(prob);

    if is_speech_frame {
        // If we were accumulating silence, log when a substantial streak
        // gets broken — smoking gun for "silence wasn't quite silent
        // enough to count" failures.
        if state.in_speech && state.consecutive_silence >= 5 {
            state.silence_streaks_broken += 1;
            debug!(
                "VAD silence streak broken at {} frames by p={:.3} (target was {} frames)",
                state.consecutive_silence, prob, SILENCE_STOP_FRAMES
            );
        }
        state.consecutive_speech += 1;
        state.consecutive_silence = 0;
        if state.in_speech && prob > state.peak_prob {
            state.peak_prob = prob;
        }
        if state.consecutive_speech > state.hb_max_consecutive_speech {
            state.hb_max_consecutive_speech = state.consecutive_speech;
        }
    } else if is_silence_frame {
        if !state.in_speech && prev_consecutive_speech > 0 {
            state.hb_speech_flickers += 1;
        }
        state.consecutive_silence += 1;
        if state.in_speech && state.consecutive_silence > state.peak_silence_in_burst {
            state.peak_silence_in_burst = state.consecutive_silence;
        }
        state.consecutive_speech = 0;
    } else {
        // Neutral zone (CONTINUE ≤ prob ≤ START): hold state.
        // Don't increment silence — that's the hysteresis. Do break any
        // pre-trigger speech streak so starts require true consecutive
        // frames above the start threshold.
        if !state.in_speech && prev_consecutive_speech > 0 {
            state.hb_speech_flickers += 1;
        }
        state.consecutive_speech = 0;
    }

    if !state.in_speech && state.consecutive_speech >= SPEECH_START_FRAMES {
        state.in_speech = true;
        state.peak_prob = prob;
        state.speech_start_instant = Some(std::time::Instant::now());
        state.peak_silence_in_burst = 0;
        state.silence_streaks_broken = 0;
        return Some(VadTransition::SpeechStarted(prob));
    }
    if state.in_speech && state.consecutive_silence >= SILENCE_STOP_FRAMES {
        state.in_speech = false;
        let peak = state.peak_prob;
        state.peak_prob = 0.0;
        // Reset silero's LSTM state at the utterance boundary so the next
        // utterance starts with a clean hidden state. Matches Pipecat /
        // LiveKit's pattern and mitigates documented v4 silence-bias drift.
        state.vad.reset();
        return Some(VadTransition::SpeechStopped(peak));
    }
    None
}

/// Accumulate `samples` into the 24kHz buffer, run silero on every full
/// 768-sample (32ms) chunk, return transitions in arrival order.
fn process_chunk(state: &mut VadState, samples: &[i16]) -> Vec<VadTransition> {
    state.sample_buf.extend_from_slice(samples);

    let mut transitions = Vec::new();
    while state.sample_buf.len() >= SILERO_FRAME_24K {
        let drain: Vec<i16> = state.sample_buf.drain(..SILERO_FRAME_24K).collect();
        let chunk_16k = resample_24k_to_16k(&drain);
        let prob = state.vad.predict(chunk_16k);
        state.debug.record(prob);
        if let Some(t) = step_vad(state, prob) {
            transitions.push(t);
        }
        state.debug.maybe_flush(
            state.in_speech,
            state.consecutive_speech,
            state.consecutive_silence,
        );
    }

    // Per-second VAD heartbeat. Always fires as long as audio is flowing
    // (not muted). Idle heartbeats include "near-miss" stats so we can tell
    // why silero didn't declare speech for short utterances.
    let now = std::time::Instant::now();
    let interval = if state.in_speech {
        std::time::Duration::from_secs(1)
    } else {
        std::time::Duration::from_secs(2)
    };
    let due = match state.last_heartbeat_at {
        Some(t) => now.duration_since(t) >= interval,
        None => true,
    };
    if due && state.hb_frames > 0 {
        if state.in_speech {
            let elapsed_s = state
                .speech_start_instant
                .map(|t| now.duration_since(t).as_secs_f32())
                .unwrap_or(0.0);
            let silence_pct = 100 - (100 * state.hb_speech_frames / state.hb_frames);
            info!(
                "VAD heartbeat (in_speech for {:.1}s): sil_count={}/{} | window min_p={:.2} max_p={:.2} silence={}%",
                elapsed_s,
                state.consecutive_silence,
                SILENCE_STOP_FRAMES,
                state.hb_min_prob,
                state.hb_max_prob,
                silence_pct,
            );
        } else {
            // Only log if there's something worth looking at — either a
            // borderline reading or a near-miss flicker. Silencing the
            // log when truly nothing is happening avoids flooding the
            // JSONL during idle minutes.
            let interesting = state.hb_max_prob > 0.2
                || state.hb_speech_flickers > 0
                || state.hb_max_consecutive_speech > 0;
            if interesting {
                info!(
                    "VAD heartbeat (idle, {} frames): max_p={:.2} | frames>p0.2: {}/{}  frames>p0.5: {}/{} | max_consec_speech={}/{} | flickers={}",
                    state.hb_frames,
                    state.hb_max_prob,
                    state.hb_frames_above_low,
                    state.hb_frames,
                    state.hb_speech_frames,
                    state.hb_frames,
                    state.hb_max_consecutive_speech,
                    SPEECH_START_FRAMES,
                    state.hb_speech_flickers,
                );
            }
        }
        state.last_heartbeat_at = Some(now);
        state.reset_heartbeat_window();
    }

    transitions
}

/// Run the Audio Router task.
///
/// Receives raw audio from cpal, runs local Silero VAD, forwards audio to the
/// WebSocket, and triggers `input_audio_buffer.commit` when speech ends. The
/// realtime API responds with `.completed` events that flow through the
/// websocket task to the transcript task.
pub async fn run_audio_router_task(
    mut audio_rx: mpsc::Receiver<AudioChunk>,
    mut audio_event_rx: mpsc::Receiver<AudioEvent>,
    ws_cmd_tx: mpsc::Sender<WsCommand>,
    metrics_tx: mpsc::Sender<MetricsEvent>,
    cancel: CancellationToken,
) {
    let mut vad = VadState::new();
    info!("Silero VAD initialised");

    // Never-blocking sender: overflow buffers locally instead of stalling
    // this loop when the websocket backs up (see send_queue.rs).
    let mut send_q = crate::send_queue::SendQueue::new(
        (MAX_SEND_BACKLOG_MS / 1000) * BYTES_PER_SEC,
        BYTES_PER_SEC,
    );
    let mut last_reported_backlog_ms: u64 = 0;

    // Keepalive for the VAD debug overlay: re-flush the snapshot file every
    // 500ms while audio is paused so the widget doesn't think we've died.
    // Skip behaviour avoids burst-firing if a chunk hog stalls the task.
    // Doubles as the backlog flush/report tick when no chunks are flowing.
    let mut keepalive = tokio::time::interval(std::time::Duration::from_millis(500));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Audio router shutting down");
                break;
            }

            chunk = audio_rx.recv() => {
                let Some(chunk) = chunk else { break };

                // Drain any send backlog first — a recovered socket catches
                // up at chunk cadence, oldest audio first.
                send_q.try_flush(&ws_cmd_tx);
                vad.debug.set_backlog_ms(send_q.backlog_ms());

                // Detect capture pause (mute/unmute). On resume:
                //   - If we were mid-utterance with audio sent, commit it.
                //   - Drop any prefix buffer from before the gap (stale).
                //   - Clear the debug history so the sparkline doesn't render
                //     a huge timestamp gap as a stretched flat line.
                let now = std::time::Instant::now();
                if let Some(last) = vad.last_chunk_at {
                    if now.duration_since(last) > PAUSE_GAP_THRESHOLD {
                        if vad.bytes_since_commit >= MIN_COMMIT_BYTES {
                            let ms = (vad.bytes_since_commit * 1000)
                                / (SAMPLE_RATE as u64 * BYTES_PER_SAMPLE as u64);
                            info!(
                                "Capture resumed after gap ({:?}); committing {}ms of in-flight audio",
                                now.duration_since(last),
                                ms,
                            );
                            // No reliable speech duration here — mute/unmute
                            // mid-utterance; just send None.
                            send_q.send(&ws_cmd_tx, WsCommand::Commit { utterance_duration_ms: None });
                            vad.reset_after_commit();
                        }
                        vad.prefix_buf.clear();
                        vad.sample_buf.clear();
                        vad.debug.clear_history();
                    }
                }
                vad.last_chunk_at = Some(now);

                // Convert i16 samples to little-endian bytes for the API.
                let bytes: Vec<u8> = chunk.data.iter()
                    .flat_map(|s| s.to_le_bytes())
                    .collect();

                // Always push to the rolling fallback buffer (regardless of
                // VAD state). Currently no read path consumes this — see
                // VadState::fallback_buf for context.
                vad.push_fallback(now, bytes.clone());

                // Run VAD first so we know what state we're in for THIS chunk.
                // (Transitions emitted here are based on the chunk's samples.)
                let transitions = process_chunk(&mut vad, &chunk.data);

                let mut sent_via_prefix_flush = false;
                for transition in transitions {
                    match transition {
                        VadTransition::SpeechStarted(prob) => {
                            // Flush the pre-speech prefix buffer to the server
                            // so we capture the lead-in audio (consonants,
                            // first word) that silero needed to see *before*
                            // it could declare speech.
                            let prefix = vad.drain_prefix();
                            let prefix_ms = (prefix.len() as u64 * 1000)
                                / (SAMPLE_RATE as u64 * BYTES_PER_SAMPLE as u64);
                            info!(
                                "Silero VAD: speech started (p={:.3}); flushing {}ms prefix",
                                prob, prefix_ms
                            );
                            if !prefix.is_empty() {
                                let b64 = base64::engine::general_purpose::STANDARD.encode(&prefix);
                                send_q.send(&ws_cmd_tx, WsCommand::SendAudio { audio_b64: b64 });
                                vad.bytes_since_commit += prefix.len() as u64;
                            }
                            // Also send the current chunk's bytes (the chunk
                            // that triggered the transition).
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                            send_q.send(&ws_cmd_tx, WsCommand::SendAudio { audio_b64: b64 });
                            vad.bytes_since_commit += bytes.len() as u64;
                            sent_via_prefix_flush = true;
                        }
                        VadTransition::SpeechStopped(peak_prob) => {
                            // The current chunk likely contains trailing
                            // audio — send it before committing so whisper
                            // sees the full tail.
                            if !sent_via_prefix_flush {
                                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                                send_q.send(&ws_cmd_tx, WsCommand::SendAudio { audio_b64: b64 });
                                vad.bytes_since_commit += bytes.len() as u64;
                                sent_via_prefix_flush = true; // suppress further send below
                            }
                            if vad.bytes_since_commit >= MIN_COMMIT_BYTES {
                                let ms = (vad.bytes_since_commit * 1000)
                                    / (SAMPLE_RATE as u64 * BYTES_PER_SAMPLE as u64);
                                // Approximate speech duration: elapsed since
                                // we declared start minus the hysteresis wait
                                // (we declared start SPEECH_START_FRAMES late,
                                // declared stop SILENCE_STOP_FRAMES late after
                                // speech actually ended).
                                let hysteresis_ms = ((SILENCE_STOP_FRAMES - SPEECH_START_FRAMES) as u64) * 32;
                                let utterance_duration_ms = vad.speech_start_instant.map(|t| {
                                    let elapsed = std::time::Instant::now()
                                        .duration_since(t)
                                        .as_millis() as u64;
                                    elapsed.saturating_sub(hysteresis_ms)
                                });
                                info!(
                                    "Silero VAD: speech stopped, committing ({ms}ms buffer / ~{}ms speech; peak_p={:.3}, peak_silence={} of {} required, silence_streaks_broken={})",
                                    utterance_duration_ms.unwrap_or(0),
                                    peak_prob,
                                    vad.peak_silence_in_burst,
                                    SILENCE_STOP_FRAMES,
                                    vad.silence_streaks_broken,
                                );
                                send_q.send(&ws_cmd_tx, WsCommand::Commit { utterance_duration_ms });
                                vad.reset_after_commit();
                            } else {
                                debug!(
                                    "VAD stop but only {}ms buffered — skipping commit",
                                    (vad.bytes_since_commit * 1000)
                                        / (SAMPLE_RATE as u64 * BYTES_PER_SAMPLE as u64)
                                );
                                vad.in_speech = false;
                            }
                        }
                    }
                }

                // If no transition handled this chunk, dispatch based on state:
                //   - in_speech: send to server, count it
                //   - not in_speech: add to rolling prefix buffer (no send)
                if !sent_via_prefix_flush {
                    if vad.in_speech {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        send_q.send(&ws_cmd_tx, WsCommand::SendAudio { audio_b64: b64 });
                        vad.bytes_since_commit += bytes.len() as u64;
                    } else {
                        vad.push_prefix(&bytes);
                    }
                }

                // Safety: bound how long a single commit can buffer up if VAD
                // never declares end-of-speech (continuous monologue).
                if vad.bytes_since_commit >= MAX_BUFFER_BYTES {
                    let ms = (vad.bytes_since_commit * 1000)
                        / (SAMPLE_RATE as u64 * BYTES_PER_SAMPLE as u64);
                    info!("Max-buffer safety commit triggered ({ms}ms uncommitted)");
                    // We're mid-utterance — use elapsed since speech_started
                    // (if known) as a rough duration estimate.
                    let utterance_duration_ms = vad.speech_start_instant.map(|t| {
                        std::time::Instant::now().duration_since(t).as_millis() as u64
                    });
                    send_q.send(&ws_cmd_tx, WsCommand::Commit { utterance_duration_ms });
                    vad.reset_after_commit();
                }
            }

            event = audio_event_rx.recv() => {
                let Some(event) = event else { break };
                match event {
                    AudioEvent::SessionReset => {
                        vad = VadState::new();
                        debug!("Session reset: VAD state cleared");
                    }
                    AudioEvent::DumpAudio => {
                        crate::audio_dump::dump_to_wav(&vad.fallback_buf);
                    }
                }
            }

            _ = keepalive.tick() => {
                // Opportunistic backlog drain + gauge/drop reporting. Chunks
                // also flush on arrival; this covers the mic-muted case.
                send_q.try_flush(&ws_cmd_tx);
                // try_reserve before mutating local state so a full metrics
                // channel delays reporting (next tick retries) instead of
                // silently losing gauge updates / drop counts.
                let backlog_ms = send_q.backlog_ms();
                vad.debug.set_backlog_ms(backlog_ms);
                if backlog_ms != last_reported_backlog_ms
                    && let Ok(permit) = metrics_tx.try_reserve()
                {
                    permit.send(MetricsEvent::SendBacklog { ms: backlog_ms });
                    last_reported_backlog_ms = backlog_ms;
                }
                if let Ok(permit) = metrics_tx.try_reserve() {
                    let dropped = send_q.take_dropped();
                    if dropped.chunks > 0 || dropped.commits > 0 {
                        warn!(
                            "Send backlog overflow: dropped {} chunks ({}ms audio), {} commits",
                            dropped.chunks, dropped.ms, dropped.commits,
                        );
                        permit.send(MetricsEvent::AudioDropped {
                            chunks: dropped.chunks,
                            ms: dropped.ms,
                            commits: dropped.commits,
                        });
                    }
                }

                // Refresh the debug snapshot if no chunks have arrived recently
                // (mic muted, or capture stalled). Holds the widget visible.
                let stale = vad.last_chunk_at
                    .map(|t| std::time::Instant::now().duration_since(t)
                        > std::time::Duration::from_millis(400))
                    .unwrap_or(true);
                if stale {
                    vad.debug.flush_paused(
                        vad.in_speech,
                        vad.consecutive_speech,
                        vad.consecutive_silence,
                    );
                }
            }
        }
    }
}
