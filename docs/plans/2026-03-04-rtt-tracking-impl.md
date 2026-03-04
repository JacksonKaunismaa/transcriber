# RTT Tracking Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add ping RTT and transcription RTT tracking so "working but slow" is detectable via metrics and health status.

**Architecture:** Websocket task captures timestamps for ping/pong and item creation/completion, sends RTT values to metrics task via existing channel. Metrics task stores recent RTTs in bounded deques, computes percentiles, surfaces them in the METRICS log line, and uses latency thresholds for health classification.

**Tech Stack:** Rust, tokio, std::collections::HashMap/VecDeque

---

### Task 1: Add MetricsEvent variants

**Files:**
- Modify: `transcriber-rs/src/messages.rs:69-85`

**Step 1: Add PingRtt and TranscriptionRtt variants**

Add to the `MetricsEvent` enum (after `ApiError` on line 84):

```rust
pub enum MetricsEvent {
    ConnectionAttempt,
    ConnectionSuccess,
    SessionExpiration,
    ReconnectionAttempt,
    AudioChunkSent,
    RealtimeTranscription,
    Timeout,
    FallbackSuccess,
    FallbackFailure { duration_ms: u64 },
    FallbackRace,
    ShortSegmentSkipped,
    DuplicateFiltered,
    ContentFiltered,
    WebSocketError,
    ApiError,
    PingRtt { millis: u64 },
    TranscriptionRtt { item_id: String, millis: u64 },
}
```

**Step 2: Build to verify no compile errors**

Run: `cd transcriber-rs && cargo build 2>&1 | head -20`
Expected: Warnings about unmatched variants in `metrics.rs` (we'll handle those in Task 3). No hard errors.

**Step 3: Commit**

```bash
git add transcriber-rs/src/messages.rs
git commit -m "Add PingRtt and TranscriptionRtt metrics event variants"
```

---

### Task 2: Capture RTT in websocket task

**Files:**
- Modify: `transcriber-rs/src/websocket.rs`

**Step 1: Add HashMap import and item_created_at map**

At the top of `do_connection` (after line 163 `let (mut ws_write, mut ws_read) = ws_stream.split();`), add the item creation time tracker:

```rust
use std::collections::HashMap;
// ...
let mut item_created_at: HashMap<String, std::time::Instant> = HashMap::new();
```

Move the `use std::collections::HashMap;` to the file-level imports at the top of the file (line 1-6 area).

**Step 2: Capture ping RTT on pong receive**

Replace line 301-303:
```rust
Some(Ok(Message::Pong(_))) => {
    last_pong = std::time::Instant::now();
}
```

With:
```rust
Some(Ok(Message::Pong(_))) => {
    last_pong = std::time::Instant::now();
    if let Some(ping_time) = last_ping {
        let rtt_ms = ping_time.elapsed().as_millis() as u64;
        info!("Ping RTT: {rtt_ms}ms");
        metrics_tx.send(MetricsEvent::PingRtt { millis: rtt_ms }).await.ok();
    }
}
```

**Step 3: Record item creation timestamp**

In `handle_server_event`, the `"conversation.item.created"` branch (lines 359-371) currently sends `TranscriptEvent::ItemCreated`. But `handle_server_event` is a standalone async function — it doesn't have access to the `item_created_at` map.

Two options:
- (A) Pass `&mut HashMap` into `handle_server_event`
- (B) Track creation times in the `do_connection` loop by inspecting the `EventAction` return

Option A is cleaner. Change `handle_server_event` signature to accept the map, and insert on item created.

Update the signature (line 339):
```rust
async fn handle_server_event(
    text: &str,
    audio_event_tx: &mpsc::Sender<AudioEvent>,
    transcript_tx: &mpsc::Sender<TranscriptEvent>,
    metrics_tx: &mpsc::Sender<MetricsEvent>,
    item_created_at: &mut HashMap<String, std::time::Instant>,
) -> EventAction {
```

Update the call site (lines 270-275):
```rust
let event_result = handle_server_event(
    &text,
    audio_event_tx,
    transcript_tx,
    metrics_tx,
    &mut item_created_at,
).await;
```

In the `"conversation.item.created"` branch (line 359-371), add timestamp recording:
```rust
"conversation.item.created" => {
    if let Some(item) = &event.item {
        if let Some(id) = &item.id {
            item_created_at.insert(id.clone(), std::time::Instant::now());
            transcript_tx
                .send(TranscriptEvent::ItemCreated {
                    item_id: id.clone(),
                })
                .await
                .ok();
        }
    }
    EventAction::Continue
}
```

**Step 4: Compute and send transcription RTT on completion**

In the `"conversation.item.input_audio_transcription.completed"` branch (lines 399-427), after the existing `metrics_tx.send(MetricsEvent::RealtimeTranscription)`, add RTT computation:

```rust
"conversation.item.input_audio_transcription.completed"
| "response.audio_transcript.done" => {
    metrics_tx
        .send(MetricsEvent::RealtimeTranscription)
        .await
        .ok();
    if let Some(transcript) = &event.transcript {
        if !transcript.is_empty() {
            info!("Realtime transcription: {transcript}");
            if let Some(item_id) = &event.item_id {
                // Compute and report transcription RTT
                if let Some(created) = item_created_at.remove(item_id) {
                    let rtt_ms = created.elapsed().as_millis() as u64;
                    info!("Transcription RTT: {rtt_ms}ms for {item_id}");
                    metrics_tx.send(MetricsEvent::TranscriptionRtt {
                        item_id: item_id.clone(),
                        millis: rtt_ms,
                    }).await.ok();
                }
                // Cancel pending fallback timer in audio_buffer
                audio_event_tx
                    .send(AudioEvent::ItemCompleted {
                        item_id: item_id.clone(),
                    })
                    .await
                    .ok();
                transcript_tx
                    .send(TranscriptEvent::RealtimeCompleted {
                        item_id: item_id.clone(),
                        transcript: transcript.clone(),
                    })
                    .await
                    .ok();
            }
        }
    }
    EventAction::Continue
}
```

**Step 5: Clear stale entries on reconnect**

After `transcript_tx.send(TranscriptEvent::SessionReset)` (line 178), add:
```rust
item_created_at.clear();
```

**Step 6: Build to verify compilation**

Run: `cd transcriber-rs && cargo build 2>&1 | head -20`
Expected: Warning about unmatched `PingRtt`/`TranscriptionRtt` in `metrics.rs`. No errors.

**Step 7: Commit**

```bash
git add transcriber-rs/src/websocket.rs
git commit -m "Capture ping RTT and transcription RTT in websocket task"
```

---

### Task 3: Store RTTs and compute percentiles in metrics

**Files:**
- Modify: `transcriber-rs/src/metrics.rs`

**Step 1: Add RTT storage fields to Metrics struct**

Add after `connected: bool` (line 139):
```rust
    ping_rtts: VecDeque<u64>,
    transcription_rtts: VecDeque<u64>,
```

Initialize in `Metrics::new()` (after `connected: false` on line 163):
```rust
    ping_rtts: VecDeque::new(),
    transcription_rtts: VecDeque::new(),
```

**Step 2: Add percentile helper function**

Add as a free function (above or below `get_rss_mb`):

```rust
/// Compute p50 and p95 from a deque of values. Returns (p50, p95) or None if empty.
fn percentiles(values: &VecDeque<u64>) -> Option<(u64, u64)> {
    if values.is_empty() {
        return None;
    }
    let mut sorted: Vec<u64> = values.iter().copied().collect();
    sorted.sort_unstable();
    let len = sorted.len();
    let p50 = sorted[len / 2];
    let p95 = sorted[(len * 95 / 100).min(len - 1)];
    Some((p50, p95))
}
```

**Step 3: Handle new events in apply()**

Add match arms in `apply()` (after the `MetricsEvent::ApiError` arm, line 218):

```rust
MetricsEvent::PingRtt { millis } => {
    self.ping_rtts.push_back(millis);
    if self.ping_rtts.len() > 100 {
        self.ping_rtts.pop_front();
    }
}
MetricsEvent::TranscriptionRtt { millis, .. } => {
    self.transcription_rtts.push_back(millis);
    if self.transcription_rtts.len() > 100 {
        self.transcription_rtts.pop_front();
    }
    self.update_health();
}
```

**Step 4: Add RTT percentiles to METRICS log line**

In `log_stats()`, after computing `rss_mb` (line 224) and before the format string, add:

```rust
let ping_stats = percentiles(&self.ping_rtts)
    .map(|(p50, p95)| format!("ping p50:{p50}ms p95:{p95}ms"))
    .unwrap_or_else(|| "ping -".to_string());
let rtt_stats = percentiles(&self.transcription_rtts)
    .map(|(p50, p95)| format!("rtt p50:{p50}ms p95:{p95}ms"))
    .unwrap_or_else(|| "rtt -".to_string());
```

Update the format string (lines 242-261) to include the RTT stats. Insert after `rss:{rss_mb:.0}MB`:

```rust
let stats = format!(
    "METRICS [{minutes}m] | rss:{rss_mb:.0}MB | {ping_stats} | {rtt_stats} | \
     5m: {w5m} | 15m: {w15m} | 1h: {w1h} | all: {wall} | \
     races:{races} dupes:{dupes} short_skip:{ss} | \
     conn:{cs}/{ca} expires:{se} reconnects:{ra} | \
     errors: ws={we} api={ae}",
    w5m = w5m.format(),
    w15m = w15m.format(),
    w1h = w1h.format(),
    wall = wall.format(),
    races = self.fallback_races,
    dupes = self.duplicates_filtered,
    ss = self.short_segments_skipped,
    cs = self.connection_successes,
    ca = self.connection_attempts,
    se = self.session_expirations,
    ra = self.reconnection_attempts,
    we = self.websocket_errors,
    ae = self.api_errors,
);
```

**Step 5: Add latency thresholds to update_health()**

In `update_health()`, after the existing fail-rate logic (line 288 `"ok"` branch), add a latency check. Replace the inner block:

```rust
fn update_health(&mut self) {
    let health = if !self.connected {
        "error"
    } else {
        let recent = self.recent.last_n(8);
        let successes = recent.realtime + recent.fallback_ok;
        let failures = recent.fallback_fail;
        let total = successes + failures;
        if total < 3 {
            // Not enough data for fail-rate, but check latency
            if self.is_latency_degraded() {
                "degraded"
            } else {
                "ok"
            }
        } else {
            let fail_rate = failures as f64 / total as f64;
            if fail_rate > 0.5 {
                "error"
            } else if fail_rate > 0.25 || self.is_latency_degraded() {
                "degraded"
            } else {
                "ok"
            }
        }
    };
    if health != self.last_health {
        self.last_health = health;
        write_health_file(health);
    }
}

/// Check if recent transcription latency indicates degradation.
/// Uses the last 8 RTT values (matching the event window size).
fn is_latency_degraded(&self) -> bool {
    let recent: Vec<u64> = self.transcription_rtts.iter().rev().take(8).copied().collect();
    if recent.len() < 3 {
        return false;
    }
    let mut sorted = recent.clone();
    sorted.sort_unstable();
    let p50 = sorted[sorted.len() / 2];
    let p95 = sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)];
    p50 > 2000 || p95 > 3000
}
```

**Step 6: Build and verify**

Run: `cd transcriber-rs && cargo build 2>&1`
Expected: Clean build, no warnings about unmatched variants.

**Step 7: Commit**

```bash
git add transcriber-rs/src/metrics.rs
git commit -m "Add RTT percentile tracking and latency-based health degradation"
```

---

### Task 4: Build, smoke test, and final commit

**Step 1: Release build**

Run: `cd transcriber-rs && cargo build --release 2>&1`
Expected: Clean build.

**Step 2: Verify METRICS line format**

Run: `cd transcriber-rs && cargo test 2>&1` (if tests exist)
Otherwise, run the binary briefly and grep for the METRICS line to verify format:
```bash
grep METRICS $(ls -t conversations/*.jsonl | head -1) | tail -1
```
Expected: Line includes `ping p50:XXms p95:XXms | rtt p50:XXms p95:XXms` or `ping - | rtt -` if no data yet.

**Step 3: Update CLAUDE.md metrics documentation**

Update the "Checking Transcription Performance" section in `CLAUDE.md` to mention the new RTT fields in the METRICS line. Add to the "Key fields" list:
- `ping p50/p95` — WebSocket ping round-trip latency (ms)
- `rtt p50/p95` — Transcription round-trip latency: item created → transcription received (ms)

Also mention that individual RTT values are logged as `"Ping RTT: Xms"` and `"Transcription RTT: Xms for item_id"` for retroactive analysis.

**Step 4: Commit docs**

```bash
git add CLAUDE.md
git commit -m "Update docs with RTT metrics fields"
```
