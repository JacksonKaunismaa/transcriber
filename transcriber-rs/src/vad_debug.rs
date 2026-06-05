//! Live VAD state snapshot writer for the Quickshell debug overlay.
//!
//! Maintains a ring buffer of recent silero probabilities and writes a small
//! JSON snapshot to `$XDG_RUNTIME_DIR/transcriber_vad.json` every ~160ms.
//! The widget reads this file via Quickshell's inotify-based FileView.
//!
//! Atomic write pattern: serialize → tmp file → rename. Readers never see a
//! partial file.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use serde::Serialize;
use tracing::warn;

/// 10s of history at ~31 frames/sec.
const HISTORY_CAPACITY: usize = 320;

/// Flush every 5 frames ≈ 160ms — fast enough for a live sparkline, cheap
/// enough that the file write isn't a measurable cost.
const FLUSH_EVERY_N_FRAMES: u32 = 5;

#[derive(Serialize)]
struct Snapshot {
    /// Wall-clock unix ms — lets the widget detect a stale file.
    written_at_unix_ms: u128,
    /// Constants the widget renders as threshold lines / counter targets.
    speech_start_threshold: f32,
    speech_continue_threshold: f32,
    speech_start_frames: u32,
    silence_stop_frames: u32,
    /// Dynamic state.
    in_speech: bool,
    current_prob: f32,
    consecutive_speech: u32,
    consecutive_silence: u32,
    /// True when audio capture is paused (mic mute) — widget renders a
    /// muted indicator and lets the sparkline freeze.
    paused: bool,
    /// (ms_since_writer_start, probability), oldest first.
    history: Vec<(u64, f32)>,
}

pub struct VadDebugWriter {
    out_path: PathBuf,
    tmp_path: PathBuf,
    history: VecDeque<(u64, f32)>,
    start: Instant,
    frames_since_flush: u32,
    last_prob: f32,
    speech_start_threshold: f32,
    speech_continue_threshold: f32,
    speech_start_frames: u32,
    silence_stop_frames: u32,
}

impl VadDebugWriter {
    pub fn new(
        speech_start_threshold: f32,
        speech_continue_threshold: f32,
        speech_start_frames: u32,
        silence_stop_frames: u32,
    ) -> Self {
        let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        Self {
            out_path: PathBuf::from(format!("{dir}/transcriber_vad.json")),
            tmp_path: PathBuf::from(format!("{dir}/transcriber_vad.json.tmp")),
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
            start: Instant::now(),
            frames_since_flush: 0,
            last_prob: 0.0,
            speech_start_threshold,
            speech_continue_threshold,
            speech_start_frames,
            silence_stop_frames,
        }
    }

    /// Push one silero probability into the ring buffer. Cheap; call every frame.
    pub fn record(&mut self, prob: f32) {
        let ms = self.start.elapsed().as_millis() as u64;
        if self.history.len() == HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back((ms, prob));
        self.last_prob = prob;
        self.frames_since_flush += 1;
    }

    /// If enough frames have accumulated since the last flush, serialize and
    /// rename. Otherwise a no-op. Call after every step_vad call.
    pub fn maybe_flush(
        &mut self,
        in_speech: bool,
        consecutive_speech: u32,
        consecutive_silence: u32,
    ) {
        if self.frames_since_flush < FLUSH_EVERY_N_FRAMES {
            return;
        }
        self.frames_since_flush = 0;
        self.write(in_speech, consecutive_speech, consecutive_silence, false);
    }

    /// Force a snapshot write tagged paused=true. Used by a keepalive timer
    /// during mute so the widget doesn't think the transcriber crashed. Does
    /// not touch the history ring buffer — the sparkline freezes naturally.
    pub fn flush_paused(
        &mut self,
        in_speech: bool,
        consecutive_speech: u32,
        consecutive_silence: u32,
    ) {
        self.write(in_speech, consecutive_speech, consecutive_silence, true);
    }

    /// Drop the history ring buffer. Use on resume after a capture gap so
    /// stale timestamps don't distort the sparkline x-axis.
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    fn write(
        &mut self,
        in_speech: bool,
        consecutive_speech: u32,
        consecutive_silence: u32,
        paused: bool,
    ) {
        let written_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        let snapshot = Snapshot {
            written_at_unix_ms,
            speech_start_threshold: self.speech_start_threshold,
            speech_continue_threshold: self.speech_continue_threshold,
            speech_start_frames: self.speech_start_frames,
            silence_stop_frames: self.silence_stop_frames,
            in_speech,
            current_prob: self.last_prob,
            consecutive_speech,
            consecutive_silence,
            paused,
            history: self.history.iter().copied().collect(),
        };

        let Ok(bytes) = serde_json::to_vec(&snapshot) else {
            warn!("vad_debug: serialize failed");
            return;
        };
        if let Err(e) = std::fs::write(&self.tmp_path, &bytes) {
            warn!("vad_debug: write tmp failed: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&self.tmp_path, &self.out_path) {
            warn!("vad_debug: rename failed: {e}");
        }
    }
}

impl Drop for VadDebugWriter {
    fn drop(&mut self) {
        // Remove the snapshot on clean shutdown so the widget stops drawing
        // stale data. Best-effort.
        let _ = std::fs::remove_file(&self.out_path);
    }
}
