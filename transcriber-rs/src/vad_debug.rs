//! Live VAD state snapshot writer for the Quickshell debug overlay.
//!
//! Maintains a ring buffer of recent silero probabilities and writes a small
//! JSON snapshot to `$XDG_RUNTIME_DIR/transcriber_vad.json` every ~160ms.
//! The widget reads this file via Quickshell's inotify-based FileView.
//!
//! Writes are in-place (truncate + write) so the file keeps its inode across
//! flushes — inotify watchers (the overlay's FileView) survive. Readers may
//! rarely see a torn write and must tolerate a JSON parse failure by
//! retrying on the next change event; the overlay already does.

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
    /// Audio duration queued locally behind a slow websocket (network
    /// backpressure) — widget shows a buffering indicator when nonzero.
    backlog_ms: u64,
    /// (ms_since_writer_start, probability), oldest first.
    history: Vec<(u64, f32)>,
}

pub struct VadDebugWriter {
    out_path: PathBuf,
    history: VecDeque<(u64, f32)>,
    start: Instant,
    frames_since_flush: u32,
    last_prob: f32,
    backlog_ms: u64,
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
        Self::in_dir(
            &dir,
            speech_start_threshold,
            speech_continue_threshold,
            speech_start_frames,
            silence_stop_frames,
        )
    }

    fn in_dir(
        dir: &str,
        speech_start_threshold: f32,
        speech_continue_threshold: f32,
        speech_start_frames: u32,
        silence_stop_frames: u32,
    ) -> Self {
        Self {
            out_path: PathBuf::from(format!("{dir}/transcriber_vad.json")),
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
            start: Instant::now(),
            frames_since_flush: 0,
            last_prob: 0.0,
            backlog_ms: 0,
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

    /// Update the send-backlog gauge included in subsequent snapshots.
    pub fn set_backlog_ms(&mut self, ms: u64) {
        self.backlog_ms = ms;
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
            backlog_ms: self.backlog_ms,
            history: self.history.iter().copied().collect(),
        };

        let Ok(bytes) = serde_json::to_vec(&snapshot) else {
            warn!("vad_debug: serialize failed");
            return;
        };
        // In-place rewrite (truncate + write), NOT tmp + rename: replacing the
        // file swaps its inode, which kills inotify watches — the overlay's
        // FileView then misses updates and its stale-detector blinks the panel
        // off mid-dictation (2026-07-28 blink incident). A torn read during the
        // non-atomic window is tolerated by the reader (parse-failure retry).
        if let Err(e) = std::fs::write(&self.out_path, &bytes) {
            warn!("vad_debug: write failed: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn writer_in(dir: &std::path::Path) -> VadDebugWriter {
        VadDebugWriter::in_dir(dir.to_str().unwrap(), 0.4, 0.15, 3, 30)
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vad_debug_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Regression test for the overlay blink (2026-07-28): rewriting the
    /// snapshot must keep the file's inode. Replacing it (tmp + rename)
    /// swaps the inode, which kills inotify watches on the file — the
    /// Quickshell overlay's FileView then misses updates and the widget's
    /// stale-detector blinks the panel off mid-dictation.
    #[test]
    fn snapshot_rewrite_preserves_inode() {
        let dir = temp_dir("inode");
        let mut w = writer_in(&dir);

        w.record(0.5);
        w.write(true, 1, 0, false);
        let ino_first = std::fs::metadata(&w.out_path).unwrap().ino();

        w.record(0.6);
        w.write(true, 2, 0, false);
        let ino_second = std::fs::metadata(&w.out_path).unwrap().ino();

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            ino_first, ino_second,
            "snapshot rewrite must reuse the inode or inotify watches on it die"
        );
    }

    /// Every flush must leave complete, parseable JSON at the snapshot path.
    #[test]
    fn snapshot_rewrite_leaves_valid_json() {
        let dir = temp_dir("json");
        let mut w = writer_in(&dir);

        for i in 0..20 {
            w.record(i as f32 / 20.0);
            w.write(i % 2 == 0, i, 0, false);
            let bytes = std::fs::read(&w.out_path).unwrap();
            let parsed: serde_json::Value = serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("write {i} left invalid JSON: {e}"));
            assert!(parsed["written_at_unix_ms"].as_u64().is_some());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
