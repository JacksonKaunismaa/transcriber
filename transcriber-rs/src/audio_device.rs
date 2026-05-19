use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::messages::AudioChunk;

/// Shared pause state — toggled by SIGUSR1 from mic-toggle.sh.
/// When paused, the cpal stream is paused; the patched cpal worker
/// (see `vendor/cpal/PATCH.md`) then blocks on a condvar instead of
/// busy-spinning in alsa::poll(), so the cpal_alsa_in thread sits at 0% CPU
/// while muted. PipeWire/RNNoise CPU is unaffected — they stay at their
/// baseline whether muted or not.
static PAUSED: AtomicBool = AtomicBool::new(false);

/// Check if audio capture is currently paused.
pub fn is_paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

/// Set pause state directly. Returns the new state.
pub fn set_paused(paused: bool) -> bool {
    PAUSED.store(paused, Ordering::Relaxed);
    paused
}

/// Target audio config for OpenAI Realtime API: 24kHz mono PCM16.
const TARGET_RATE: u32 = 24000;

/// Errors per 100ms check window before triggering a rebuild.
const ERROR_THRESHOLD: u64 = 50;

/// Maximum consecutive rebuild attempts before giving up.
/// PipeWire can crash-loop during audio filter failures or device re-enumeration,
/// so this needs enough headroom to ride out the instability.
const MAX_REBUILDS: u32 = 15;

/// Seconds of healthy operation before resetting the rebuild counter.
const HEALTHY_RESET_SECS: u64 = 30;

/// Delay before attempting to rebuild a broken stream.
const REBUILD_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Try all input devices and return the first working stream.
fn try_build_stream(
    host: &cpal::Host,
    tx: &mpsc::Sender<AudioChunk>,
    start_time: std::time::Instant,
    error_count: &Arc<AtomicU64>,
) -> Option<(cpal::Stream, String)> {
    let configs_to_try = [
        cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(TARGET_RATE),
            buffer_size: cpal::BufferSize::Default,
        },
        cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(TARGET_RATE),
            buffer_size: cpal::BufferSize::Fixed(1024),
        },
    ];

    // Collect devices: default first, then all others (deduped)
    let mut devices: Vec<cpal::Device> = Vec::new();
    if let Some(default) = host.default_input_device() {
        devices.push(default);
    }
    if let Ok(all) = host.input_devices() {
        for d in all {
            let is_dup = d
                .name()
                .ok()
                .and_then(|name| {
                    devices
                        .first()
                        .and_then(|first| first.name().ok().map(|first_name| name == first_name))
                })
                .unwrap_or(false);
            if !is_dup {
                devices.push(d);
            }
        }
    }

    if devices.is_empty() {
        error!("No audio input devices found");
        return None;
    }

    // Try each device with each config — build the REAL stream, not a test
    for device in devices {
        let name = device.name().unwrap_or_else(|_| "?".into());

        for config in &configs_to_try {
            let tx_clone = tx.clone();
            let start = start_time;
            let err_count = Arc::clone(error_count);

            match device.build_input_stream(
                config,
                move |data: &[i16], _info: &cpal::InputCallbackInfo| {
                    let timestamp_ms = start.elapsed().as_millis() as u64;
                    let chunk = AudioChunk {
                        timestamp_ms,
                        data: data.to_vec(),
                    };
                    let _ = tx_clone.try_send(chunk);
                },
                move |err| {
                    let n = err_count.fetch_add(1, Ordering::Relaxed);
                    if n == 0 {
                        error!("Audio stream error: {err}");
                    }
                },
                None,
            ) {
                Ok(s) => {
                    info!(device = %name, "Audio device opened successfully");
                    return Some((s, name));
                }
                Err(e) => {
                    warn!(device = %name, error = %e, "Failed to open, trying next");
                }
            }
        }
    }

    None
}

/// Start audio capture on a background thread.
///
/// cpal's Stream is `!Send`, so stream creation and lifetime must live on the
/// same thread. The thread runs a build-monitor-rebuild loop: if the stream
/// starts producing errors (e.g. ALSA EPIPE), it drops the broken stream,
/// waits briefly, and rebuilds.
pub fn start_audio_capture(
    tx: mpsc::Sender<AudioChunk>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    std::thread::spawn(move || {
        let host = cpal::default_host();
        let start_time = std::time::Instant::now();
        let error_count = Arc::new(AtomicU64::new(0));
        let mut consecutive_rebuilds: u32 = 0;

        'rebuild: loop {
            if cancel.is_cancelled() {
                break;
            }

            // Reset error counter before each build attempt
            error_count.store(0, Ordering::Relaxed);

            let Some((stream, device_name)) =
                try_build_stream(&host, &tx, start_time, &error_count)
            else {
                consecutive_rebuilds += 1;
                if consecutive_rebuilds > MAX_REBUILDS {
                    error!(
                        "No audio device available after {MAX_REBUILDS} consecutive attempts, giving up"
                    );
                    return;
                }
                warn!(
                    attempt = consecutive_rebuilds,
                    "No audio device available, retrying"
                );
                sleep_cancellable(&cancel, REBUILD_DELAY);
                continue;
            };

            if let Err(e) = stream.play() {
                error!("Failed to start audio stream: {e}");
                consecutive_rebuilds += 1;
                if consecutive_rebuilds > MAX_REBUILDS {
                    error!("Giving up after {MAX_REBUILDS} consecutive rebuild failures");
                    return;
                }
                sleep_cancellable(&cancel, REBUILD_DELAY);
                continue;
            }

            if consecutive_rebuilds > 0 {
                info!(device = %device_name, rebuilds = consecutive_rebuilds, "Audio stream rebuilt");
                println!("[INFO] Audio stream rebuilt, using: {device_name}");
            } else {
                info!(device = %device_name, "Audio capture started");
                println!("[INFO] Using audio device: {device_name}");
            }

            let healthy_since = std::time::Instant::now();
            let mut currently_paused = false;

            // Monitor loop: check error counter and pause state every 100ms
            loop {
                if cancel.is_cancelled() {
                    break 'rebuild;
                }

                std::thread::sleep(std::time::Duration::from_millis(100));

                // Handle pause/resume toggled by SIGUSR1
                let should_pause = is_paused();
                if should_pause != currently_paused {
                    if should_pause {
                        if let Err(e) = stream.pause() {
                            warn!(error = %e, "Failed to pause audio stream");
                        } else {
                            info!(device = %device_name, "Audio stream paused (mic muted)");
                            println!("[INFO] Audio paused");
                        }
                    } else {
                        if let Err(e) = stream.play() {
                            warn!(error = %e, "Failed to resume audio stream, rebuilding");
                            drop(stream);
                            sleep_cancellable(&cancel, REBUILD_DELAY);
                            continue 'rebuild;
                        }
                        info!(device = %device_name, "Audio stream resumed (mic unmuted)");
                        println!("[INFO] Audio resumed");
                    }
                    currently_paused = should_pause;
                }

                // Skip error checking while paused
                if currently_paused {
                    continue;
                }

                let errors = error_count.swap(0, Ordering::Relaxed);
                if errors >= ERROR_THRESHOLD {
                    warn!(errors, device = %device_name, "Audio stream broken, rebuilding");
                    drop(stream);
                    consecutive_rebuilds += 1;
                    if consecutive_rebuilds > MAX_REBUILDS {
                        error!("Giving up after {MAX_REBUILDS} consecutive rebuild failures");
                        return;
                    }
                    sleep_cancellable(&cancel, REBUILD_DELAY);
                    continue 'rebuild;
                }

                // Reset rebuild counter after sustained healthy operation
                if consecutive_rebuilds > 0
                    && healthy_since.elapsed().as_secs() >= HEALTHY_RESET_SECS
                {
                    info!(
                        "Audio stream healthy for {HEALTHY_RESET_SECS}s, resetting rebuild counter"
                    );
                    consecutive_rebuilds = 0;
                }
            }
        }

        info!("Audio capture stopped");
    });

    Ok(())
}

/// Sleep in small increments so we can check cancellation.
fn sleep_cancellable(cancel: &CancellationToken, duration: std::time::Duration) {
    let step = std::time::Duration::from_millis(100);
    let mut remaining = duration;
    while remaining > std::time::Duration::ZERO && !cancel.is_cancelled() {
        let sleep = remaining.min(step);
        std::thread::sleep(sleep);
        remaining = remaining.saturating_sub(sleep);
    }
}
