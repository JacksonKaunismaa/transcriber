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
        }
    }

    fn apply(&mut self, event: MetricsEvent) {
        match event {
            MetricsEvent::ConnectionAttempt => self.connection_attempts += 1,
            MetricsEvent::ConnectionSuccess => self.connection_successes += 1,
            MetricsEvent::SessionExpiration => self.session_expirations += 1,
            MetricsEvent::ReconnectionAttempt => self.reconnection_attempts += 1,
            MetricsEvent::AudioChunkSent => {} // not tracked
            MetricsEvent::RealtimeTranscription => {
                self.realtime_transcriptions += 1;
                self.recent.push(Outcome::Realtime);
            }
            MetricsEvent::Timeout => {
                self.timeouts += 1;
                self.recent.push(Outcome::Timeout);
            }
            MetricsEvent::FallbackSuccess => {
                self.fallback_successes += 1;
                self.recent.push(Outcome::FallbackOk);
            }
            MetricsEvent::FallbackFailure { duration_ms } => {
                if duration_ms >= 1000 {
                    self.fallback_failures_long += 1;
                } else {
                    self.fallback_failures_short += 1;
                }
                self.recent.push(Outcome::FallbackFail);
            }
            MetricsEvent::FallbackRace => self.fallback_races += 1,
            MetricsEvent::ShortSegmentSkipped => self.short_segments_skipped += 1,
            MetricsEvent::DuplicateFiltered => self.duplicates_filtered += 1,
            MetricsEvent::ContentFiltered => {
                self.content_filtered += 1;
                self.recent.push(Outcome::Filtered);
            }
            MetricsEvent::WebSocketError => self.websocket_errors += 1,
            MetricsEvent::ApiError => self.api_errors += 1,
        }
    }

    fn log_stats(&mut self) {
        let minutes = self.start_time.elapsed().as_secs() / 60;
        let rss_mb = get_rss_mb();

        // Prune entries older than 1 hour
        self.recent.prune(Duration::from_secs(60 * 60));

        let w5m = self.recent.counts(Duration::from_secs(5 * 60));
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
            "METRICS [{minutes}m] | rss:{rss_mb:.0}MB | \
             5m: {w5m} | 1h: {w1h} | all: {wall} | \
             races:{races} dupes:{dupes} short_skip:{ss} | \
             conn:{cs}/{ca} expires:{se} reconnects:{ra} | \
             errors: ws={we} api={ae}",
            w5m = w5m.format(),
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
}

/// Run the metrics task. Receives MetricsEvent messages and logs periodically.
pub async fn run_metrics_task(
    mut rx: mpsc::Receiver<MetricsEvent>,
    cancel: CancellationToken,
    _debug_log_file: Option<PathBuf>,
) {
    let mut metrics = Metrics::new();
    let mut log_interval = tokio::time::interval(Duration::from_secs(60));
    log_interval.tick().await; // Consume initial tick

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                metrics.log_stats(); // Final stats
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
