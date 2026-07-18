use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::messages::MetricsEvent;

/// Timestamped transcription outcome for sliding window stats.
#[derive(Clone, Copy)]
enum Outcome {
    Realtime,
    Filtered,
}

/// Aggregated counts for a time window.
struct WindowCounts {
    realtime: u64,
    filtered: u64,
}

impl WindowCounts {
    /// Format as a compact string: `rt:12 filt:0`
    fn format(&self) -> String {
        format!("rt:{} filt:{}", self.realtime, self.filtered)
    }
}

/// Sliding window tracking recent transcription outcomes.
struct RecentWindow {
    events: VecDeque<(Instant, Outcome)>,
}

impl RecentWindow {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
        }
    }

    fn push(&mut self, outcome: Outcome) {
        self.events.push_back((Instant::now(), outcome));
    }

    /// Prune entries older than `max_age` (the largest window we care about).
    fn prune(&mut self, max_age: Duration) {
        let cutoff = Instant::now() - max_age;
        while self.events.front().is_some_and(|(t, _)| *t < cutoff) {
            self.events.pop_front();
        }
    }

    /// Count outcomes within the last `window` duration.
    fn counts(&self, window: Duration) -> WindowCounts {
        let cutoff = Instant::now() - window;
        let mut c = WindowCounts { realtime: 0, filtered: 0 };
        for (t, outcome) in self.events.iter().rev() {
            if *t < cutoff {
                break;
            }
            match outcome {
                Outcome::Realtime => c.realtime += 1,
                Outcome::Filtered => c.filtered += 1,
            }
        }
        c
    }
}

/// Time-decayed exponential moving average.
///
/// Uses a half-life parameter: after `half_life` seconds of sustained new values,
/// the EMA is halfway to the new level. Single outliers barely move it.
/// Formula: alpha = 1 - exp(-dt / half_life), ema = alpha * sample + (1 - alpha) * ema
struct Ema {
    value: f64,
    half_life_secs: f64,
    last_update: Option<Instant>,
}

impl Ema {
    fn new(half_life_secs: f64) -> Self {
        Self {
            value: 0.0,
            half_life_secs,
            last_update: None,
        }
    }

    fn update(&mut self, sample: f64) {
        let now = Instant::now();
        match self.last_update {
            None => self.value = sample,
            Some(last) => {
                let dt = now.duration_since(last).as_secs_f64();
                let alpha = 1.0 - (-dt / self.half_life_secs).exp();
                self.value = alpha * sample + (1.0 - alpha) * self.value;
            }
        }
        self.last_update = Some(now);
    }

    /// Current EMA value, decayed toward zero based on time since last sample.
    /// During silence (no new samples), the value fades out rather than staying
    /// frozen at the last known level.
    fn current(&self) -> f64 {
        match self.last_update {
            None => 0.0,
            Some(last) => {
                let dt = last.elapsed().as_secs_f64();
                let decay = (-dt / self.half_life_secs).exp();
                self.value * decay
            }
        }
    }
}

/// Metrics counters, owned exclusively by the metrics task.
/// No locks needed — only this task reads/writes these.
struct Metrics {
    connection_attempts: u64,
    connection_successes: u64,
    session_expirations: u64,
    reconnection_attempts: u64,
    realtime_transcriptions: u64,
    duplicates_filtered: u64,
    content_filtered: u64,
    websocket_errors: u64,
    api_errors: u64,
    start_time: Instant,
    recent: RecentWindow,
    last_health: &'static str,
    connected: bool,
    ping_rtt_ema: Ema,
    transcription_rtt_ema: Ema,
    /// When the last ping RTT sample arrived. A connection whose writes are
    /// wedged produces no pong samples, so the EMA silently holds its last
    /// (possibly healthy) value — staleness here is itself a degraded signal.
    last_ping_sample_at: Option<Instant>,
    /// Gauge: audio queued router-side behind a slow websocket.
    send_backlog_ms: u64,
    /// Cumulative audio dropped from the router's send queue (backlog cap).
    net_dropped_chunks: u64,
    net_dropped_ms: u64,
    net_dropped_commits: u64,
    /// Cumulative chunks the capture callback failed to hand off.
    capture_dropped_chunks: u64,
}

/// No pong sample for this long while connected means the ping signal is
/// stale (pings every 5s → 3 consecutive lost rounds), i.e. the connection
/// is wedged even if the EMA still holds a healthy-looking value.
const PING_STALE_AFTER: Duration = Duration::from_secs(15);

impl Metrics {
    fn new() -> Self {
        Metrics {
            connection_attempts: 0,
            connection_successes: 0,
            session_expirations: 0,
            reconnection_attempts: 0,
            realtime_transcriptions: 0,
            duplicates_filtered: 0,
            content_filtered: 0,
            websocket_errors: 0,
            api_errors: 0,
            start_time: Instant::now(),
            recent: RecentWindow::new(),
            last_health: "ok",
            connected: false,
            ping_rtt_ema: Ema::new(60.0),
            transcription_rtt_ema: Ema::new(60.0),
            last_ping_sample_at: None,
            send_backlog_ms: 0,
            net_dropped_chunks: 0,
            net_dropped_ms: 0,
            net_dropped_commits: 0,
            capture_dropped_chunks: 0,
        }
    }

    /// Compute current health from connection state and latency signals.
    fn compute_health(&self) -> &'static str {
        if !self.connected {
            "error"
        } else if self.is_latency_degraded() || self.ping_signal_stale() {
            "degraded"
        } else {
            "ok"
        }
    }

    /// True when we're nominally connected but pong samples stopped arriving
    /// — a wedged connection the EMA can't see (it just holds its last value).
    fn ping_signal_stale(&self) -> bool {
        self.connected
            && self
                .last_ping_sample_at
                .is_some_and(|t| t.elapsed() > PING_STALE_AFTER)
    }

    fn apply(&mut self, event: MetricsEvent) {
        match event {
            MetricsEvent::ConnectionAttempt => self.connection_attempts += 1,
            MetricsEvent::ConnectionSuccess => {
                self.connection_successes += 1;
                self.connected = true;
                // Grace-restart the staleness clock — the first pong of a
                // fresh connection takes a few seconds to arrive.
                self.last_ping_sample_at = Some(Instant::now());
                self.update_health();
            }
            MetricsEvent::SessionExpiration => self.session_expirations += 1,
            MetricsEvent::ReconnectionAttempt => {
                self.reconnection_attempts += 1;
                self.connected = false;
                self.update_health();
            }
            MetricsEvent::AudioChunkSent => {} // not tracked
            MetricsEvent::SendBacklog { ms } => {
                self.send_backlog_ms = ms;
            }
            MetricsEvent::AudioDropped { chunks, ms, commits } => {
                self.net_dropped_chunks += chunks;
                self.net_dropped_ms += ms;
                self.net_dropped_commits += commits;
            }
            MetricsEvent::CaptureDropped { chunks } => {
                self.capture_dropped_chunks += chunks;
            }
            MetricsEvent::RealtimeTranscription => {
                self.realtime_transcriptions += 1;
                self.recent.push(Outcome::Realtime);
                self.update_health();
            }
            // Parked variants — kept as no-ops so the events can be sent
            // again without compile errors if the fallback is revived.
            // See messages.rs and INCIDENTS.md.
            MetricsEvent::Timeout
            | MetricsEvent::FallbackSuccess
            | MetricsEvent::FallbackFailure { .. }
            | MetricsEvent::FallbackRace
            | MetricsEvent::ShortSegmentSkipped => {}
            MetricsEvent::DuplicateFiltered => self.duplicates_filtered += 1,
            MetricsEvent::ContentFiltered => {
                self.content_filtered += 1;
                self.recent.push(Outcome::Filtered);
            }
            MetricsEvent::WebSocketError => {
                self.websocket_errors += 1;
                self.connected = false;
                self.update_health();
            }
            MetricsEvent::ApiError => self.api_errors += 1,
            MetricsEvent::PingRtt { millis } => {
                self.ping_rtt_ema.update(millis as f64);
                self.last_ping_sample_at = Some(Instant::now());
                self.update_health();
            }
            MetricsEvent::TranscriptionRtt { millis, .. } => {
                self.transcription_rtt_ema.update(millis as f64);
                self.update_health();
            }
        }
    }

    fn log_stats(&mut self) {
        let minutes = self.start_time.elapsed().as_secs() / 60;
        let rss_mb = get_rss_mb();
        let ping_stats = match self.ping_rtt_ema.last_update {
            Some(_) => format!("ping ema:{:.0}ms", self.ping_rtt_ema.current()),
            None => "ping -".to_string(),
        };
        let rtt_stats = match self.transcription_rtt_ema.last_update {
            Some(_) => format!("rtt ema:{:.0}ms", self.transcription_rtt_ema.current()),
            None => "rtt -".to_string(),
        };

        // Prune entries older than 1 hour
        self.recent.prune(Duration::from_secs(60 * 60));

        let w5m = self.recent.counts(Duration::from_secs(5 * 60));
        let w15m = self.recent.counts(Duration::from_secs(15 * 60));
        let w1h = self.recent.counts(Duration::from_secs(60 * 60));

        // All-time counts reuse the cumulative counters
        let wall = WindowCounts {
            realtime: self.realtime_transcriptions,
            filtered: self.content_filtered,
        };

        // v6 schema: backlog gauge + drop counters appended (2026-07-18,
        // network-backpressure hardening).
        let stats = format!(
            "METRICS [{minutes}m] | rss:{rss_mb:.0}MB | {ping_stats} | {rtt_stats} | \
             5m: {w5m} | 15m: {w15m} | 1h: {w1h} | all: {wall} | \
             dupes:{dupes} | \
             conn:{cs}/{ca} expires:{se} reconnects:{ra} | \
             errors: ws={we} api={ae} | \
             backlog:{backlog}ms dropped: net={nd}({ndms}ms)/{ndc}c mic={cd}",
            backlog = self.send_backlog_ms,
            nd = self.net_dropped_chunks,
            ndms = self.net_dropped_ms,
            ndc = self.net_dropped_commits,
            cd = self.capture_dropped_chunks,
            w5m = w5m.format(),
            w15m = w15m.format(),
            w1h = w1h.format(),
            wall = wall.format(),
            dupes = self.duplicates_filtered,
            cs = self.connection_successes,
            ca = self.connection_attempts,
            se = self.session_expirations,
            ra = self.reconnection_attempts,
            we = self.websocket_errors,
            ae = self.api_errors,
        );

        info!("{stats}");
    }

    fn update_health(&mut self) {
        // With local VAD + manual commits, the realtime path doesn't have a
        // success/failure-rate signal anymore (every commit succeeds or the
        // WS is dead). Health is driven purely by connection state and
        // sustained latency / ping-signal staleness.
        let health = self.compute_health();
        if health != self.last_health {
            self.last_health = health;
            write_health_file(health);
        }
    }

    /// Check if recent latency indicates degradation using ping RTT EMA.
    /// Ping measures pure network round-trip, which is what "degraded" means
    /// from the user's perspective (bad VPN / network connection).
    /// Transcription RTT is NOT used — it's dominated by OpenAI's server-side
    /// processing time (~700ms), not network latency.
    /// With a 60s half-life and pings every 5s (alpha≈0.080), reaching 400ms
    /// from a ~126ms baseline requires ~30s of sustained bad pings.
    fn is_latency_degraded(&self) -> bool {
        self.ping_rtt_ema.current() > 400.0
    }
}

/// Get the health file path: $XDG_RUNTIME_DIR/transcriber_health (or /tmp fallback).
fn health_file_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(dir).join("transcriber_health")
}

/// Write health status atomically (write tmp + rename).
fn write_health_file(health: &str) {
    let path = health_file_path();
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, health).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Run the metrics task. Receives MetricsEvent messages and logs periodically.
pub async fn run_metrics_task(
    mut rx: mpsc::Receiver<MetricsEvent>,
    cancel: CancellationToken,
) {
    let mut metrics = Metrics::new();
    // Write initial health state so we don't inherit stale status from a previous run
    write_health_file("ok");
    let mut log_interval = tokio::time::interval(Duration::from_secs(60));
    log_interval.tick().await; // Consume initial tick

    // Health depends on wall-clock staleness (ping_signal_stale), not just
    // events — a wedged connection stops producing events entirely, so
    // re-evaluate periodically or the health file freezes on a stale "ok".
    let mut health_interval = tokio::time::interval(Duration::from_secs(5));
    health_interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                metrics.log_stats();
                let _ = std::fs::remove_file(health_file_path());
                break;
            }
            _ = log_interval.tick() => {
                metrics.log_stats();
            }
            _ = health_interval.tick() => {
                metrics.update_health();
            }
            event = rx.recv() => {
                match event {
                    Some(event) => metrics.apply(event),
                    None => break,
                }
            }
        }
    }
}

/// Read RSS from /proc/self/status (Linux only).
fn get_rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<f64>().ok())
                .map(|kb| kb / 1024.0)
        })
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 2026-07-18 incident signature: during a wedged connection the
    /// select loop froze, no pong samples arrived, and the EMA held a stale
    /// 94ms reading — health claimed better than reality. Sample staleness
    /// itself must degrade health.
    #[test]
    fn stale_ping_signal_degrades_health_while_connected() {
        let mut m = Metrics::new();
        m.connected = true;
        m.ping_rtt_ema.update(50.0);
        m.last_ping_sample_at = Some(Instant::now() - Duration::from_secs(20));
        assert_eq!(m.compute_health(), "degraded");
    }

    #[test]
    fn fresh_pings_with_low_ema_report_ok() {
        let mut m = Metrics::new();
        m.connected = true;
        m.ping_rtt_ema.update(50.0);
        m.last_ping_sample_at = Some(Instant::now());
        assert_eq!(m.compute_health(), "ok");
    }

    #[test]
    fn sustained_high_ping_ema_degrades_health() {
        let mut m = Metrics::new();
        m.connected = true;
        m.ping_rtt_ema.update(800.0);
        m.last_ping_sample_at = Some(Instant::now());
        assert_eq!(m.compute_health(), "degraded");
    }

    #[test]
    fn disconnected_reports_error_regardless_of_latency() {
        let mut m = Metrics::new();
        m.connected = false;
        assert_eq!(m.compute_health(), "error");
    }
}
