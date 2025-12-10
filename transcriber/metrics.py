"""
Metrics tracking for transcription sessions.

Tracks:
- Connection attempts, successes, and failures
- Reconnections and session expirations
- Audio chunks sent
- Transcription outcomes (realtime vs fallback)
- Timeouts, failures, and duplicates

Note: Metrics are written to file, not terminal. This app runs as a background
service so terminal output is not visible.
"""

import logging
import threading
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Dict, Optional


@dataclass
class TranscriptionMetrics:
    """Tracks metrics for a transcription session."""

    # Connection metrics
    connection_attempts: int = 0
    connection_successes: int = 0
    session_expirations: int = 0
    reconnection_attempts: int = 0

    # Audio metrics
    audio_chunks_sent: int = 0

    # Transcription metrics
    realtime_transcriptions: int = 0  # Completed via realtime API
    timeouts: int = 0  # Items that timed out
    fallback_successes: int = 0  # Fallback transcriptions that worked
    fallback_failures_short: int = 0  # Fallback failed, segment < 1s (likely noise)
    fallback_failures_long: int = 0  # Fallback failed, segment >= 1s (real failure)
    fallback_races: int = 0  # Realtime API returned after fallback already started
    short_segments_skipped: int = 0  # Too short to transcribe

    # Filtering metrics
    duplicates_filtered: int = 0  # Fuzzy duplicates detected
    content_filtered: int = 0  # Filtered by content rules (bye/thank you/fillers)

    # Error metrics
    websocket_errors: int = 0
    api_errors: int = 0

    # Timing
    session_start_time: Optional[float] = None
    _lock: threading.Lock = field(default_factory=threading.Lock)

    # Periodic logging
    _logger: Optional[logging.Logger] = field(default=None, repr=False)
    _log_thread: Optional[threading.Thread] = field(default=None, repr=False)
    _running: bool = field(default=False, repr=False)
    _log_interval: int = 60  # seconds

    def start_session(self, logger: Optional[logging.Logger] = None):
        """Mark session start time and start periodic logging."""
        self.session_start_time = time.time()
        self._logger = logger
        if logger:
            self._running = True
            self._log_thread = threading.Thread(target=self._periodic_log, daemon=True)
            self._log_thread.start()

    def stop(self):
        """Stop periodic logging."""
        self._running = False

    def _periodic_log(self):
        """Log metrics summary every interval."""
        while self._running:
            time.sleep(self._log_interval)
            if self._running and self._logger:
                self._log_current_stats()

    def _log_current_stats(self):
        """Log current metrics to the logger."""
        with self._lock:
            duration = self.get_session_duration()
            minutes = int(duration // 60)
            total_attempts = self.realtime_transcriptions + self.timeouts
            timeout_pct = round(100 * self.timeouts / total_attempts, 1) if total_attempts > 0 else 0

            stats = (
                f"METRICS [{minutes}m] | "
                f"realtime:{self.realtime_transcriptions} "
                f"timeouts:{self.timeouts} ({timeout_pct}%) "
                f"fallback_ok:{self.fallback_successes} fail_short:{self.fallback_failures_short} fail_long:{self.fallback_failures_long} races:{self.fallback_races} | "
                f"filtered:{self.content_filtered} dupes:{self.duplicates_filtered} | "
                f"errors: ws={self.websocket_errors} api={self.api_errors}"
            )
            self._logger.info(f'"{stats}"')

    def get_session_duration(self) -> float:
        """Get session duration in seconds."""
        if self.session_start_time is None:
            return 0.0
        return time.time() - self.session_start_time

    def record_connection_attempt(self):
        """Record a connection attempt."""
        with self._lock:
            self.connection_attempts += 1

    def record_connection_success(self):
        """Record a successful connection."""
        with self._lock:
            self.connection_successes += 1

    def record_session_expiration(self):
        """Record a session expiration."""
        with self._lock:
            self.session_expirations += 1

    def record_reconnection_attempt(self):
        """Record a reconnection attempt."""
        with self._lock:
            self.reconnection_attempts += 1

    def record_audio_chunk_sent(self):
        """Record an audio chunk being sent."""
        with self._lock:
            self.audio_chunks_sent += 1

    def record_realtime_transcription(self):
        """Record a successful realtime API transcription."""
        with self._lock:
            self.realtime_transcriptions += 1

    def record_timeout(self):
        """Record a transcription timeout."""
        with self._lock:
            self.timeouts += 1

    def record_fallback_success(self):
        """Record a successful fallback transcription."""
        with self._lock:
            self.fallback_successes += 1

    def record_fallback_failure(self, duration_ms: int = 0):
        """Record a failed fallback transcription, categorized by duration."""
        with self._lock:
            if duration_ms >= 1000:
                self.fallback_failures_long += 1
            else:
                self.fallback_failures_short += 1

    def record_fallback_race(self):
        """Record when realtime API returned after fallback already started."""
        with self._lock:
            self.fallback_races += 1

    def record_short_segment_skipped(self):
        """Record a segment skipped due to short duration."""
        with self._lock:
            self.short_segments_skipped += 1

    def record_duplicate_filtered(self):
        """Record a duplicate being filtered out."""
        with self._lock:
            self.duplicates_filtered += 1

    def record_content_filtered(self):
        """Record content being filtered out."""
        with self._lock:
            self.content_filtered += 1

    def record_websocket_error(self):
        """Record a WebSocket error."""
        with self._lock:
            self.websocket_errors += 1

    def record_api_error(self):
        """Record an API error."""
        with self._lock:
            self.api_errors += 1

    @property
    def total_transcription_attempts(self) -> int:
        """Total items that needed transcription."""
        return self.realtime_transcriptions + self.timeouts

    @property
    def total_successful_transcriptions(self) -> int:
        """Total successful transcriptions (realtime + fallback)."""
        return self.realtime_transcriptions + self.fallback_successes

    def get_summary(self) -> Dict[str, any]:
        """Get a summary dictionary of all metrics."""
        with self._lock:
            total_attempts = self.total_transcription_attempts
            total_success = self.total_successful_transcriptions

            return {
                "session_duration_seconds": round(self.get_session_duration(), 1),

                # Connection
                "connection_attempts": self.connection_attempts,
                "connection_successes": self.connection_successes,
                "session_expirations": self.session_expirations,
                "reconnection_attempts": self.reconnection_attempts,

                # Audio
                "audio_chunks_sent": self.audio_chunks_sent,

                # Transcription
                "realtime_transcriptions": self.realtime_transcriptions,
                "timeouts": self.timeouts,
                "fallback_successes": self.fallback_successes,
                "fallback_failures_short": self.fallback_failures_short,
                "fallback_failures_long": self.fallback_failures_long,
                "fallback_races": self.fallback_races,
                "short_segments_skipped": self.short_segments_skipped,

                # Filtering
                "duplicates_filtered": self.duplicates_filtered,
                "content_filtered": self.content_filtered,

                # Errors
                "websocket_errors": self.websocket_errors,
                "api_errors": self.api_errors,

                # Calculated percentages
                "timeout_rate_pct": round(100 * self.timeouts / total_attempts, 1) if total_attempts > 0 else 0.0,
                "fallback_success_rate_pct": round(100 * self.fallback_successes / self.timeouts, 1) if self.timeouts > 0 else 0.0,
                "overall_success_rate_pct": round(100 * total_success / total_attempts, 1) if total_attempts > 0 else 0.0,
            }

    def write_summary(self, output_dir: Path):
        """Write a formatted summary to a metrics file."""
        summary = self.get_summary()
        duration = summary["session_duration_seconds"]
        minutes = int(duration // 60)
        seconds = int(duration % 60)

        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        metrics_file = output_dir / f"metrics_{timestamp}.txt"

        lines = [
            "=" * 50,
            "TRANSCRIPTION SESSION METRICS",
            "=" * 50,
            "",
            f"Session Duration: {minutes}m {seconds}s",
            "",
            "--- Connection ---",
            f"  Connection attempts:    {summary['connection_attempts']}",
            f"  Successful connections: {summary['connection_successes']}",
            f"  Session expirations:    {summary['session_expirations']}",
            f"  Reconnection attempts:  {summary['reconnection_attempts']}",
            "",
            "--- Transcription ---",
            f"  Realtime API success:   {summary['realtime_transcriptions']}",
            f"  Timeouts (needed fallback): {summary['timeouts']} ({summary['timeout_rate_pct']}%)",
            f"  Fallback successes:     {summary['fallback_successes']}",
            f"  Fallback fail (<1s):    {summary['fallback_failures_short']}",
            f"  Fallback fail (>=1s):   {summary['fallback_failures_long']}",
            f"  Fallback races:         {summary['fallback_races']}",
        ]

        if summary['timeouts'] > 0:
            lines.append(f"  Fallback success rate:  {summary['fallback_success_rate_pct']}%")

        lines.extend([
            f"  Overall success rate:   {summary['overall_success_rate_pct']}%",
            "",
            "--- Filtering ---",
            f"  Short segments skipped: {summary['short_segments_skipped']}",
            f"  Duplicates filtered:    {summary['duplicates_filtered']}",
            f"  Content filtered:       {summary['content_filtered']}",
            "",
            "--- Errors ---",
            f"  WebSocket errors:       {summary['websocket_errors']}",
            f"  API errors:             {summary['api_errors']}",
            "",
            "--- Audio ---",
            f"  Audio chunks sent:      {summary['audio_chunks_sent']}",
            "=" * 50,
        ])

        with open(metrics_file, "w") as f:
            f.write("\n".join(lines) + "\n")

        return metrics_file
