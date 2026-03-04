# RTT Tracking for Transcriber Metrics

**Date:** 2026-03-04
**Status:** Approved

## Problem

Current metrics only track success/failure counts. "Working but slow" (3-4s RTT instead of 0.5-1.5s) is invisible — health stays "ok" as long as transcriptions eventually arrive. No ping or transcription latency is recorded, so degradation can't be detected or diagnosed retroactively.

## Design

### New MetricsEvent Variants

```rust
MetricsEvent::PingRtt { millis: u64 }
MetricsEvent::TranscriptionRtt { millis: u64 }
```

### Websocket Task (websocket.rs)

**Ping RTT:** When pong arrives, compute `last_ping.elapsed()` and send `PingRtt` to metrics.

**Transcription RTT:** Add `HashMap<String, Instant>` to the websocket loop. Insert `item_id -> Instant::now()` on `conversation.item.created`. On `transcription.completed`, look up creation time, compute delta, send `TranscriptionRtt`, remove entry. Clear the map on reconnect (stale entries from dropped connections).

### Metrics Task (metrics.rs)

**Storage:** Two `VecDeque<u64>` fields — `ping_rtts` and `transcription_rtts`, capped at 100 entries each.

**Percentile calculation:** Copy deque to vec, sort, index at p50/p95 positions. Runs once per minute on <= 100 elements.

**METRICS line format:**
```
METRICS [1578m] | rss:32MB | ping p50:45ms p95:82ms | rtt p50:620ms p95:1340ms | 5m: rt:15 ...
```

### Health Classification (metrics.rs)

Existing fail-rate logic stays. Add latency thresholds using recent transcription RTTs (last 8):

- p95 > 3000ms -> "degraded"
- p50 > 2000ms -> "degraded"

Existing fail-rate thresholds still trigger "error" when things fully break.

### Logging

Each RTT event gets a tracing log for retroactive analysis:
```
info!("Ping RTT: {millis}ms");
info!("Transcription RTT: {millis}ms for {item_id}");
```

## Approach

All timing captured in the websocket task (sees both ends of item lifecycle). Metrics task just stores and aggregates. No new tasks or cross-task coordination needed.

## Files Changed

- `transcriber-rs/src/messages.rs` — Add PingRtt, TranscriptionRtt variants
- `transcriber-rs/src/websocket.rs` — Capture ping RTT on pong, track item creation times
- `transcriber-rs/src/metrics.rs` — Store RTT deques, compute percentiles, update METRICS line and health logic
