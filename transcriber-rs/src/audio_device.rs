use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::messages::AudioChunk;

/// Start audio capture on a background thread.
///
/// cpal's Stream is `!Send`, so the entire device enumeration, stream creation,
/// and stream lifetime must live on the same thread. We spawn a dedicated thread
/// that owns the stream until cancellation.
///
/// The cpal audio callback uses `try_send` to avoid blocking the OS realtime thread.
/// If the channel is full (1024 slots ≈ 43s), the chunk is silently dropped.
pub fn start_audio_capture(
    tx: mpsc::Sender<AudioChunk>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    // We do device discovery on the main thread for early error reporting,
    // but build the stream on the dedicated thread.
    let host = cpal::default_host();
    let device = find_input_device(&host)?;
    let device_name = device.name().unwrap_or_else(|_| "unknown".into());
    println!("[INFO] Using audio device: {device_name}");

    // cpal::Device IS Send, so we can move it into the thread.
    // cpal::Stream is !Send, so it must be created and dropped on the same thread.
    std::thread::spawn(move || {
        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(24000),
            buffer_size: cpal::BufferSize::Fixed(1024),
        };

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

        // Keep stream alive until cancellation
        while !cancel.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        drop(stream);
        info!("Audio capture stopped");
    });

    Ok(())
}

/// Find a suitable input device. Prefers PipeWire, falls back to default.
fn find_input_device(host: &cpal::Host) -> anyhow::Result<cpal::Device> {
    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(name) = device.name() {
                if name.to_lowercase().contains("pipewire") {
                    if supports_24khz(&device) {
                        return Ok(device);
                    }
                }
            }
        }
    }

    if let Some(device) = host.default_input_device() {
        return Ok(device);
    }

    anyhow::bail!("No audio input device found")
}

fn supports_24khz(device: &cpal::Device) -> bool {
    device
        .supported_input_configs()
        .is_ok_and(|mut configs| {
            configs.any(|c| {
                c.channels() == 1
                    && c.min_sample_rate().0 <= 24000
                    && c.max_sample_rate().0 >= 24000
                    && c.sample_format() == cpal::SampleFormat::I16
            })
        })
}
