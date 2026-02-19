use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::messages::MetricsEvent;

/// Metrics counters, owned exclusively by the metrics task.
/// No locks needed — only this task reads/writes these.
struct Metrics {
    connection_attempts: u64,
    connection_successes: u64,
    session_expirations: u64,
    reconnection_attempts: u64,
    audio_chunks_sent: u64,
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
    start_time: std::time::Instant,
}

impl Metrics {
    fn new() -> Self {
        Metrics {
            connection_attempts: 0,
            connection_successes: 0,
            session_expirations: 0,
            reconnection_attempts: 0,
            audio_chunks_sent: 0,
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
            start_time: std::time::Instant::now(),
        }
    }

    fn apply(&mut self, event: MetricsEvent) {
        match event {
            MetricsEvent::ConnectionAttempt => self.connection_attempts += 1,
            MetricsEvent::ConnectionSuccess => self.connection_successes += 1,
            MetricsEvent::SessionExpiration => self.session_expirations += 1,
            MetricsEvent::ReconnectionAttempt => self.reconnection_attempts += 1,
            MetricsEvent::AudioChunkSent => self.audio_chunks_sent += 1,
            MetricsEvent::RealtimeTranscription => self.realtime_transcriptions += 1,
            MetricsEvent::Timeout => self.timeouts += 1,
            MetricsEvent::FallbackSuccess => self.fallback_successes += 1,
            MetricsEvent::FallbackFailure { duration_ms } => {
                if duration_ms >= 1000 {
                    self.fallback_failures_long += 1;
                } else {
                    self.fallback_failures_short += 1;
                }
            }
            MetricsEvent::FallbackRace => self.fallback_races += 1,
            MetricsEvent::ShortSegmentSkipped => self.short_segments_skipped += 1,
            MetricsEvent::DuplicateFiltered => self.duplicates_filtered += 1,
            MetricsEvent::ContentFiltered => self.content_filtered += 1,
            MetricsEvent::WebSocketError => self.websocket_errors += 1,
            MetricsEvent::ApiError => self.api_errors += 1,
        }
    }

    fn log_stats(&self) {
        let minutes = self.start_time.elapsed().as_secs() / 60;
        let total_attempts = self.realtime_transcriptions + self.timeouts;
        let timeout_pct = if total_attempts > 0 {
            (100 * self.timeouts) as f64 / total_attempts as f64
        } else {
            0.0
        };

        let rss_mb = get_rss_mb();

        let stats = format!(
            "METRICS [{minutes}m] | \
             rss:{rss_mb:.0}MB | \
             realtime:{rt} timeouts:{to} ({tp:.1}%) \
             fallback_ok:{fok} fail_short:{fs} fail_long:{fl} races:{fr} | \
             filtered:{cf} dupes:{df} short_skipped:{ss} | \
             conn:{cs}/{ca} expires:{se} reconnects:{ra} | \
             errors: ws={we} api={ae}",
            rt = self.realtime_transcriptions,
            to = self.timeouts,
            tp = timeout_pct,
            fok = self.fallback_successes,
            fs = self.fallback_failures_short,
            fl = self.fallback_failures_long,
            fr = self.fallback_races,
            cf = self.content_filtered,
            df = self.duplicates_filtered,
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
    let mut log_interval = tokio::time::interval(std::time::Duration::from_secs(60));
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
