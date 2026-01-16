"""
Transcript processing, filtering, ordering, and output.

Handles:
- Text filtering (false positives, non-ASCII characters)
- Ordered output of transcripts (ensuring speech order is preserved)
- Fuzzy duplicate detection (prevents double-typing from race conditions)
- Logging to file and terminal
- Keyboard typing output
"""

import re
import time
import threading
import logging
from datetime import datetime
from difflib import SequenceMatcher
from pathlib import Path
from typing import TYPE_CHECKING, Dict, List, Optional, Tuple

import yaml

from transcriber.typer import KeyboardTyper

if TYPE_CHECKING:
    from transcriber.metrics import TranscriptionMetrics


def _load_filters(config_path: Path) -> dict:
    """Load filter patterns from YAML config file."""
    if not config_path.exists():
        return {"hallucinations": [], "fillers": [], "non_english": []}

    with open(config_path) as f:
        return yaml.safe_load(f) or {}


def _compile_filters(filter_list: List[dict]) -> List[Tuple[re.Pattern, str]]:
    """Compile a list of filter definitions into regex patterns.

    Returns list of (compiled_pattern, original_pattern_string) tuples.
    """
    compiled = []
    for item in filter_list:
        pattern = item.get("pattern", "")
        if not pattern:
            continue

        flags = 0
        flag_str = item.get("flags", "")
        if "ignorecase" in flag_str.lower():
            flags |= re.IGNORECASE
        if "multiline" in flag_str.lower():
            flags |= re.MULTILINE
        if "dotall" in flag_str.lower():
            flags |= re.DOTALL

        try:
            compiled.append((re.compile(pattern, flags), pattern))
        except re.error as e:
            logging.warning(f"Invalid filter pattern '{pattern}': {e}")

    return compiled


class TranscriptManager:
    """Manages transcript filtering, ordering, deduplication, and output."""

    def __init__(
        self,
        typer: KeyboardTyper,
        log_file: Path,
        logger: logging.Logger,
        allow_bye_thank_you: bool = False,
        allow_non_ascii: bool = False,
        allow_fillers: bool = False,
        metrics: Optional["TranscriptionMetrics"] = None,
        filters_config: Optional[Path] = None,
    ):
        self.typer = typer
        self.log_file = log_file
        self.logger = logger
        self.allow_bye_thank_you = allow_bye_thank_you
        self.allow_non_ascii = allow_non_ascii
        self.allow_fillers = allow_fillers
        self.metrics = metrics

        # Load and compile filters from config (reloaded dynamically when file changes)
        if filters_config is None:
            filters_config = Path(__file__).parent / "filters.yaml"
        self._filters_config = filters_config
        self._filters_mtime: float = 0
        self._reload_filters()

        # Ordering system to ensure transcriptions appear in the order they were spoken
        self.item_order: List[str] = []
        self.completed_transcripts: Dict[str, str] = {}
        self.next_output_index = 0
        self.recent_transcripts: List[
            tuple
        ] = []  # (timestamp, text) for duplicate detection
        self.output_lock = threading.Lock()

        # Track completed items (shared with audio buffer for race condition prevention)
        self.item_speech_times: Dict[str, dict] = {}

        # Current transcript for session
        self.current_transcript: List[str] = []

    def set_item_speech_times(self, item_speech_times: Dict[str, dict]):
        """Set reference to shared item_speech_times dict from audio buffer."""
        self.item_speech_times = item_speech_times

    def _reload_filters(self):
        """Reload filters from config file if modified (allows live editing)."""
        try:
            if not self._filters_config.exists():
                return

            mtime = self._filters_config.stat().st_mtime
            if mtime == self._filters_mtime:
                return

            filters = _load_filters(self._filters_config)
            self._hallucination_filters = _compile_filters(
                filters.get("hallucinations", [])
            )
            self._filler_filters = _compile_filters(filters.get("fillers", []))
            self._non_ascii_filters = _compile_filters(filters.get("non_ascii", []))

            if self._filters_mtime > 0:
                self.logger.info(f'"Reloaded filters from {self._filters_config}"')
            self._filters_mtime = mtime
        except Exception as e:
            self.logger.warning(f'"Failed to reload filters: {e}"')

    def filter_text(self, text: str) -> str:
        """
        Filter text based on configured options and filters.yaml patterns.

        By default:
        - Filters out hallucinations (common false positives from background noise)
        - Filters out filler words (um, uh, hmm, etc.)
        - Filters out non-ASCII characters
        """
        if not text:
            return text

        # Reload filters if the YAML config was modified
        self._reload_filters()

        # Apply hallucination filters
        if not self.allow_bye_thank_you:
            for pattern, _ in self._hallucination_filters:
                text = pattern.sub("", text)

        # Apply filler filters
        if not self.allow_fillers:
            for pattern, _ in self._filler_filters:
                text = pattern.sub("", text)

        # Apply non-ASCII filters
        if not self.allow_non_ascii:
            for pattern, _ in self._non_ascii_filters:
                text = pattern.sub("", text)

        # Clean up multiple spaces
        text = re.sub(r"\s+", " ", text)

        return text.strip()

    def log_transcript(self, text: str, partial: bool = False):
        """Log transcript to file and display in terminal."""
        if not text.strip():
            return

        # Display in terminal
        prefix = "[PARTIAL] " if partial else "[FINAL]   "
        print(f"{prefix}{text}", flush=True)

        # Log to file (only final transcripts, skip if no_log mode)
        if not partial:
            if self.log_file is not None:
                timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
                with open(self.log_file, "a", encoding="utf-8") as f:
                    f.write(f"[{timestamp}] {text}\n")
                    f.flush()

            self.current_transcript.append(text)

    def type_text(self, text: str):
        """Type text using the robust keyboard typer."""
        if not text.strip():
            return

        try:
            success = self.typer.type_text(text)
            if not success:
                self.logger.info(f'"Text not typed but saved to: {self.log_file}"')
        except Exception:
            self.logger.exception(f'"Failed to type text, saved to: {self.log_file}"')

    def track_item_creation(self, item_id: str):
        """Track when a conversation item is created to maintain order."""
        if item_id and item_id not in self.item_order:
            self.item_order.append(item_id)

    def handle_completed_transcript(self, item_id: str, transcript: str):
        """Handle a completed transcript, buffering it and outputting in order."""
        with self.output_lock:
            # Check if already completed (race between fallback and realtime API)
            if item_id and item_id in self.item_speech_times:
                if self.item_speech_times[item_id].get("completed"):
                    self.logger.debug(
                        f'"Skipping already-completed item {item_id[:20]}"'
                    )
                    if self.metrics:
                        self.metrics.record_fallback_race()
                    return
                self.item_speech_times[item_id]["completed"] = True

            if not item_id:
                self._output_transcript(transcript)
                return

            self.completed_transcripts[item_id] = transcript
            self._flush_ordered_transcripts()

    def _flush_ordered_transcripts(self):
        """Output completed transcripts in the order items were created.

        Note: Caller must hold self.output_lock.
        """
        while self.next_output_index < len(self.item_order):
            next_item_id = self.item_order[self.next_output_index]

            if next_item_id in self.completed_transcripts:
                transcript = self.completed_transcripts.pop(next_item_id)
                self._output_transcript(transcript)
                self.next_output_index += 1
            else:
                break

    def _is_fuzzy_duplicate(
        self,
        text: str,
        threshold: float = 0.85,
        max_age_seconds: float = 7.0,
        max_count: int = 7,
    ) -> bool:
        """Check if text is a fuzzy duplicate of a recent transcript.

        Only considers transcripts within the last max_age_seconds AND
        within the last max_count transcripts.
        """
        now = time.time()

        for i, (timestamp, previous) in enumerate(reversed(self.recent_transcripts)):
            if i >= max_count:
                break
            if now - timestamp > max_age_seconds:
                break
            ratio = SequenceMatcher(None, text, previous).ratio()
            if ratio >= threshold:
                self.logger.debug(f'"Fuzzy duplicate ({ratio:.2f}): {text}"')
                if self.metrics:
                    self.metrics.record_duplicate_filtered()
                return True
        return False

    def _output_transcript(self, transcript: str):
        """Output a transcript (log and type) after filtering."""
        filtered_transcript = self.filter_text(transcript)

        if filtered_transcript and self._is_fuzzy_duplicate(filtered_transcript):
            return

        if filtered_transcript:
            self.recent_transcripts.append((time.time(), filtered_transcript))
            if len(self.recent_transcripts) > 14:
                self.recent_transcripts = self.recent_transcripts[-7:]

            self.log_transcript(filtered_transcript, partial=False)
            self.type_text(filtered_transcript)
        elif transcript and transcript != filtered_transcript:
            self.logger.debug(f'"Filtered out: {transcript}"')
            if self.metrics:
                self.metrics.record_content_filtered()

    def reset(self):
        """Reset state for a new session while preserving log file."""
        self.item_order = []
        self.completed_transcripts = {}
        self.next_output_index = 0
        # Keep recent_transcripts to prevent duplicates across reconnections
        self.current_transcript = []
