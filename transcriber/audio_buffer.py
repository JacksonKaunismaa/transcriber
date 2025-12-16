"""
Audio buffering and fallback transcription via Whisper API.

Handles:
- Buffering audio chunks with timestamps as they're sent to the realtime API
- Tracking speech start/stop times for each conversation item
- Timeout detection when the realtime API doesn't respond
- Extracting audio segments and transcribing via Whisper API as fallback
"""

import io
import time
import wave
import logging
import threading
from typing import TYPE_CHECKING, Dict, List, Optional, Tuple, Callable

from openai import OpenAI

if TYPE_CHECKING:
    from transcriber.metrics import TranscriptionMetrics


class AudioBuffer:
    """Buffers audio and provides fallback transcription when realtime API times out."""

    def __init__(self, openai_client: OpenAI, logger: logging.Logger,
                 on_transcript_complete: Callable[[str, str], None],
                 timeout_seconds: float = 2.5,
                 timestamp_margin_ms: int = 200,
                 min_duration_ms: int = 300,
                 metrics: Optional["TranscriptionMetrics"] = None):
        """
        Args:
            openai_client: OpenAI client for Whisper API calls
            logger: Logger instance
            on_transcript_complete: Callback(item_id, transcript) when fallback succeeds
            timeout_seconds: Seconds to wait before triggering fallback
            timestamp_margin_ms: Margin for timestamp matching
            min_duration_ms: Minimum segment duration to transcribe
            metrics: Optional metrics tracker
        """
        self.openai_client = openai_client
        self.logger = logger
        self.on_transcript_complete = on_transcript_complete
        self.timeout_seconds = timeout_seconds
        self.timestamp_margin_ms = timestamp_margin_ms
        self.min_duration_ms = min_duration_ms
        self.metrics = metrics

        # Audio buffer: list of (timestamp_ms, audio_chunk)
        self.audio_buffer: List[Tuple[int, bytes]] = []

        # Speech timing: item_id -> {start_ms, end_ms, stopped_at, completed}
        self.item_speech_times: Dict[str, dict] = {}

        # Session timing
        self.session_start_time: Optional[float] = None

        # Control flags
        self.running = False
        self.timeout_check_thread: Optional[threading.Thread] = None

    def start(self):
        """Start the timeout checking thread."""
        self.running = True
        self.session_start_time = time.time()
        self.timeout_check_thread = threading.Thread(target=self._check_timeouts)
        self.timeout_check_thread.daemon = True
        self.timeout_check_thread.start()

    def stop(self):
        """Stop the timeout checking thread and wait for it to finish."""
        self.running = False
        if self.timeout_check_thread and self.timeout_check_thread.is_alive():
            self.timeout_check_thread.join(timeout=2.0)
        self.timeout_check_thread = None

    def add_audio_chunk(self, chunk: bytes):
        """Add an audio chunk to the buffer with current timestamp."""
        if self.session_start_time:
            timestamp_ms = int((time.time() - self.session_start_time) * 1000)
            self.audio_buffer.append((timestamp_ms, chunk))

    def record_speech_started(self, item_id: str, audio_start_ms: int):
        """Record when speech started for an item."""
        self.item_speech_times[item_id] = {
            "start_ms": audio_start_ms,
            "completed": False
        }

    def record_speech_stopped(self, item_id: str, audio_end_ms: int):
        """Record when speech stopped for an item."""
        if item_id in self.item_speech_times:
            self.item_speech_times[item_id]["end_ms"] = audio_end_ms
            self.item_speech_times[item_id]["stopped_at"] = time.time()

    def mark_completed(self, item_id: str):
        """Mark an item as completed (transcription received from realtime API)."""
        if item_id in self.item_speech_times:
            self.item_speech_times[item_id]["completed"] = True

    def reset(self):
        """Reset buffer state for a new session."""
        self.session_start_time = None
        self.audio_buffer = []
        self.item_speech_times = {}

    def _check_timeouts(self):
        """Check for items that have timed out and need fallback transcription."""
        while self.running:
            time.sleep(1)

            if not self.session_start_time:
                continue

            current_time = time.time()

            for item_id, times in list(self.item_speech_times.items()):
                if times.get("completed") or "stopped_at" not in times:
                    continue

                time_since_stopped = current_time - times["stopped_at"]
                if time_since_stopped >= self.timeout_seconds:
                    self.logger.warning(f'"Item {item_id[:20]} timeout after {self.timeout_seconds}s, trying fallback"')
                    if self.metrics:
                        self.metrics.record_timeout()

                    # Calculate segment duration for metrics
                    duration_ms = 0
                    if "start_ms" in times and "end_ms" in times:
                        duration_ms = times["end_ms"] - times["start_ms"]

                    transcript = self._fallback_transcribe(item_id)

                    if transcript:
                        if self.metrics:
                            self.metrics.record_fallback_success()
                        self.on_transcript_complete(item_id, transcript)
                    else:
                        if self.metrics:
                            self.metrics.record_fallback_failure(duration_ms)
                        self.logger.warning(f'"Skipping item {item_id[:20]} - fallback failed ({duration_ms}ms)"')
                        self.on_transcript_complete(item_id, "")

    def _find_best_chunk_match(self, start_ms: int, end_ms: int) -> Tuple[Optional[List[bytes]], float, Optional[int]]:
        """
        Find best matching audio chunks for a time range.

        Tries different timestamp offsets to handle VAD timing uncertainty,
        and picks the match with the best duration alignment.
        """
        expected_duration_ms = end_ms - start_ms
        best_chunks = None
        best_duration_error = float('inf')
        best_offset = None

        for offset_ms in range(-self.timestamp_margin_ms, self.timestamp_margin_ms + 1, 20):
            test_start = start_ms + offset_ms
            test_end = end_ms + offset_ms

            candidate_chunks = [
                chunk for ts, chunk in self.audio_buffer
                if test_start <= ts <= test_end
            ]

            if candidate_chunks:
                actual_duration_ms = len(candidate_chunks) * (1024 / 24000 * 1000)
                duration_error = abs(expected_duration_ms - actual_duration_ms)

                if duration_error < best_duration_error:
                    best_duration_error = duration_error
                    best_chunks = candidate_chunks
                    best_offset = offset_ms

        return best_chunks, best_duration_error, best_offset

    def _extract_audio_chunks(self, item_id: str) -> Optional[bytes]:
        """Extract audio chunks for a specific item using best timestamp match."""
        if item_id not in self.item_speech_times:
            return None

        times = self.item_speech_times[item_id]
        if "start_ms" not in times or "end_ms" not in times:
            return None

        start_ms = times["start_ms"]
        end_ms = times["end_ms"]
        expected_duration_ms = end_ms - start_ms

        if expected_duration_ms < self.min_duration_ms:
            self.logger.debug(f'"Skipping short segment ({expected_duration_ms}ms) for item {item_id[:20]}"')
            if self.metrics:
                self.metrics.record_short_segment_skipped()
            return None

        best_chunks, duration_error, offset = self._find_best_chunk_match(start_ms, end_ms)

        if not best_chunks:
            self.logger.warning(f'"No matching chunks found for item {item_id[:20]}"')
            return None

        actual_duration_ms = len(best_chunks) * (1024 / 24000 * 1000)
        self.logger.debug(f'"Fallback extracted {len(best_chunks)} chunks (offset {offset}ms), duration error: {duration_error:.0f}ms"')

        if duration_error > 500:
            self.logger.warning(f'"Large duration mismatch: expected {expected_duration_ms}ms, got {actual_duration_ms:.0f}ms"')

        return b''.join(best_chunks)

    def _fallback_transcribe(self, item_id: str) -> Optional[str]:
        """Transcribe audio using OpenAI Whisper API as fallback."""
        try:
            audio_data = self._extract_audio_chunks(item_id)
            if not audio_data:
                return None

            wav_buffer = io.BytesIO()
            with wave.open(wav_buffer, 'wb') as wav_file:
                wav_file.setnchannels(1)
                wav_file.setsampwidth(2)
                wav_file.setframerate(24000)
                wav_file.writeframes(audio_data)

            wav_buffer.seek(0)
            wav_buffer.name = "audio.wav"

            self.logger.debug(f'"Fallback transcribing item {item_id[:20]} with Whisper API"')
            transcription = self.openai_client.audio.transcriptions.create(
                model="whisper-1",
                file=wav_buffer
            )

            transcript = transcription.text
            self.logger.info(f'"Fallback transcription success: {transcript}"')
            return transcript

        except Exception as e:
            self.logger.exception(f'"Fallback transcription failed for item {item_id[:20]}"')
            return None
