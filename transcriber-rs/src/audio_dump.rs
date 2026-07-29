//! On-demand audio dump for debugging missed transcriptions.
//!
//! When the user hits the dump hotkey (SIGHUP, see main.rs), the audio
//! router calls into here to write its rolling 30s fallback buffer to a
//! timestamped WAV file. Lets us inspect / cross-test alternative VADs
//! against actual recorded misses.
//!
//! Format: 24kHz mono i16 LE (matches what cpal captures). 44-byte WAV
//! header written by hand to avoid a new dependency.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use tracing::{info, warn};

const SAMPLE_RATE: u32 = 24_000;
const BITS_PER_SAMPLE: u16 = 16;
const CHANNELS: u16 = 1;

/// Concatenate the rolling fallback buffer and write a WAV to
/// `conversations/audio_dumps/dump_YYYYMMDD_HHMMSS.wav`. Returns the
/// path it wrote (or None on failure — already logged).
pub fn dump_to_wav(fallback_buf: &VecDeque<(Instant, Vec<u8>)>) -> Option<PathBuf> {
    if fallback_buf.is_empty() {
        warn!("audio_dump: fallback buffer empty, nothing to dump");
        notify("Audio dump failed", "Rolling buffer is empty — no audio captured yet");
        return None;
    }

    let dir = PathBuf::from("conversations/audio_dumps");
    if let Err(e) = fs::create_dir_all(&dir) {
        warn!("audio_dump: failed to create {}: {e}", dir.display());
        return None;
    }

    let now = chrono::Local::now();
    let stamp = now.format("%Y%m%d_%H%M%S").to_string();
    let path = dir.join(format!("dump_{stamp}.wav"));

    let total_bytes: usize = fallback_buf.iter().map(|(_, b)| b.len()).sum();
    let duration_ms = (total_bytes as u64 * 1000)
        / (SAMPLE_RATE as u64 * (BITS_PER_SAMPLE as u64 / 8) * CHANNELS as u64);

    let mut file = match File::create(&path) {
        Ok(f) => f,
        Err(e) => {
            warn!("audio_dump: open {} failed: {e}", path.display());
            return None;
        }
    };

    if let Err(e) = write_wav_header(&mut file, total_bytes as u32) {
        warn!("audio_dump: header write failed: {e}");
        return None;
    }

    for (_, chunk) in fallback_buf {
        if let Err(e) = file.write_all(chunk) {
            warn!("audio_dump: data write failed: {e}");
            return None;
        }
    }

    info!(
        "audio_dump: wrote {} ({} bytes, ~{}ms of audio)",
        path.display(),
        total_bytes,
        duration_ms,
    );
    notify(
        &format!("Saved ~{}s of audio", duration_ms / 1000),
        &path.display().to_string(),
    );
    Some(path)
}

/// Desktop notification — the dump hotkey's only user-visible feedback.
/// Runs notify-send on a detached thread: the caller is the audio router,
/// which must never block (notify-send can stall on DBus), and waiting
/// there also reaps the child so it doesn't linger as a zombie.
fn notify(summary: &str, body: &str) {
    let summary = summary.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        let result = std::process::Command::new("notify-send")
            .args(["--app-name=transcriber", "--expire-time=4000", &summary, &body])
            .status();
        if let Err(e) = result {
            warn!("audio_dump: notify-send failed: {e}");
        }
    });
}

fn write_wav_header(file: &mut File, data_bytes: u32) -> std::io::Result<()> {
    let byte_rate = SAMPLE_RATE * (BITS_PER_SAMPLE as u32 / 8) * CHANNELS as u32;
    let block_align = (BITS_PER_SAMPLE / 8) * CHANNELS;

    // RIFF chunk
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_bytes).to_le_bytes())?; // ChunkSize
    file.write_all(b"WAVE")?;

    // fmt sub-chunk
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?; // Subchunk1Size (PCM)
    file.write_all(&1u16.to_le_bytes())?; // AudioFormat (PCM)
    file.write_all(&CHANNELS.to_le_bytes())?;
    file.write_all(&SAMPLE_RATE.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&BITS_PER_SAMPLE.to_le_bytes())?;

    // data sub-chunk header
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}
