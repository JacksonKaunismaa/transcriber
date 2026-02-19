use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::messages::AudioChunk;

/// Target audio config for OpenAI Realtime API: 24kHz mono PCM16.
const TARGET_RATE: u32 = 24000;

/// Start audio capture on a background thread.
///
/// cpal's Stream is `!Send`, so stream creation and lifetime must live on the
/// same thread. The cpal audio callback uses `try_send` to avoid blocking the
/// OS realtime thread — if the channel is full (1024 slots ≈ 43s), the chunk
/// is silently dropped.
pub fn start_audio_capture(
    tx: mpsc::Sender<AudioChunk>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let host = cpal::default_host();

    // Try to find a working device + config on the main thread for early error reporting.
    let (device, config) = find_working_device(&host)?;
    let device_name = device.name().unwrap_or_else(|_| "unknown".into());
    println!("[INFO] Using audio device: {device_name}");

    std::thread::spawn(move || {
        let start_time = std::time::Instant::now();

        let stream = match device.build_input_stream(
            &config,
            move |data: &[i16], _info: &cpal::InputCallbackInfo| {
                let timestamp_ms = start_time.elapsed().as_millis() as u64;
                let chunk = AudioChunk {
                    timestamp_ms,
                    data: data.to_vec(),
                };
                let _ = tx.try_send(chunk);
            },
            move |err| {
                error!("Audio stream error: {err}");
            },
            None,
        ) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to build audio stream: {e}");
                return;
            }
        };

        if let Err(e) = stream.play() {
            error!("Failed to start audio stream: {e}");
            return;
        }

        info!("Audio capture started");

        while !cancel.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        drop(stream);
        info!("Audio capture stopped");
    });

    Ok(())
}

/// Find a device and stream config that actually works.
///
/// Tries all input devices with multiple config strategies:
/// 1. Preferred: 24kHz, mono, i16, default buffer (lets PipeWire/ALSA choose)
/// 2. Fallback: 24kHz, mono, i16, fixed 1024 buffer
/// 3. Last resort: any device that supports 24kHz i16 capture
///
/// This mirrors PyAudio/PortAudio's approach of trying multiple ALSA PCMs,
/// which is necessary when the PipeWire ALSA plugin's "default" PCM is broken
/// but raw hw: devices still work.
fn find_working_device(host: &cpal::Host) -> anyhow::Result<(cpal::Device, cpal::StreamConfig)> {
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

    // Collect all input devices: default first, then all others
    let mut devices: Vec<cpal::Device> = Vec::new();
    if let Some(default) = host.default_input_device() {
        devices.push(default);
    }
    if let Ok(all) = host.input_devices() {
        for d in all {
            // Skip duplicates (default is already first)
            let dominated = d.name().ok().and_then(|name| {
                devices.first().and_then(|first| {
                    first.name().ok().map(|first_name| name == first_name)
                })
            }).unwrap_or(false);
            if !dominated {
                devices.push(d);
            }
        }
    }

    if devices.is_empty() {
        anyhow::bail!("No audio input devices found");
    }

    // Try each device with each config
    for device in &devices {
        let name = device.name().unwrap_or_else(|_| "?".into());

        // First check if the device advertises 24kHz i16 support
        let supports_target = device
            .supported_input_configs()
            .is_ok_and(|mut configs| {
                configs.any(|c| {
                    c.min_sample_rate().0 <= TARGET_RATE
                        && c.max_sample_rate().0 >= TARGET_RATE
                        && c.sample_format() == cpal::SampleFormat::I16
                })
            });

        if !supports_target {
            // Still try — PipeWire may resample transparently even if
            // the supported configs don't list our target rate
        }

        for config in &configs_to_try {
            // Test by actually building a stream (the only reliable way)
            match device.build_input_stream(
                config,
                |_data: &[i16], _info: &cpal::InputCallbackInfo| {},
                |_err| {},
                None,
            ) {
                Ok(test_stream) => {
                    drop(test_stream);
                    info!(device = %name, "Audio device opened successfully");
                    return Ok((device.clone(), config.clone()));
                }
                Err(e) => {
                    warn!(device = %name, error = %e, "Failed to open audio device, trying next");
                }
            }
        }
    }

    anyhow::bail!(
        "No audio device supports {TARGET_RATE}Hz mono i16 capture. \
         Tried {} devices.",
        devices.len()
    )
}
