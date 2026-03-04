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
    Timeout,
    FallbackOk,
    FallbackFail,
    Filtered,
}

/// Aggregated counts for a time window.
struct WindowCounts {
    realtime: u64,
    timeouts: u64,
    fallback_ok: u64,
    fallback_fail: u64,
    filtered: u64,
}

impl WindowCounts {
    /// Format as a compact string: `rt:12 to:3 (80%) fb:1/4 filt:0`
    fn format(&self) -> String {
        let total = self.realtime + self.timeouts;
        let ok_pct = if total > 0 {
            (100 * self.realtime) as f64 / total as f64
        } else {
            0.0
        };
        let fb_total = self.fallback_ok + self.fallback_fail;
        format!(
            "rt:{} to:{} ({:.0}%) fb:{}/{} filt:{}",
            self.realtime, self.timeouts, ok_pct, self.fallback_ok, fb_total, self.filtered,
        )
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

    /// Count outcomes from the last `n` events (most recent first).
    fn last_n(&self, n: usize) -> WindowCounts {
        let mut c = WindowCounts {
            realtime: 0,
            timeouts: 0,
            fallback_ok: 0,
            fallback_fail: 0,
            filtered: 0,
        };
        for (_, outcome) in self.events.iter().rev().take(n) {
            match outcome {
                Outcome::Realtime => c.realtime += 1,
                Outcome::Timeout => c.timeouts += 1,
                Outcome::FallbackOk => c.fallback_ok += 1,
                Outcome::FallbackFail => c.fallback_fail += 1,
                Outcome::Filtered => c.filtered += 1,
            }
        }
        c
    }

    /// Count outcomes within the last `window` duration.
    fn counts(&self, window: Duration) -> WindowCounts {
        let cutoff = Instant::now() - window;
        let mut c = WindowCounts {
            realtime: 0,
            timeouts: 0,
            fallback_ok: 0,
            fallback_fail: 0,
            filtered: 0,
        };
        for (t, outcome) in self.events.iter().rev() {
            if *t < cutoff {
                break;
            }
            match outcome {
                Outcome::Realtime => c.realtime += 1,
                Outcome::Timeout => c.timeouts += 1,
                Outcome::FallbackOk => c.fallback_ok += 1,
                Outcome::FallbackFail => c.fallback_fail += 1,
                Outcome::Filtered => c.filtered += 1,
            }
        }
        c
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
    timeouts: u64,
    fallback_successes: u64,
    fallback_failures_short: u64,
    fallback_failures_long: u64,
    fallback_races: u64,
    short_segments_skipped: u64,
    duplicates_filtered: u64,
    content_filtered: u64,
    websocket_errors: u64,
    api_errors: u64,
    start_time: Instant,
    recent: RecentWindow,
    last_health: &'static str,
    connected: bool,
    ping_rtts: VecDeque<u64>,
    transcription_rtts: VecDeque<u64>,
}

impl Metrics {
    fn new() -> Self {
        Metrics {
            connection_attempts: 0,
            connection_successes: 0,
            session_expirations: 0,
            reconnection_attempts: 0,
            realtime_transcriptions: 0,
            timeouts: 0,
            fallback_successes: 0,
            fallback_failures_short: 0,
            fallback_failures_long: 0,
            fallback_races: 0,
            short_segments_skipped: 0,
            duplicates_filtered: 0,
            content_filtered: 0,
            websocket_errors: 0,
            api_errors: 0,
            start_time: Instant::now(),
            recent: RecentWindow::new(),
            last_health: "ok",
            connected: false,
            ping_rtts: VecDeque::new(),
            transcription_rtts: VecDeque::new(),
        }
    }

    fn apply(&mut self, event: MetricsEvent) {
        match event {
            MetricsEvent::ConnectionAttempt => self.connection_attempts += 1,
            MetricsEvent::ConnectionSuccess => {
                self.connection_successes += 1;
                self.connected = true;
                self.update_health();
            }
            MetricsEvent::SessionExpiration => self.session_expirations += 1,
            MetricsEvent::ReconnectionAttempt => {
                self.reconnection_attempts += 1;
                self.connected = false;
                self.update_health();
            }
            MetricsEvent::AudioChunkSent => {} // not tracked
            MetricsEvent::RealtimeTranscription => {
                self.realtime_transcriptions += 1;
                self.recent.push(Outcome::Realtime);
                self.update_health();
            }
            MetricsEvent::Timeout => {
                self.timeouts += 1;
                self.recent.push(Outcome::Timeout);
                self.update_health();
            }
            MetricsEvent::FallbackSuccess => {
                self.fallback_successes += 1;
                self.recent.push(Outcome::FallbackOk);
                self.update_health();
            }
            MetricsEvent::FallbackFailure { duration_ms } => {
                if duration_ms >= 1000 {
                    self.fallback_failures_long += 1;
                } else {
                    self.fallback_failures_short += 1;
                }
                self.recent.push(Outcome::FallbackFail);
                self.update_health();
            }
            MetricsEvent::FallbackRace => self.fallback_races += 1,
            MetricsEvent::ShortSegmentSkipped => self.short_segments_skipped += 1,
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
        }
    }

    fn log_stats(&mut self) {
        let minutes = self.start_time.elapsed().as_secs() / 60;
        let rss_mb = get_rss_mb();
        let ping_stats = percentiles(&self.ping_rtts)
            .map(|(p50, p95)| format!("ping p50:{p50}ms p95:{p95}ms"))
            .unwrap_or_else(|| "ping -".to_string());
        let rtt_stats = percentiles(&self.transcription_rtts)
            .map(|(p50, p95)| format!("rtt p50:{p50}ms p95:{p95}ms"))
            .unwrap_or_else(|| "rtt -".to_string());

        // Prune entries older than 1 hour
        self.recent.prune(Duration::from_secs(60 * 60));

        let w5m = self.recent.counts(Duration::from_secs(5 * 60));
        let w15m = self.recent.counts(Duration::from_secs(15 * 60));
        let w1h = self.recent.counts(Duration::from_secs(60 * 60));

        // All-time counts reuse the cumulative counters
        let wall = WindowCounts {
            realtime: self.realtime_transcriptions,
            timeouts: self.timeouts,
            fallback_ok: self.fallback_successes,
            fallback_fail: self.fallback_failures_short + self.fallback_failures_long,
            filtered: self.content_filtered,
        };

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

        info!("{stats}");
    }

    fn update_health(&mut self) {
        let health = if !self.connected {
            "error"
        } else {
            let recent = self.recent.last_n(8);
            let successes = recent.realtime + recent.fallback_ok;
            let failures = recent.fallback_fail;
            let total = successes + failures;
            if total < 3 {
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
    _debug_log_file: Option<PathBuf>,
) {
    let mut metrics = Metrics::new();
    // Write initial health state so we don't inherit stale status from a previous run
    write_health_file("ok");
    let mut log_interval = tokio::time::interval(Duration::from_secs(60));
    log_interval.tick().await; // Consume initial tick

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
            event = rx.recv() => {
                match event {
                    Some(event) => metrics.apply(event),
                    None => break,
                }
            }
        }
    }
}

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
