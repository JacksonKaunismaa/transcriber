# Logging Format Evolution

How the transcriber's logging has evolved from initial Python prototype through the Rust port.

## Mental Model: How to Detect Which Schema a File Uses

There are **3 distinct JSONL schemas** (not 2). Detection from the first JSON line:

```
Has "fields" key?
  └─ YES → Rust tracing (Feb 20, 2026+)
  └─ NO → Has "level" key?
           └─ YES → Python logging (Dec 2025 -- Feb 19, 2026)
           └─ NO  → Python raw (Nov 2025 only)
```

| Schema | First-line example | Key difference |
|--------|-------------------|----------------|
| **Python raw** | `{"local_sequence":1,"timestamp":"2025-11-10T18:23:34","type":"transcription_session.created",...}` | No `level`, no `message` wrapper — flat API event object |
| **Python logging** | `{"timestamp":"2026-02-19 20:46:13,638","level":"DEBUG","message":{...} or "string"}` | `message` is polymorphic: string OR embedded API event dict |
| **Rust tracing** | `{"timestamp":"2026-03-20T03:06:11.909-04:00","level":"INFO","fields":{"message":"...","device":"..."}}` | `message` always a string, lives under `fields`, extra structured data as sibling keys |

**Critical gotcha**: Python logging `message` can be a **dict** (raw API event) or a **string** (app event). You must check `isinstance(msg, dict)` before treating it as text. The Rust schema's `fields.message` is always a string.

## What Text Is Available in Each Era

Not all eras logged the same things. This matters when analyzing transcription history:

| What you want | Where to find it | Eras available |
|---------------|-----------------|----------------|
| **Post-filter text** (what was actually typed) | `Transcript output: {text}` in JSONL, or `[ts] text` in `.txt` | `.txt`: Oct 2025 -- Feb 2026. JSONL `Transcript output`: Feb 20, 2026+ only |
| **Pre-filter text** (raw API output) | `Realtime transcription:` / completed events | Python raw: Nov 2025. Python logging: Dec 2025+. Rust: Feb 20, 2026+ |
| **Rejected text** (what filters removed) | `Filtered out:` | Python logging: Dec 2025+. Rust: Feb 20, 2026+ |
| **Metrics** | `METRICS [...]` line | Python logging: Dec 2025+. Rust: Feb 20, 2026+ |

**The gap**: From Oct 2025 -- Feb 19, 2026, post-filter text was ONLY in `.txt` files (not in JSONL). The parser handles this by falling back to `.txt` for sessions without JSONL `Transcript output:` entries.

## Active Files Per Era

Each era lists **every file being produced** and exactly what text content it captures.

### Era 1: Oct -- Nov 2025

| File | Format | Text Logged | Filtered? |
|------|--------|-------------|-----------|
| `transcription_*.txt` | `[timestamp] text` | Final transcriptions only (not partials) | **Pre-filter** — no filtering existed yet |
| _(terminal stdout)_ | `[PARTIAL] text` / `[FINAL] text` | All transcriptions including partials | Pre-filter |

No JSONL, no metrics files, no debug events.

### Era 2a: Nov -- Dec 2025

| File | Format | Text Logged | Filtered? |
|------|--------|-------------|-----------|
| `transcription_*.txt` | `[timestamp] text` | Final transcriptions | **Post-filter** — filters existed by now |
| `debug_events_*.jsonl` | Python JSON (see schema below) | Raw API events + app events including `Filtered out:` rejected text | Both — raw API text is pre-filter, `Filtered out:` shows what was rejected, `Transcript output:` (added later) is post-filter |
| _(terminal stdout)_ | `[PARTIAL] text` / `[FINAL] text` | All transcriptions | Post-filter for finals |

### Era 2b: Dec 2025 -- Feb 10, 2026

Same as 2a, plus:

| File | Format | Text Logged | Filtered? |
|------|--------|-------------|-----------|
| `metrics_*.txt` | Human-readable summary | No transcription text — just counters (realtime count, timeout %, filtered count, etc.) | N/A |

### Era 2c: Feb 10 -- Feb 19, 2026

`metrics_*.txt` **removed** — metrics consolidated into periodic `METRICS` line in the JSONL file.

| File | Format | Text Logged | Filtered? |
|------|--------|-------------|-----------|
| `transcription_*.txt` | `[timestamp] text` | Final transcriptions | Post-filter |
| `debug_events_*.jsonl` | Python JSON | Raw API events + app events + `METRICS` line every 60s | Mixed (see above) |

### Era 3a: Feb 19-20, 2026 (Rust port, transition)

Both file types briefly coexisted during testing:

| File | Format | Text Logged | Filtered? |
|------|--------|-------------|-----------|
| `transcription_*.txt` | `[timestamp] text` | Final transcriptions | Post-filter |
| `debug_events_*.jsonl` | Rust JSON (see schema below) | App events + metrics. **No more raw API event objects.** | Mixed |

### Era 3b: Feb 20, 2026 -- present

`transcription_*.txt` **removed**. Single file for everything:

| File | Format | Text Logged | Filtered? |
|------|--------|-------------|-----------|
| `debug_events_*.jsonl` | Rust JSON | Everything (see event catalog below) | Mixed — see next table |

**What text appears where in the JSONL:**

| Event | Text Content | Filtered? |
|-------|-------------|-----------|
| `Realtime transcription: {text} [item_id=X]` | Raw text from OpenAI Realtime API | **Pre-filter** — straight from the API |
| `Fallback transcription success: {text} [item_id=X]` | Raw text from Whisper fallback API | **Pre-filter** — straight from the API |
| `Filtered out: {text}` | Text that was **rejected** by filters | Shows what was removed (debug level) |
| `Repetition hallucination detected, dropping: {text}` | Text rejected by repetition detector | Shows what was removed |
| `Speed hallucination: ...` | No text content, just stats | N/A |
| `Fuzzy duplicate ({ratio}): {text}` | Text rejected as duplicate | Shows what was removed (debug level) |
| `Transcript output: {text}` | Final text sent to the typer | **Post-filter** — this is what was actually typed |

### File Type Summary

| Type | Produced During | Stopped |
|------|----------------|---------|
| `transcription_*.txt` (~327 files) | Oct 15, 2025 -- Feb 19, 2026 | Removed in Rust port (`5216e8e`, Feb 20, 2026) |
| `metrics_*.txt` (~72 files) | Dec 5, 2025 -- Feb 10, 2026 | Consolidated into JSONL (`17258af`, Feb 10, 2026) |
| `debug_events_*.jsonl` (427+ files) | Nov 10, 2025 -- present | Still active |

---

## Era 1: Plain Text (Oct -- Nov 2025)

**Files produced:** `transcription_*.txt` only. No JSONL, no metrics files.

Logging was `print()` to terminal + plain `.txt` files. No filtering existed yet — all text logged is raw API output.

**`transcription_*.txt`** format (pre-filter, raw API text):
```
[2025-10-15 16:59:31] Hey there! How can I help you today?
[2025-10-15 17:00:08] Got it! I'll focus on just transcribing what you say.
```

Three output paths: terminal display (`[PARTIAL]`/`[FINAL]` prefixes), file logging (final transcripts only), keyboard typing. No structured logging, no metrics.

---

## Era 2: Python JSONL (Nov 2025 -- Feb 2026)

**Files produced:** `transcription_*.txt` (post-filter final text) + `debug_events_*.jsonl` (everything) + `metrics_*.txt` (Dec 2025 -- Feb 10, 2026 only, then removed).

This era has **two distinct JSONL schemas**:

### Schema A: Python Raw (Nov 2025 only)

The earliest JSONL files have **no `level`/`message` wrapper** — each line is a flat API event object:

```json
{
  "local_sequence": 1,
  "timestamp": "2025-11-10T18:23:34.766568",
  "event_id": "event_CaQq6MwJ7Wvv47kFWAeNg",
  "type": "transcription_session.created",
  "item_id": null,
  "content_index": null,
  "delta": null,
  "transcript": null,
  "full_event": { ... full API response ... }
}
```

- **Timestamp**: ISO 8601, **no timezone** (local time)
- **No level field** — all events are DEBUG-equivalent
- **No string messages** — only raw API event types
- **Transcription text** is in the `transcript` field on `conversation.item.input_audio_transcription.completed` events
- **No metrics, no filter events, no app-level logging** — just raw API passthrough

### Schema B: Python Logging (Dec 2025 -- Feb 19, 2026)

Added Python `logging` module. Events now wrapped with `level` and `message`:

```json
{
  "timestamp": "2026-01-12 17:21:01,096",
  "level": "DEBUG",
  "message": <string or object>
}
```

- **Timestamp**: `YYYY-MM-DD HH:MM:SS,mmm` (Python logging format, comma before millis)
- **Levels**: `DEBUG`, `INFO`, `WARNING`, `ERROR`
- **`message` is polymorphic**: either a plain string (app events) or a dict (embedded API event, same structure as Schema A)

When `message` is a **dict**, it's the same flat API event object from Schema A (with `local_sequence`, `type`, `transcript`, `full_event`, etc.).

When `message` is a **string**, it's an app-level event (see list below).

**API event types logged as embedded objects** (inside `message` dict):
- `transcription_session.created` / `transcription_session.updated`
- `input_audio_buffer.speech_started` / `speech_stopped` / `committed`
- `conversation.item.created`
- `conversation.item.input_audio_transcription.delta` / `completed`
- `error`

### Application-Level String Events (Python)

When `message` is a string:
- `METRICS [Nm] | realtime:X timeouts:Y (Z%) fallback_ok:A fail_short:B fail_long:C races:D | filtered:E dupes:F | errors: ws=G api=H`
- `TYPER window='chromium' method=wtype`
- `Filtered out: <text>`
- `Item item_XXXX timeout after 2.5s, trying fallback`
- `Fallback extracted N chunks (offset Xms), duration error: Yms`
- `Fallback transcribing item item_XXXX with Whisper API`
- `Fallback transcription success: <text>`
- `Skipping item item_XXXX - fallback failed (Nms)`
- `Session expired: Your session hit the maximum duration of 60 minutes.`
- `Session state reset for reconnection`
- `WebSocket error: 0`
- `WebSocket connection lost unexpectedly, code=None, msg=None, will reconnect`

### Metrics Summary Files (Dec 2025 -- Feb 10, 2026)

Separate `metrics_*.txt` files with end-of-session summaries:

```
==================================================
TRANSCRIPTION SESSION METRICS
==================================================
Session Duration: 156m 0s
--- Connection ---
  Connection attempts:    3
  Successful connections: 3
  Session expirations:    2
--- Transcription ---
  Realtime API success:   340
  Timeouts (needed fallback): 8 (2.3%)
  Fallback successes:     8
  Fallback failures:      0
--- Filtering ---
  Short segments skipped: 0
  Duplicates filtered:    2
  Content filtered:       34
--- Errors ---
  WebSocket errors:       1
  API errors:             0
--- Audio ---
  Audio chunks sent:      218258
==================================================
```

Later versions added `Fallback fail (<1s)`, `Fallback fail (>=1s)`, and `Fallback races`.

**Removed Feb 10, 2026** (`17258af`) — all metrics consolidated into the periodic JSONL `METRICS` line.

---

## Era 3: Rust JSONL (Feb 19, 2026 -- present)

**Files produced:** `debug_events_*.jsonl` only (after brief transition period where `.txt` still coexisted, removed Feb 20). This single file contains everything: pre-filter raw text, post-filter final text, filtered-out text, metrics, connection events, errors.

Full rewrite to Rust. Logging via `tracing` crate with JSON formatter.

### JSON Schema (Rust era)

```json
{
  "timestamp": "2026-03-19T18:48:18.321-04:00",
  "level": "INFO",
  "fields": {
    "message": "...",
    ...optional structured fields...
  }
}
```

**Key structural differences from Python:**
1. **`message` moved to `fields.message`** — top-level `message` replaced by `fields` object
2. **No raw API event logging** — Rust logs parsed/summarized string messages, not raw API JSON
3. **Timestamp format changed** — ISO 8601 with timezone offset (see below)
4. **Level names changed** — `WARNING` -> `WARN`, `DEBUG` rarely used (most events at `info`)
5. **Structured fields on some events** — e.g. `{"message":"Audio device opened successfully","device":"pipewire"}`

### Timestamp Format Evolution

| Era | Format | Example |
|-----|--------|---------|
| Python | `YYYY-MM-DD HH:MM:SS,mmm` | `2026-01-12 17:21:01,096` |
| Early Rust (Feb 19-25) | ISO 8601 UTC | `2026-02-26T17:26:41.987036Z` |
| Later Rust (Mar 9+) | ISO 8601 local + offset | `2026-03-20T03:06:11.909-04:00` |

The switch from UTC to local time happened in commit `e246e66` (Mar 9, 2026) via a custom `LocalTimer` in the tracing subscriber.

### JSONL File Details

- **Naming**: `conversations/debug_events_{YYYYMMDD_HHMMSS}.jsonl` (local time)
- **Permissions**: `0o600` (owner-only, added `47892d4` Mar 3, 2026)
- **Initialization**: `tracing_subscriber` with JSON layer to file + human-readable layer to stderr (warn+ only)

---

## METRICS Line Evolution (3 versions)

### Version 1: Python (Dec 2025 -- Feb 2026)

```
METRICS [1m] | realtime:1 timeouts:1 (50.0%) fallback_ok:0 fail_short:1 fail_long:0 races:0 | filtered:0 dupes:0 | errors: ws=0 api=0
```

Flat cumulative counters. No memory, latency, connection, or windowed stats.

### Version 2: Rust without RTT (Feb 20 -- Mar 3, 2026)

```
METRICS [1m] | rss:27MB | 5m: rt:0 to:0 (0%) fb:0/0 filt:0 | 15m: rt:0 to:0 (0%) fb:0/0 filt:0 | 1h: rt:0 to:0 (0%) fb:0/0 filt:0 | all: rt:0 to:0 (0%) fb:0/0 filt:0 | races:0 dupes:0 short_skip:0 | conn:1/1 expires:0 reconnects:0 | errors: ws=0 api=0
```

Added:
- `rss` — RSS memory from `/proc/self/status`
- Rolling time windows: `5m`, `15m`, `1h`, `all` (each with rt/to/fb/filt)
- `conn:X/Y`, `expires`, `reconnects`, `short_skip`

### Version 3: Rust with RTT (Mar 4, 2026 -- present)

```
METRICS [1m] | rss:28MB | ping p50:122ms p95:122ms | rtt p50:826ms p95:2156ms | 5m: rt:4 to:0 (100%) fb:0/0 filt:1 | 15m: ... | 1h: ... | all: ... | races:0 dupes:0 short_skip:0 | conn:1/1 expires:0 reconnects:0 | errors: ws=0 api=0
```

Added `ping p50/p95` and `rtt p50/p95` percentile tracking (last 100 samples each). Shows `-` when no data available yet.

### METRICS Field Reference

| Field | Meaning | Since |
|-------|---------|-------|
| `[Nm]` | Session uptime in minutes | v1 (Python) |
| `rss` | Resident set size in MB | v2 (Rust) |
| `ping p50/p95` | WebSocket ping RTT percentiles (ms) | v3 |
| `rtt p50/p95` | Transcription RTT percentiles (ms) | v3 |
| `rt` | Realtime API transcription count (per window) | v2 |
| `to` | Timeout count + success % (per window) | v2 |
| `fb` | Fallback ok / total (per window) | v2 |
| `filt` | Content filtered count (per window) | v2 |
| `realtime` | Cumulative realtime count | v1 (removed in v2, replaced by windowed) |
| `timeouts` | Cumulative timeout count + % | v1 (removed in v2) |
| `fallback_ok` | Cumulative fallback successes | v1 (removed in v2) |
| `fail_short` / `fail_long` | Fallback failures by duration | v1 (removed in v2) |
| `races` | Fallback race conditions prevented | v1 |
| `dupes` | Fuzzy duplicates filtered | v1 |
| `short_skip` | Short segments skipped | v2 |
| `conn:X/Y` | Successful / attempted connections | v2 |
| `expires` | Session expirations | v2 |
| `reconnects` | Reconnection attempts | v2 |
| `errors: ws=N api=N` | WebSocket and API error counts | v1 |

---

## Current Event Types (Rust, all via `tracing`)

### Startup & Shutdown

| Level | Event | Structured Fields |
|-------|-------|-------------------|
| `INFO` | `Session starting` | `api_key_len`, `model` |
| `INFO` | `Audio device opened successfully` | `device` |
| `INFO` | `Audio capture started` | `device` |
| `INFO` | `Transcription session config sent (model: {model})` | — |
| `INFO` | `Session created` | — |
| `INFO` | `Session configured — ready for transcription` | — |
| `INFO` | `Transcript task shutting down` | — |
| `INFO` | `Audio router shutting down` | — |
| `INFO` | `Typer task shutting down` | — |
| `INFO` | `Audio capture stopped` | — |
| `ERROR` | `FATAL: {name} task died: {reason}` | — |

### Speech & Transcription

| Level | Event | When |
|-------|-------|------|
| `INFO` | `Speech started: item_id=X audio_start_ms=Y` | VAD detects speech |
| `INFO` | `Speech stopped: item_id=X audio_end_ms=Y` | VAD detects silence |
| `INFO` | `Realtime transcription: {text} [item_id=X]` | API returns transcription |
| `INFO` | `Transcription RTT: {ms}ms for {item_id}` | RTT measured (item_created -> completed) |
| `INFO` | `Transcript output: {text}` | Final filtered text sent to typer |

### Fallback (Whisper API)

| Level | Event | When |
|-------|-------|------|
| `WARN` | `Item {id} timeout after 2.5s, trying fallback` | Realtime API didn't respond |
| `DEBUG` | `Fallback transcribing item {id} with Whisper API` | Starting Whisper request |
| `INFO` | `Fallback transcription success: {text} [item_id=X]` | Whisper returned text |
| `WARN` | `No matching chunks found for fallback` | No audio data available |
| `WARN` | `Whisper API returned {status}` | API error |

### Filtering & Dedup

| Level | Event | When |
|-------|-------|------|
| `DEBUG` | `Filtered out: {text}` | Text rejected by filters |
| `INFO` | `Repetition hallucination detected, dropping: {text}` | Repetition detector fired |
| `INFO` | `Speed hallucination: {wps} words/sec ({n} words in {ms}ms)` | Speech-rate anomaly |
| `DEBUG` | `Fuzzy duplicate ({ratio}): {text}` | Sequence matcher caught duplicate |

### Queue Diagnostics

| Level | Event | When |
|-------|-------|------|
| `WARN` | `Queue blocked on {id} (waiting {s}s, {n} items pending)` | Ordered queue stuck |
| `WARN` | `Queue timeout: skipping item {id}` | 10s timeout exceeded |

### Connection

| Level | Event | When |
|-------|-------|------|
| `WARN` | `Session expired: {message}` | 60-minute session limit |
| `WARN` | `Pong timeout (no pong within {n}s of last ping)` | Keepalive failed |
| `ERROR` | `WebSocket error: {err}` | Connection error |
| `ERROR` | `API error: {code}: {message}` | API-level error |

### Audio Device

| Level | Event | Structured Fields |
|-------|-------|-------------------|
| `WARN` | `Failed to open, trying next` | `device`, `error` |
| `WARN` | `No audio device available, retrying` | `attempt` |
| `WARN` | `Audio stream broken, rebuilding` | `errors`, `device` |
| `INFO` | `Audio stream rebuilt` | `device`, `rebuilds` |
| `ERROR` | `No audio input devices found` | — |

### Typing

| Level | Event | Structured Fields |
|-------|-------|-------------------|
| `DEBUG` | `TYPER` | `window`, `method` |
| `ERROR` | `Typing failed: {e}` | — |
| `ERROR` | `Typing timed out after 5s` | — |

### Metrics & Health

| Level | Event | When |
|-------|-------|------|
| `INFO` | `METRICS [...]` (see above) | Every 60 seconds + shutdown |
| (file) | `ok` / `degraded` / `error` written to `$XDG_RUNTIME_DIR/transcriber_health` | On health state change |

---

## Output Channels (Current)

| Channel | What Goes There | Format |
|---------|----------------|--------|
| **JSONL file** | All `tracing` events at configured level (default: `info`) | JSON lines |
| **Stderr** | `warn`+ from tracing, plus a few `eprintln!` calls | Human-readable |
| **Stdout** | `println!` for user-facing status (`[PARTIAL]`/`[FINAL]`, connection info, startup banner) | Plain text |
| **Health file** | `ok`/`degraded`/`error` | Single word, atomic write |

Note: stdout and stderr output does **not** appear in the JSONL log. When running as a background service, only the JSONL file captures events.

---

## Git Commits That Changed Logging

| Date | Commit | Change |
|------|--------|--------|
| 2025-10-15 | `9d9769a` | Initial: `print()` + `.txt` files |
| 2025-12-05 | `0255661` | Python `logging` module |
| 2025-12-10 | `5ed167a` | First `metrics.py`, periodic 60s logging, `metrics_*.txt` summaries |
| 2025-12-10 | `0bf0305` | `fail_short`/`fail_long` split, `fallback_races` |
| 2025-12-15 | `ebf3a7c` | `--no-log` flag |
| 2026-01-16 | `bae6914` | Logger passed to typer module |
| 2026-02-10 | `17258af` | Removed `metrics_*.txt`, consolidated to periodic JSONL |
| 2026-02-19 | `733eac0` | Rust port — `tracing` crate, actor-based metrics |
| 2026-02-20 | `904af69` | 5m/1h/all sliding windows, RSS memory, `Outcome` enum |
| 2026-02-20 | `5216e8e` | Removed `.txt` files, JSONL-only, added `Transcript output:` logging |
| 2026-02-25 | `356d373` | Health indicator file, 15m window |
| 2026-02-25 | `38e081c` | Event-driven health (last-8 outcomes) |
| 2026-02-26 | `384a0f6` | Connection state in health |
| 2026-03-03 | `47892d4` | JSONL file permissions `0o600` |
| 2026-03-04 | `ff5981e` | Ping RTT + Transcription RTT events |
| 2026-03-04 | `398e401` | RTT percentiles in METRICS line, latency-based health degradation |
| 2026-03-05 | `cccfbc5` | Speech start/stop timing events |
| 2026-03-09 | `e246e66` | Local timestamps, ping demoted to debug, queue timeout logging |
| 2026-03-14 | `8979f7b` | Watchdog `FATAL` logging on task death |
| 2026-03-19 | `39bfb66` | `item_id` added to transcription log messages |
