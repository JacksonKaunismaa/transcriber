use std::collections::HashMap;

use base64::Engine;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::messages::{AudioChunk, AudioEvent, MetricsEvent, TranscriptEvent, WsCommand};

const SAMPLE_RATE: u64 = 24000;
const FRAMES_PER_CHUNK: u64 = 1024;
/// Milliseconds per chunk: 1024 / 24000 * 1000 ≈ 42.67ms
const MS_PER_CHUNK: f64 = (FRAMES_PER_CHUNK as f64 / SAMPLE_RATE as f64) * 1000.0;

/// Timeout before triggering Whisper fallback
const TIMEOUT_SECONDS: f64 = 2.5;
/// Margin for timestamp matching when extracting audio
const TIMESTAMP_MARGIN_MS: i64 = 200;
/// Minimum segment duration to attempt transcription
const MIN_DURATION_MS: u64 = 300;
/// Maximum age of audio chunks to keep (30s)
const MAX_BUFFER_AGE_MS: u64 = 30_000;

/// Speech timing info for a conversation item.
struct SpeechTiming {
    start_ms: u64,
    end_ms: Option<u64>,
    stopped_at: Option<std::time::Instant>,
    completed: bool,
}

/// Run the Audio Router task.
///
/// Receives raw audio from cpal, base64-encodes and forwards to WebSocket.
/// Tracks speech timing from VAD events.
/// Triggers Whisper fallback when items timeout.
pub async fn run_audio_router_task(
    mut audio_rx: mpsc::Receiver<AudioChunk>,
    mut audio_event_rx: mpsc::Receiver<AudioEvent>,
    ws_cmd_tx: mpsc::Sender<WsCommand>,
    transcript_tx: mpsc::Sender<TranscriptEvent>,
    metrics_tx: mpsc::Sender<MetricsEvent>,
    cancel: CancellationToken,
    api_key: String,
) {
    // Chunks tagged with session-relative ms (matching OpenAI's audio_start_ms domain),
    // not raw cpal uptime. See session_origin_ms below.
    let mut audio_buffer: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut speech_times: HashMap<String, SpeechTiming> = HashMap::new();
    let mut timeout_check = tokio::time::interval(std::time::Duration::from_secs(1));
    timeout_check.tick().await;

    // Cpal uptime ms at the start of the current OpenAI session. OpenAI's
    // audio_start_ms / audio_end_ms reset to ~0 on each session.created, so
    // local chunk timestamps must reset too — otherwise extract_audio_chunks
    // searches in the wrong window after any reconnect.
    let mut session_origin_ms: u64 = 0;
    let mut latest_chunk_ts_ms: u64 = 0;

    let http_client = reqwest::Client::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Audio router shutting down");
                break;
            }

            // Receive audio from cpal and forward to WebSocket
            chunk = audio_rx.recv() => {
                match chunk {
                    Some(chunk) => {
                        // Convert i16 samples to bytes (little-endian)
                        let bytes: Vec<u8> = chunk.data.iter()
                            .flat_map(|s| s.to_le_bytes())
                            .collect();

                        // Track latest cpal uptime so SessionReset can re-anchor.
                        latest_chunk_ts_ms = chunk.timestamp_ms;
                        let session_relative_ts = chunk.timestamp_ms.saturating_sub(session_origin_ms);

                        // Store in buffer for potential fallback (session-relative ts)
                        audio_buffer.push((session_relative_ts, bytes.clone()));

                        // Base64 encode and send to WebSocket
                        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        ws_cmd_tx.send(WsCommand::SendAudio { audio_b64 }).await.ok();

                        metrics_tx.send(MetricsEvent::AudioChunkSent).await.ok();
                    }
                    None => break,
                }
            }

            // Receive VAD events from WebSocket
            event = audio_event_rx.recv() => {
                match event {
                    Some(AudioEvent::SpeechStarted { item_id, audio_start_ms }) => {
                        speech_times.insert(item_id, SpeechTiming {
                            start_ms: audio_start_ms,
                            end_ms: None,
                            stopped_at: None,
                            completed: false,
                        });
                    }
                    Some(AudioEvent::SpeechStopped { item_id, audio_end_ms }) => {
                        if let Some(timing) = speech_times.get_mut(&item_id) {
                            timing.end_ms = Some(audio_end_ms);
                            timing.stopped_at = Some(std::time::Instant::now());
                        }
                    }
                    Some(AudioEvent::ItemCompleted { item_id }) => {
                        if let Some(timing) = speech_times.get_mut(&item_id) {
                            timing.completed = true;
                        }
                    }
                    Some(AudioEvent::SessionReset) => {
                        audio_buffer.clear();
                        speech_times.clear();
                        // Re-anchor: next chunk's session-relative ts will be ~0,
                        // matching OpenAI's audio_start_ms after session.created.
                        session_origin_ms = latest_chunk_ts_ms;
                        debug!("Session reset: cleared audio buffer; new origin {session_origin_ms}ms");
                    }
                    None => break,
                }
            }

            // Periodic timeout check + buffer pruning
            _ = timeout_check.tick() => {
                prune_old_data(&mut audio_buffer, &mut speech_times);

                let timed_out: Vec<(String, u64, u64)> = speech_times.iter()
                    .filter(|(_, t)| !t.completed && t.stopped_at.is_some())
                    .filter(|(_, t)| {
                        t.stopped_at.unwrap().elapsed().as_secs_f64() >= TIMEOUT_SECONDS
                    })
                    .filter_map(|(id, t)| {
                        t.end_ms.map(|end| (id.clone(), t.start_ms, end))
                    })
                    .collect();

                for (item_id, start_ms, end_ms) in timed_out {
                    // Mark as completed to prevent double-processing
                    if let Some(timing) = speech_times.get_mut(&item_id) {
                        timing.completed = true;
                    }

                    let duration_ms = end_ms.saturating_sub(start_ms);
                    warn!("Item {} timeout after {TIMEOUT_SECONDS}s, trying fallback",
                          &item_id[..20.min(item_id.len())]);
                    metrics_tx.send(MetricsEvent::Timeout).await.ok();

                    if duration_ms < MIN_DURATION_MS {
                        debug!("Skipping short segment ({duration_ms}ms)");
                        metrics_tx.send(MetricsEvent::ShortSegmentSkipped).await.ok();
                        transcript_tx.send(TranscriptEvent::FallbackCompleted {
                            item_id,
                            transcript: String::new(),
                            duration_ms: Some(duration_ms),
                        }).await.ok();
                        continue;
                    }

                    // Extract audio and call Whisper
                    let audio_data = extract_audio_chunks(&audio_buffer, start_ms, end_ms);
                    match audio_data {
                        Some(data) => {
                            let transcript = fallback_transcribe(
                                &http_client, &api_key, &data, &item_id
                            ).await;

                            match transcript {
                                Some(text) => {
                                    info!("Fallback transcription success: {text} [item_id={item_id}]");
                                    metrics_tx.send(MetricsEvent::FallbackSuccess).await.ok();
                                    transcript_tx.send(TranscriptEvent::FallbackCompleted {
                                        item_id,
                                        transcript: text,
                                        duration_ms: Some(duration_ms),
                                    }).await.ok();
                                }
                                None => {
                                    metrics_tx.send(MetricsEvent::FallbackFailure { duration_ms }).await.ok();
                                    transcript_tx.send(TranscriptEvent::FallbackCompleted {
                                        item_id,
                                        transcript: String::new(),
                                        duration_ms: Some(duration_ms),
                                    }).await.ok();
                                }
                            }
                        }
                        None => {
                            warn!("No matching chunks found for fallback");
                            metrics_tx.send(MetricsEvent::FallbackFailure { duration_ms }).await.ok();
                            transcript_tx.send(TranscriptEvent::FallbackCompleted {
                                item_id,
                                transcript: String::new(),
                                duration_ms: Some(duration_ms),
                            }).await.ok();
                        }
                    }
                }
            }
        }
    }
}

/// Remove old audio chunks and completed speech times.
fn prune_old_data(
    audio_buffer: &mut Vec<(u64, Vec<u8>)>,
    speech_times: &mut HashMap<String, SpeechTiming>,
) {
    if let Some(&(latest_ts, _)) = audio_buffer.last() {
        let cutoff = latest_ts.saturating_sub(MAX_BUFFER_AGE_MS);
        audio_buffer.retain(|(ts, _)| *ts >= cutoff);
    }

    speech_times.retain(|_, t| {
        if t.completed {
            t.stopped_at
                .map(|s| s.elapsed().as_secs() < 30)
                .unwrap_or(true)
        } else {
            true
        }
    });
}

/// Extract audio chunks for a time range, trying different offsets.
fn extract_audio_chunks(
    audio_buffer: &[(u64, Vec<u8>)],
    start_ms: u64,
    end_ms: u64,
) -> Option<Vec<u8>> {
    let expected_duration = end_ms.saturating_sub(start_ms) as f64;
    let mut best_chunks: Option<Vec<&Vec<u8>>> = None;
    let mut best_error = f64::INFINITY;

    for offset in (-TIMESTAMP_MARGIN_MS..=TIMESTAMP_MARGIN_MS).step_by(20) {
        let test_start = (start_ms as i64 + offset).max(0) as u64;
        let test_end = (end_ms as i64 + offset).max(0) as u64;

        let candidates: Vec<&Vec<u8>> = audio_buffer
            .iter()
            .filter(|(ts, _)| *ts >= test_start && *ts <= test_end)
            .map(|(_, data)| data)
            .collect();

        if !candidates.is_empty() {
            let actual_duration = candidates.len() as f64 * MS_PER_CHUNK;
            let duration_error = (expected_duration - actual_duration).abs();

            if duration_error < best_error {
                best_error = duration_error;
                best_chunks = Some(candidates);
            }
        }
    }

    best_chunks.map(|chunks| {
        chunks.into_iter().flat_map(|c| c.iter().copied()).collect()
    })
}

/// Call OpenAI Whisper API for fallback transcription.
async fn fallback_transcribe(
    client: &reqwest::Client,
    api_key: &str,
    pcm_data: &[u8],
    item_id: &str,
) -> Option<String> {
    // Build WAV in memory
    let wav_data = build_wav(pcm_data)?;

    debug!("Fallback transcribing item {} with Whisper API",
           &item_id[..20.min(item_id.len())]);

    let part = reqwest::multipart::Part::bytes(wav_data)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .ok()?;

    let form = reqwest::multipart::Form::new()
        .text("model", "whisper-1")
        .part("file", part);

    let response = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {api_key}"))
        .multipart(form)
        .send()
        .await
        .ok()?;

    if !response.status().is_success() {
        warn!("Whisper API returned {}", response.status());
        return None;
    }

    #[derive(serde::Deserialize)]
    struct WhisperResponse {
        text: String,
    }

    let body: WhisperResponse = response.json().await.ok()?;
    if body.text.is_empty() {
        None
    } else {
        Some(body.text)
    }
}

/// Build a WAV file from raw PCM data (24kHz, mono, 16-bit).
fn build_wav(pcm_data: &[u8]) -> Option<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 24000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(&mut cursor, spec).ok()?;
        for chunk in pcm_data.chunks_exact(2) {
            let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
            writer.write_sample(sample).ok()?;
        }
        writer.finalize().ok()?;
    }
    Some(cursor.into_inner())
}
