use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::messages::{AudioChunk, MetricsEvent};

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

/// Converts a device's native capture format to the 24kHz mono i16 the
/// pipeline expects. PipeWire resamples for us on Linux, so this only runs
/// when a device refuses 24kHz mono outright (CoreAudio: the MacBook mic only
/// opens at its native 48kHz).
///
/// Channels are averaged to mono, then each output sample is the mean of the
/// input samples that fall in its 1/24000 s slot. The box average doubles as
/// the anti-aliasing low-pass, so 12-24kHz content isn't folded into speech.
struct Downsampler {
    in_rate: u64,
    channels: usize,
    in_index: u64,
    slot: u64,
    sum: f32,
    count: u32,
}

impl Downsampler {
    fn new(in_rate: u32, channels: u16) -> Self {
        Self {
            in_rate: u64::from(in_rate),
            channels: usize::from(channels),
            in_index: 0,
            slot: 0,
            sum: 0.0,
            count: 0,
        }
    }

    /// Feed interleaved samples in [-1, 1]; completed output samples are
    /// appended to `out`. A partly filled slot carries over to the next call.
    fn push(&mut self, interleaved: impl Iterator<Item = f32>, out: &mut Vec<i16>) {
        let mut frame_sum = 0.0;
        for (i, sample) in interleaved.enumerate() {
            frame_sum += sample;
            if (i + 1) % self.channels != 0 {
                continue;
            }
            let mono = frame_sum / self.channels as f32;
            frame_sum = 0.0;

            let slot = self.in_index * u64::from(TARGET_RATE) / self.in_rate;
            if slot != self.slot && self.count > 0 {
                let mean = self.sum / self.count as f32;
                out.push((mean.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16);
                self.sum = 0.0;
                self.count = 0;
            }
            self.slot = slot;
            self.sum += mono;
            self.count += 1;
            self.in_index += 1;
        }
    }
}

/// Open `device` in its own default format, converting to 24kHz mono i16 in
/// the callback. Fallback for devices that reject the 24kHz mono configs.
fn try_build_native_stream(
    device: &cpal::Device,
    tx: &mpsc::Sender<AudioChunk>,
    metrics_tx: &mpsc::Sender<MetricsEvent>,
    error_count: &Arc<AtomicU64>,
) -> Result<cpal::Stream, String> {
    let supported = device
        .default_input_config()
        .map_err(|e| format!("no default input config: {e}"))?;
    let config = supported.config();
    if config.sample_rate.0 < TARGET_RATE {
        return Err(format!(
            "native rate {}Hz is below the {TARGET_RATE}Hz the API needs",
            config.sample_rate.0
        ));
    }
    info!(
        rate = config.sample_rate.0,
        channels = config.channels,
        format = ?supported.sample_format(),
        "Opening device at its native format; downsampling to {TARGET_RATE}Hz mono"
    );

    let mut downsampler = Downsampler::new(config.sample_rate.0, config.channels);
    let tx = tx.clone();
    let metrics_tx = metrics_tx.clone();
    let mut send = move |samples: &mut dyn Iterator<Item = f32>| {
        let mut data = Vec::new();
        downsampler.push(samples, &mut data);
        if data.is_empty() {
            return;
        }
        // try_send only (RT callback must not block); count losses as below.
        if tx.try_send(AudioChunk { data }).is_err() {
            let _ = metrics_tx.try_send(MetricsEvent::CaptureDropped { chunks: 1 });
        }
    };
    let err_count = Arc::clone(error_count);
    let on_error = move |err| {
        let n = err_count.fetch_add(1, Ordering::Relaxed);
        if n == 0 {
            error!("Audio stream error: {err}");
        }
    };

    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| send(&mut data.iter().copied()),
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                send(&mut data.iter().map(|&x| f32::from(x) / 32768.0))
            },
            on_error,
            None,
        ),
        other => return Err(format!("unsupported native sample format {other:?}")),
    };
    stream.map_err(|e| e.to_string())
}

/// Try all input devices and return the first working stream.
fn try_build_stream(
    host: &cpal::Host,
    tx: &mpsc::Sender<AudioChunk>,
    metrics_tx: &mpsc::Sender<MetricsEvent>,
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
            let metrics_clone = metrics_tx.clone();
            let err_count = Arc::clone(error_count);

            match device.build_input_stream(
                config,
                move |data: &[i16], _info: &cpal::InputCallbackInfo| {
                    let chunk = AudioChunk {
                        data: data.to_vec(),
                    };
                    // try_send only (RT callback must not block). A full
                    // channel means the router is wedged — count the loss
                    // so it shows up in METRICS instead of vanishing.
                    if tx_clone.try_send(chunk).is_err() {
                        let _ = metrics_clone.try_send(MetricsEvent::CaptureDropped { chunks: 1 });
                    }
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

        match try_build_native_stream(&device, tx, metrics_tx, error_count) {
            Ok(s) => {
                info!(device = %name, "Audio device opened successfully (native format)");
                return Some((s, name));
            }
            Err(e) => {
                warn!(device = %name, error = %e, "Native-format fallback failed, trying next device");
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
    metrics_tx: mpsc::Sender<MetricsEvent>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    std::thread::spawn(move || {
        let host = cpal::default_host();
        let error_count = Arc::new(AtomicU64::new(0));
        let mut consecutive_rebuilds: u32 = 0;

        'rebuild: loop {
            if cancel.is_cancelled() {
                break;
            }

            // Reset error counter before each build attempt
            error_count.store(0, Ordering::Relaxed);

            let Some((stream, device_name)) =
                try_build_stream(&host, &tx, &metrics_tx, &error_count)
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

#[cfg(test)]
mod downsampler_tests {
    use super::*;

    #[test]
    fn stereo_48k_becomes_mono_24k_with_levels_kept() {
        let mut ds = Downsampler::new(48_000, 2);
        let mut out = Vec::new();
        // 1s of stereo: left 0.5, right 0.0 -> mono 0.25, fed in odd-sized callbacks
        let input: Vec<f32> = (0..96_000).map(|i| if i % 2 == 0 { 0.5 } else { 0.0 }).collect();
        for chunk in input.chunks(1022) {
            ds.push(chunk.iter().copied(), &mut out);
        }
        // the final slot is still pending until more input arrives
        assert_eq!(out.len(), 23_999);
        let expected = (0.25 * f32::from(i16::MAX)) as i16;
        assert!(out.iter().all(|&x| x == expected));
    }

    #[test]
    fn non_integer_ratio_keeps_the_output_rate() {
        let mut ds = Downsampler::new(44_100, 1);
        let mut out = Vec::new();
        ds.push(std::iter::repeat_n(0.0, 44_100 * 3), &mut out);
        assert_eq!(out.len(), 3 * 24_000 - 1);
    }

    #[test]
    fn box_average_cancels_a_nyquist_tone() {
        // +1/-1 alternating at 48kHz is a 24kHz tone: inaudible to the API's
        // band, so it must average out rather than alias into it.
        let mut ds = Downsampler::new(48_000, 1);
        let mut out = Vec::new();
        ds.push((0..4_800).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }), &mut out);
        assert!(out.iter().all(|&x| x == 0));
    }
}
