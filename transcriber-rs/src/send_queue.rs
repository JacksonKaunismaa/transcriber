//! Non-blocking `WsCommand` sender with local overflow buffering.
//!
//! The audio router must never block on the network: when the websocket
//! task stalls (congested uplink, reconnect in progress), the bounded
//! ws_cmd channel fills. A blocking `send().await` there freezes the whole
//! router select loop — VAD stops, the debug snapshot goes stale (the bar
//! overlay hides itself), and the capture-gap detector false-fires on
//! recovery, force-committing mid-utterance (INCIDENTS.md 2026-07-18).
//!
//! Instead, commands overflow into this local FIFO and are flushed
//! opportunistically as the channel frees up. Beyond a bounded backlog the
//! oldest entries are dropped and counted — stale audio is worth less than
//! a live pipeline, but the drop must be visible in metrics.

use std::collections::VecDeque;

use tokio::sync::mpsc;

use crate::messages::WsCommand;

/// Drop counters accumulated since the last `take_dropped()` call.
#[derive(Debug, Default, PartialEq)]
pub struct Dropped {
    pub chunks: u64,
    pub ms: u64,
    pub commits: u64,
}

pub struct SendQueue {
    queue: VecDeque<WsCommand>,
    /// Backlog cap, in raw audio bytes (decoded, not base64).
    max_audio_bytes: u64,
    /// Raw audio bytes currently queued (commits count as 0).
    queued_audio_bytes: u64,
    /// Raw audio byte rate, for bytes → ms conversion.
    bytes_per_sec: u64,
    dropped: Dropped,
}

impl SendQueue {
    pub fn new(max_audio_bytes: u64, bytes_per_sec: u64) -> Self {
        Self {
            queue: VecDeque::new(),
            max_audio_bytes,
            queued_audio_bytes: 0,
            bytes_per_sec,
            dropped: Dropped::default(),
        }
    }

    /// Raw (decoded) audio bytes a command represents; commits are free.
    fn audio_bytes(cmd: &WsCommand) -> u64 {
        match cmd {
            WsCommand::SendAudio { audio_b64 } => (audio_b64.len() as u64 * 3) / 4,
            WsCommand::Commit { .. } => 0,
        }
    }

    /// Enqueue a command and flush as much of the queue as the channel will
    /// take. Never blocks. Oldest entries are dropped past the backlog cap.
    pub fn send(&mut self, tx: &mpsc::Sender<WsCommand>, cmd: WsCommand) {
        self.queued_audio_bytes += Self::audio_bytes(&cmd);
        self.queue.push_back(cmd);
        while self.queued_audio_bytes > self.max_audio_bytes {
            let Some(old) = self.queue.pop_front() else { break };
            let bytes = Self::audio_bytes(&old);
            self.queued_audio_bytes -= bytes;
            match old {
                WsCommand::SendAudio { .. } => {
                    self.dropped.chunks += 1;
                    self.dropped.ms += bytes * 1000 / self.bytes_per_sec;
                }
                WsCommand::Commit { .. } => self.dropped.commits += 1,
            }
        }
        self.try_flush(tx);
    }

    /// Move queued commands into the channel until it's full or we're empty.
    pub fn try_flush(&mut self, tx: &mpsc::Sender<WsCommand>) {
        while let Some(cmd) = self.queue.pop_front() {
            let bytes = Self::audio_bytes(&cmd);
            match tx.try_send(cmd) {
                Ok(()) => self.queued_audio_bytes -= bytes,
                Err(mpsc::error::TrySendError::Full(cmd)) => {
                    self.queue.push_front(cmd);
                    break;
                }
                // Receiver gone = shutdown; nothing left to deliver to.
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.queued_audio_bytes -= bytes;
                }
            }
        }
    }

    /// Duration of audio currently waiting in the local queue (excludes
    /// whatever already sits in the channel).
    pub fn backlog_ms(&self) -> u64 {
        self.queued_audio_bytes * 1000 / self.bytes_per_sec
    }

    /// Return and reset the drop counters accumulated since the last call.
    pub fn take_dropped(&mut self) -> Dropped {
        std::mem::take(&mut self.dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 byte = 1 ms at this test rate — keeps assertions readable.
    const TEST_RATE: u64 = 1000;

    /// A SendAudio command whose decoded payload is `raw_bytes` long, tagged
    /// with a leading char so ordering can be asserted after drain. Use
    /// multiples of 3 so the base64 length round-trips exactly (the
    /// production accounting is a gauge and tolerates padding slop).
    fn chunk(tag: char, raw_bytes: u64) -> WsCommand {
        assert_eq!(raw_bytes % 3, 0, "test sizes must round-trip exactly");
        let b64_len = (raw_bytes * 4 / 3) as usize;
        let mut s = String::with_capacity(b64_len);
        s.push(tag);
        while s.len() < b64_len {
            s.push('x');
        }
        WsCommand::SendAudio { audio_b64: s }
    }

    fn tag_of(cmd: &WsCommand) -> char {
        match cmd {
            WsCommand::SendAudio { audio_b64 } => audio_b64.chars().next().unwrap(),
            WsCommand::Commit { .. } => '#',
        }
    }

    fn drain_tags(rx: &mut mpsc::Receiver<WsCommand>) -> Vec<char> {
        let mut tags = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            tags.push(tag_of(&cmd));
        }
        tags
    }

    #[tokio::test]
    async fn passes_through_when_channel_has_space() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut q = SendQueue::new(10_000, TEST_RATE);

        q.send(&tx, chunk('a', 300));

        assert_eq!(drain_tags(&mut rx), vec!['a']);
        assert_eq!(q.backlog_ms(), 0);
        assert_eq!(q.take_dropped(), Dropped::default());
    }

    #[tokio::test]
    async fn buffers_on_full_channel_and_flushes_in_order() {
        let (tx, mut rx) = mpsc::channel(1);
        let mut q = SendQueue::new(10_000, TEST_RATE);

        q.send(&tx, chunk('a', 300)); // fills the channel
        q.send(&tx, chunk('b', 300)); // buffered locally
        q.send(&tx, chunk('c', 300)); // buffered locally

        assert_eq!(q.backlog_ms(), 600);

        // Drain one slot at a time; flush must preserve FIFO order.
        let mut got = Vec::new();
        for _ in 0..3 {
            got.extend(drain_tags(&mut rx));
            q.try_flush(&tx);
        }
        got.extend(drain_tags(&mut rx));

        assert_eq!(got, vec!['a', 'b', 'c']);
        assert_eq!(q.backlog_ms(), 0);
        assert_eq!(q.take_dropped(), Dropped::default());
    }

    #[tokio::test]
    async fn drops_oldest_audio_beyond_backlog_cap() {
        let (tx, mut rx) = mpsc::channel(1);
        let mut q = SendQueue::new(1000, TEST_RATE);

        q.send(&tx, chunk('a', 300)); // straight into the channel
        q.send(&tx, chunk('b', 402)); // queued: 402
        q.send(&tx, chunk('c', 402)); // queued: 804
        q.send(&tx, chunk('d', 402)); // queued: 1206 > 1000 → 'b' dropped

        assert_eq!(
            q.take_dropped(),
            Dropped { chunks: 1, ms: 402, commits: 0 }
        );
        assert_eq!(q.backlog_ms(), 804);

        let mut got = Vec::new();
        for _ in 0..3 {
            got.extend(drain_tags(&mut rx));
            q.try_flush(&tx);
        }
        got.extend(drain_tags(&mut rx));
        assert_eq!(got, vec!['a', 'c', 'd']);
    }

    #[tokio::test]
    async fn commits_flow_in_order_and_cost_no_backlog() {
        let (tx, mut rx) = mpsc::channel(1);
        let mut q = SendQueue::new(10_000, TEST_RATE);

        q.send(&tx, chunk('a', 300));
        q.send(&tx, WsCommand::Commit { committed_audio_ms: Some(5) });
        q.send(&tx, chunk('b', 300));

        // Only 'b' counts toward the audio backlog.
        assert_eq!(q.backlog_ms(), 300);

        let mut got = Vec::new();
        for _ in 0..3 {
            got.extend(drain_tags(&mut rx));
            q.try_flush(&tx);
        }
        got.extend(drain_tags(&mut rx));
        assert_eq!(got, vec!['a', '#', 'b']);
    }

    #[tokio::test]
    async fn dropped_commits_are_counted() {
        let (tx, _rx) = mpsc::channel(1);
        let mut q = SendQueue::new(100, TEST_RATE);

        q.send(&tx, chunk('a', 60)); // fills the channel
        q.send(&tx, WsCommand::Commit { committed_audio_ms: None }); // queued, 0 bytes
        q.send(&tx, chunk('b', 81)); // queued: 81
        q.send(&tx, chunk('c', 81)); // queued: 162 > 100 → commit + 'b' dropped

        assert_eq!(
            q.take_dropped(),
            Dropped { chunks: 1, ms: 81, commits: 1 }
        );
        assert_eq!(q.backlog_ms(), 81);
    }
}
