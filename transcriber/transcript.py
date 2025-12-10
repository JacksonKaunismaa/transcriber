"""
Transcript processing, filtering, ordering, and output.

Handles:
- Text filtering (false positives, non-English characters)
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
from typing import TYPE_CHECKING, Dict, List, Optional, Set

from transcriber.typer import KeyboardTyper

if TYPE_CHECKING:
    from transcriber.metrics import TranscriptionMetrics


class TranscriptManager:
    """Manages transcript filtering, ordering, deduplication, and output."""

    def __init__(self, typer: KeyboardTyper, log_file: Path, logger: logging.Logger,
                 allow_bye_thank_you: bool = False, allow_non_english: bool = False,
                 allow_fillers: bool = False,
                 metrics: Optional["TranscriptionMetrics"] = None):
        self.typer = typer
        self.log_file = log_file
        self.logger = logger
        self.allow_bye_thank_you = allow_bye_thank_you
        self.allow_non_english = allow_non_english
        self.allow_fillers = allow_fillers
        self.metrics = metrics

        # Ordering system to ensure transcriptions appear in the order they were spoken
        self.item_order: List[str] = []
        self.completed_transcripts: Dict[str, str] = {}
        self.next_output_index = 0
        self.recent_transcripts: List[tuple] = []  # (timestamp, text) for duplicate detection
        self.output_lock = threading.Lock()

        # Track completed items (shared with audio buffer for race condition prevention)
        self.item_speech_times: Dict[str, dict] = {}

        # Current transcript for session
        self.current_transcript: List[str] = []

    def set_item_speech_times(self, item_speech_times: Dict[str, dict]):
        """Set reference to shared item_speech_times dict from audio buffer."""
        self.item_speech_times = item_speech_times

    def filter_text(self, text: str) -> str:
        """
        Filter text based on configured options.

        By default:
        - Filters out "Bye." and "Thank you." (common false positives)
        - Filters out filler words (um, uh, hmm, etc.)
        - Filters out non-English characters
        """
        if not text:
            return text

        # Filter out common false positive strings
        if not self.allow_bye_thank_you:
            text = re.sub(r'\bBye\.\s*', '', text)
            text = re.sub(r'\bThank you\.\s*', '', text)
            # Hallucinations from background noise
            text = re.sub(r'\bMBC\b\.?\s*', '', text)
            text = re.sub(r'\bAmen\b\.?\s*', '', text, flags=re.IGNORECASE)
            text = re.sub(r'\bHehe\b\.?\s*', '', text, flags=re.IGNORECASE)
            text = re.sub(r'\bphew\b\.?\s*', '', text, flags=re.IGNORECASE)
            text = re.sub(r'\bHuh\b\.?\s*', '', text, flags=re.IGNORECASE)
            text = re.sub(r'\bHmph\b\.?\s*', '', text, flags=re.IGNORECASE)
            text = re.sub(r'\b[Oo]m+\s*[Nn]om+(\s*[Nn]om+)*\b\.?\s*', '', text)  # omnomnom
            # Repeated characters (keyboard noise, etc.)
            text = re.sub(r'\b[Aa]+[Hh]+\b\.?\s*', '', text)  # Ahhh, aaahhhh, etc.
            text = re.sub(r'\b[Aa]+[Rr]{4,}\b\.?\s*', '', text)  # Arrrr, arrrrrr, etc.
            text = re.sub(r'\b([A-Za-z])\1{4,}\b\.?\s*', '', text)  # 5+ repeated chars

        # Filter out filler words/sounds (case-insensitive, with optional trailing punctuation)
        if not self.allow_fillers:
            # Handles variations like "um", "umm", "ummm", "Um...", "Uh,", etc.
            filler_pattern = r'\b(?:u[hm]+|er+m*|hm+|mhm+|uh-huh|mm+|ahem)\b[\.\,\!\?\s]*'
            text = re.sub(filler_pattern, '', text, flags=re.IGNORECASE)
            # "oh" and "ah" only when standalone with punctuation (not part of phrase)
            text = re.sub(r'\b[oa]h+[\.\,\!\?]+\s*', '', text, flags=re.IGNORECASE)

            # Filter out standalone ellipsis or trailing filler artifacts
            text = re.sub(r'^\s*\.{2,}\s*$', '', text)  # Just "..." or ". . ."
            text = re.sub(r'^\s*,\s*', '', text)  # Leading comma

        # Filter out non-English characters
        if not self.allow_non_english:
            # Keep only ASCII printable characters
            text = re.sub(r'[^\x20-\x7E]', '', text)

        # Clean up multiple spaces
        text = re.sub(r'\s+', ' ', text)

        return text.strip()

    def log_transcript(self, text: str, partial: bool = False):
        """Log transcript to file and display in terminal."""
        if not text.strip():
            return

        # Display in terminal
        prefix = "[PARTIAL] " if partial else "[FINAL]   "
        print(f"{prefix}{text}", flush=True)

        # Log to file (only final transcripts)
        if not partial:
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
        except Exception as e:
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
                    self.logger.debug(f'"Skipping already-completed item {item_id[:20]}"')
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

    def _is_fuzzy_duplicate(self, text: str, threshold: float = 0.85,
                           max_age_seconds: float = 7.0, max_count: int = 7) -> bool:
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
