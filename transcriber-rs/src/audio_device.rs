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
/// same thread. We spawn a dedicated thread that:
/// 1. Tries all input devices with multiple configs
/// 2. Opens the first working one
/// 3. Streams audio until cancellation
pub fn start_audio_capture(
    tx: mpsc::Sender<AudioChunk>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    // Spawn the audio thread. All device enumeration and stream creation
    // happens here because cpal::Stream is !Send.
    std::thread::spawn(move || {
        let host = cpal::default_host();

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

        // Collect devices: default first, then all others
        let mut devices: Vec<cpal::Device> = Vec::new();
        if let Some(default) = host.default_input_device() {
            devices.push(default);
        }
        if let Ok(all) = host.input_devices() {
            for d in all {
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
            error!("No audio input devices found");
            return;
        }

        // Try each device with each config — build the REAL stream, not a test
        let start_time = std::time::Instant::now();
        let mut stream_and_name: Option<(cpal::Stream, String)> = None;

        for device in devices {
            let name = device.name().unwrap_or_else(|_| "?".into());

            for config in &configs_to_try {
                let tx_clone = tx.clone();
                let start = start_time;

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
                    |err| {
                        error!("Audio stream error: {err}");
                    },
                    None,
                ) {
                    Ok(s) => {
                        info!(device = %name, "Audio device opened successfully");
                        println!("[INFO] Using audio device: {name}");
                        stream_and_name = Some((s, name));
                        break;
                    }
                    Err(e) => {
                        warn!(device = %name, error = %e, "Failed to open, trying next");
                    }
                }
            }

            if stream_and_name.is_some() {
                break;
            }
        }

        let Some((stream, _name)) = stream_and_name else {
            error!("No audio device supports {TARGET_RATE}Hz mono i16 capture");
            return;
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
