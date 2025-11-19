#!/usr/bin/env python3
"""
Real-time audio transcription with OpenAI API.

Features:
- Captures audio from microphone and sends to OpenAI for transcription
- Types transcribed text into the active window using keyboard automation
- Logs all transcriptions to timestamped files in conversations/
- Displays transcriptions in terminal
- Graceful error handling and recovery
"""

import pyaudio
import websocket
import json
import base64
import os
import sys
import signal
import threading
import time
import subprocess
import shutil
import argparse
import re
import wave
import io
from datetime import datetime
from pathlib import Path
from typing import Optional, Dict, List, Tuple
from dotenv import load_dotenv
from openai import OpenAI

from transcriber.deps import check_system_dependencies
from transcriber.typer import KeyboardTyper
from transcriber.audio_device import open_audio_stream

# Load environment variables
load_dotenv()

# Also check for .secrets file
secrets_path = Path(__file__).parent.parent / ".secrets"
if secrets_path.exists():
    load_dotenv(secrets_path)


class TranscriptionSession:
    """Manages a real-time transcription session."""

    def __init__(self, api_key: str, model: str = "whisper-1",
                 allow_bye_thank_you: bool = False, allow_non_english: bool = False):
        self.api_key = api_key
        self.model = model
        self.ws: Optional[websocket.WebSocketApp] = None
        self.audio = pyaudio.PyAudio()
        self.stream = None
        self.running = False
        self.audio_thread = None

        # Filtering options
        self.allow_bye_thank_you = allow_bye_thank_you
        self.allow_non_english = allow_non_english

        # Initialize robust keyboard typer
        self.typer = KeyboardTyper()
        print(f"[INFO] {self.typer.get_status_message()}")

        # Print setup instructions if needed
        instructions = self.typer.get_setup_instructions()
        if instructions:
            print(f"[WARNING] Keyboard typing not available!")
            print(instructions)

        # Set up conversation logging
        self.conversations_dir = Path("conversations")
        self.conversations_dir.mkdir(exist_ok=True)

        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        self.log_file = self.conversations_dir / f"transcription_{timestamp}.txt"
        self.debug_log_file = self.conversations_dir / f"debug_events_{timestamp}.jsonl"
        self.current_transcript = []

        # Buffer for partial transcripts
        self.transcript_buffer = ""

        # Event sequence tracking
        self.event_counter = 0

        # Ordering system to ensure transcriptions appear in the order they were spoken
        self.item_order = []  # List of item_ids in the order they were created
        self.completed_transcripts = {}  # item_id -> transcript (waiting to be output)
        self.next_output_index = 0  # Next position in item_order to output
        self.output_transcripts = set()  # Track what we've already output to prevent duplicates

        # Audio buffering for fallback transcription
        self.session_start_time = None  # When audio streaming started
        self.audio_buffer: List[Tuple[int, bytes]] = []  # List of (timestamp_ms, audio_chunk)
        self.item_speech_times: Dict[str, dict] = {}  # item_id -> {start_ms, end_ms, stopped_at}

        # Data-driven constants from timestamp test (16 samples):
        # - Max observed timestamp error: 39ms (this is VAD/processing jitter, NOT ping)
        # - Max observed completion delay: ~4s
        # - Ping doesn't affect alignment because both timers start at session begin
        self.timeout_seconds = 5  # Timeout after 5s (covers observed max ~4s + buffer)
        self.timestamp_margin_ms = 200  # Conservative margin for processing jitter
        self.min_duration_ms = 300  # Skip very short segments (likely noise)

        self.timeout_check_thread = None

        # Initialize OpenAI client for fallback transcription
        self.openai_client = OpenAI(api_key=api_key)

    def filter_text(self, text: str) -> str:
        """
        Filter text based on configured options.

        By default:
        - Filters out "Bye." and "Thank you." (common false positives)
        - Filters out non-English characters
        """
        if not text:
            return text

        # Filter out common false positive strings
        if not self.allow_bye_thank_you:
            # Remove exact matches of "Bye." and "Thank you."
            # Using word boundaries to avoid removing these from longer sentences
            text = re.sub(r'\bBye\.\s*', '', text)
            text = re.sub(r'\bThank you\.\s*', '', text)

        # Filter out non-English characters
        if not self.allow_non_english:
            # Keep only ASCII printable characters (letters, numbers, punctuation, spaces)
            # This preserves English text while removing non-Latin scripts
            text = re.sub(r'[^\x20-\x7E]', '', text)

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
                f.flush()  # Ensure immediate write to disk

            self.current_transcript.append(text)

    def type_text(self, text: str):
        """Type text using the robust keyboard typer."""
        if not text.strip():
            return

        try:
            success = self.typer.type_text(text)
            if not success:
                print(f"[INFO] Text not typed but saved to: {self.log_file}", file=sys.stderr)
        except Exception as e:
            print(f"\n[ERROR] Failed to type text: {e}", file=sys.stderr)
            print(f"[RECOVERY] Text saved to: {self.log_file}", file=sys.stderr)

    def track_item_creation(self, item_id: str):
        """Track when a conversation item is created to maintain order."""
        if item_id and item_id not in self.item_order:
            self.item_order.append(item_id)

    def handle_completed_transcript(self, item_id: str, transcript: str):
        """Handle a completed transcript, buffering it and outputting in order."""
        # Mark this item as completed (no fallback needed)
        if item_id and item_id in self.item_speech_times:
            self.item_speech_times[item_id]["completed"] = True

        if not item_id:
            # No item_id means we can't order it - output immediately
            self._output_transcript(transcript)
            return

        # Store the completed transcript
        self.completed_transcripts[item_id] = transcript

        # Try to output all transcripts that are now in order
        self._flush_ordered_transcripts()

    def _flush_ordered_transcripts(self):
        """Output completed transcripts in the order items were created."""
        while self.next_output_index < len(self.item_order):
            next_item_id = self.item_order[self.next_output_index]

            # Check if this item has completed
            if next_item_id in self.completed_transcripts:
                transcript = self.completed_transcripts.pop(next_item_id)
                self._output_transcript(transcript)
                self.next_output_index += 1
            else:
                # This item hasn't completed yet, stop here
                break

    def _output_transcript(self, transcript: str):
        """Output a transcript (log and type) after filtering."""
        filtered_transcript = self.filter_text(transcript)

        # Check for duplicates to prevent typing the same thing twice
        # This is important for fallback transcription to avoid double-typing
        if filtered_transcript and filtered_transcript in self.output_transcripts:
            print(f"[DUPLICATE] Skipped duplicate: '{filtered_transcript}'", file=sys.stderr)
            return

        if filtered_transcript:
            self.output_transcripts.add(filtered_transcript)
            self.log_transcript(filtered_transcript, partial=False)
            self.type_text(filtered_transcript)
        elif transcript != filtered_transcript:
            # Something was completely filtered out
            print(f"[FILTERED] Skipped: '{transcript}'", file=sys.stderr)

    def log_debug_event(self, data: dict):
        """Log detailed event information for debugging ordering issues."""
        try:
            self.event_counter += 1
            debug_entry = {
                "local_sequence": self.event_counter,
                "timestamp": datetime.now().isoformat(),
                "event_id": data.get("event_id"),
                "type": data.get("type"),
                "item_id": data.get("item_id"),
                "content_index": data.get("content_index"),
                "delta": data.get("delta"),
                "transcript": data.get("transcript"),
                "full_event": data  # Store complete event for analysis
            }

            # Append to JSONL debug log
            with open(self.debug_log_file, "a", encoding="utf-8") as f:
                f.write(json.dumps(debug_entry) + "\n")
                f.flush()  # Ensure immediate write to disk

        except Exception as e:
            print(f"\n[WARNING] Debug logging failed: {e}", file=sys.stderr)

    def on_message(self, ws, message):
        """Handle incoming WebSocket messages."""
        try:
            data = json.loads(message)
            msg_type = data.get("type", "")

            # Log all events for debugging
            self.log_debug_event(data)

            # Track item creation to maintain ordering
            if msg_type == "conversation.item.created":
                item_data = data.get("item", {})
                item_id = item_data.get("id")
                if item_id:
                    self.track_item_creation(item_id)

            # Track speech start/stop times for fallback transcription
            if "speech_started" in msg_type or msg_type == "input_audio_buffer.speech_started":
                item_id = data.get("item_id")
                audio_start_ms = data.get("audio_start_ms", 0)
                if item_id and audio_start_ms is not None:
                    if item_id not in self.item_speech_times:
                        self.item_speech_times[item_id] = {}
                    self.item_speech_times[item_id]["start_ms"] = audio_start_ms

            if "speech_stopped" in msg_type or msg_type == "input_audio_buffer.speech_stopped":
                item_id = data.get("item_id")
                audio_end_ms = data.get("audio_end_ms", 0)
                if item_id and audio_end_ms is not None:
                    if item_id not in self.item_speech_times:
                        self.item_speech_times[item_id] = {}
                    self.item_speech_times[item_id]["end_ms"] = audio_end_ms
                    self.item_speech_times[item_id]["stopped_at"] = time.time()

            # Handle input audio transcription (what the user is saying)
            if msg_type == "conversation.item.input_audio_transcription.completed":
                # Final transcript from user's speech
                item_id = data.get("item_id")
                transcript = data.get("transcript", "")
                if transcript:
                    # Use ordering system to ensure correct sequence
                    self.handle_completed_transcript(item_id, transcript)
                    self.transcript_buffer = ""

            elif msg_type == "conversation.item.input_audio_transcription.delta":
                # Partial transcript (streaming) from user's speech
                delta = data.get("delta", "")
                if delta:
                    self.transcript_buffer += delta
                    # Apply filtering to partial transcripts for display
                    filtered_buffer = self.filter_text(self.transcript_buffer)
                    if filtered_buffer:
                        self.log_transcript(filtered_buffer, partial=True)

            # Legacy response events (for compatibility)
            elif msg_type == "response.audio_transcript.done":
                item_id = data.get("item_id")
                transcript = data.get("transcript", "")
                if transcript:
                    # Use ordering system to ensure correct sequence
                    self.handle_completed_transcript(item_id, transcript)
                    self.transcript_buffer = ""

            elif msg_type == "response.audio_transcript.delta":
                delta = data.get("delta", "")
                if delta:
                    self.transcript_buffer += delta
                    # Apply filtering to partial transcripts for display
                    filtered_buffer = self.filter_text(self.transcript_buffer)
                    if filtered_buffer:
                        self.log_transcript(filtered_buffer, partial=True)

            elif msg_type == "error":
                error_msg = data.get("error", {})
                print(f"\n[ERROR] OpenAI API error: {error_msg}", file=sys.stderr)

            elif msg_type == "session.created":
                print("[INFO] Session created successfully")

            elif msg_type == "session.updated":
                print("[INFO] Session configuration updated")

            # Debug: show all message types
            else:
                print(f"[DEBUG] Received message type: {msg_type}")
                if msg_type not in ["session.created", "session.updated"]:
                    print(f"[DEBUG] Full message: {json.dumps(data, indent=2)}")

        except json.JSONDecodeError as e:
            print(f"\n[ERROR] Failed to parse message: {e}", file=sys.stderr)
        except Exception as e:
            print(f"\n[ERROR] Message handler error: {e}", file=sys.stderr)

    def on_error(self, ws, error):
        """Handle WebSocket errors."""
        print(f"\n[ERROR] WebSocket error: {error}", file=sys.stderr)

    def on_close(self, ws, close_status_code, close_msg):
        """Handle WebSocket connection close."""
        print(f"\n[WARNING] WebSocket connection closed!", file=sys.stderr)
        print(f"[WARNING] Status code: {close_status_code}", file=sys.stderr)
        print(f"[WARNING] Message: {close_msg}", file=sys.stderr)

        # Common close codes:
        # 1000 = Normal closure
        # 1001 = Going away
        # 1006 = Abnormal closure (no close frame received)
        # 1008 = Policy violation
        # 1011 = Server error

        if close_status_code == 1000:
            print("[INFO] Connection closed normally", file=sys.stderr)
        elif close_status_code == 1006:
            print("[ERROR] Connection lost unexpectedly (possible network issue or timeout)", file=sys.stderr)
        elif close_status_code:
            print(f"[ERROR] Unexpected close code: {close_status_code}", file=sys.stderr)

        self.running = False

    def on_open(self, ws):
        """Handle WebSocket connection open."""
        print(f"[INFO] WebSocket connection established (transcription mode, model: {self.model})")

        # Configure transcription session to enable transcription
        session_config = {
            "type": "transcription_session.update",
            "session": {
                "input_audio_format": "pcm16",
                "turn_detection": {
                    "type": "server_vad",
                    "threshold": 0.5
                },
                "input_audio_transcription": {
                    "model": self.model
                }
            }
        }

        ws.send(json.dumps(session_config))
        print("[INFO] Transcription enabled (keepalive: active)")
        print("[INFO] Fallback transcription enabled (5s timeout)")
        print("[INFO] Starting audio capture...")
        print("[INFO] Speak into your microphone. Transcription will be typed and logged.")
        print("[INFO] The session will stay active indefinitely - silence is OK!")
        print("[INFO] Press Ctrl+C to stop.\n")

        # Start audio streaming in a separate thread
        self.audio_thread = threading.Thread(target=self.stream_audio)
        self.audio_thread.daemon = True
        self.audio_thread.start()

        # Start timeout checking thread
        self.timeout_check_thread = threading.Thread(target=self.check_timeouts)
        self.timeout_check_thread.daemon = True
        self.timeout_check_thread.start()

    def _find_best_chunk_match(self, start_ms: int, end_ms: int, margin_ms: int) -> Optional[List[bytes]]:
        """
        Core algorithm: Find best matching audio chunks for a time range.

        Tries different timestamp offsets to handle VAD timing uncertainty,
        and picks the match with the best duration alignment.
        """
        expected_duration_ms = end_ms - start_ms
        best_chunks = None
        best_duration_error = float('inf')
        best_offset = None

        # Try offsets from -margin to +margin in 20ms steps
        for offset_ms in range(-margin_ms, margin_ms + 1, 20):
            test_start = start_ms + offset_ms
            test_end = end_ms + offset_ms

            candidate_chunks = [
                chunk for ts, chunk in self.audio_buffer
                if test_start <= ts <= test_end
            ]

            if candidate_chunks:
                # Calculate duration error for this candidate
                actual_duration_ms = len(candidate_chunks) * (1024 / 24000 * 1000)
                duration_error = abs(expected_duration_ms - actual_duration_ms)

                # Keep track of best match
                if duration_error < best_duration_error:
                    best_duration_error = duration_error
                    best_chunks = candidate_chunks
                    best_offset = offset_ms

        return best_chunks, best_duration_error, best_offset if best_chunks else (None, None, None)

    def extract_audio_chunks(self, item_id: str) -> Optional[bytes]:
        """Extract audio chunks for a specific item using best timestamp match."""
        if item_id not in self.item_speech_times:
            return None

        times = self.item_speech_times[item_id]
        if "start_ms" not in times or "end_ms" not in times:
            return None

        start_ms = times["start_ms"]
        end_ms = times["end_ms"]
        expected_duration_ms = end_ms - start_ms

        # Skip very short segments (likely noise or false positives)
        if expected_duration_ms < self.min_duration_ms:
            print(f"[INFO] Skipping short segment ({expected_duration_ms}ms) for item {item_id[:20]}", file=sys.stderr)
            return None

        # Find best matching chunks using core algorithm
        best_chunks, duration_error, offset = self._find_best_chunk_match(
            start_ms, end_ms, self.timestamp_margin_ms
        )

        if not best_chunks:
            print(f"[WARNING] No matching chunks found for item {item_id[:20]}", file=sys.stderr)
            return None

        # Report match quality
        actual_duration_ms = len(best_chunks) * (1024 / 24000 * 1000)
        print(f"[FALLBACK] Extracted {len(best_chunks)} chunks (offset {offset}ms), duration error: {duration_error:.0f}ms", file=sys.stderr)

        if duration_error > 500:
            print(f"[WARNING] Large duration mismatch: expected {expected_duration_ms}ms, got {actual_duration_ms:.0f}ms", file=sys.stderr)

        # Combine chunks into single audio data
        audio_data = b''.join(best_chunks)
        return audio_data

    def fallback_transcribe(self, item_id: str) -> Optional[str]:
        """Transcribe audio using OpenAI Whisper API as fallback."""
        try:
            # Extract audio chunks for this item
            audio_data = self.extract_audio_chunks(item_id)
            if not audio_data:
                return None

            # Create WAV file in memory
            wav_buffer = io.BytesIO()
            with wave.open(wav_buffer, 'wb') as wav_file:
                wav_file.setnchannels(1)  # Mono
                wav_file.setsampwidth(2)  # 16-bit
                wav_file.setframerate(24000)  # 24kHz
                wav_file.writeframes(audio_data)

            wav_buffer.seek(0)
            wav_buffer.name = "audio.wav"  # OpenAI client needs a name attribute

            # Call OpenAI Whisper API using official client
            print(f"[FALLBACK] Transcribing item {item_id[:20]} with Whisper API...", file=sys.stderr)
            transcription = self.openai_client.audio.transcriptions.create(
                model="whisper-1",
                file=wav_buffer
            )

            transcript = transcription.text
            print(f"[FALLBACK] Success: '{transcript}'", file=sys.stderr)
            return transcript

        except Exception as e:
            print(f"[FALLBACK] Error: {e}", file=sys.stderr)
            return None

    def check_timeouts(self):
        """Check for items that have timed out and need fallback transcription."""
        while self.running:
            time.sleep(1)  # Check every second

            if not self.session_start_time:
                continue

            current_time = time.time()

            # Check each item that has stopped but not completed
            for item_id, times in list(self.item_speech_times.items()):
                # Skip if already completed or no stopped time
                if times.get("completed") or "stopped_at" not in times:
                    continue

                # Check if timeout has elapsed
                time_since_stopped = current_time - times["stopped_at"]
                if time_since_stopped >= self.timeout_seconds:
                    print(f"\n[TIMEOUT] Item {item_id[:20]} didn't complete after {self.timeout_seconds}s", file=sys.stderr)

                    # Try fallback transcription
                    transcript = self.fallback_transcribe(item_id)

                    if transcript:
                        # Mark as completed and handle normally
                        self.handle_completed_transcript(item_id, transcript)
                    else:
                        # Mark as completed even if fallback failed (to unblock queue)
                        print(f"[TIMEOUT] Skipping item {item_id[:20]} - fallback failed", file=sys.stderr)
                        self.handle_completed_transcript(item_id, "")

    def stream_audio(self):
        """Capture and stream audio to OpenAI."""
        try:
            # Set up mic capture (24kHz, mono, PCM16) with automatic device selection
            self.stream = open_audio_stream(self.audio, rate=24000, verbose=True)
            if self.stream is None:
                self.running = False
                return

            # Track session start time for audio buffering
            self.session_start_time = time.time()

            while self.running and self.ws:
                try:
                    # Get current timestamp in milliseconds since session start
                    current_time = time.time()
                    time_since_start_ms = int((current_time - self.session_start_time) * 1000)

                    # Read audio chunk
                    audio_chunk = self.stream.read(1024, exception_on_overflow=False)

                    # Buffer the audio with timestamp for fallback transcription
                    self.audio_buffer.append((time_since_start_ms, audio_chunk))

                    # Send to OpenAI
                    b64_audio = base64.b64encode(audio_chunk).decode()
                    self.ws.send(json.dumps({
                        "type": "input_audio_buffer.append",
                        "audio": b64_audio
                    }))

                except Exception as e:
                    if self.running:
                        print(f"\n[ERROR] Audio streaming error: {e}", file=sys.stderr)
                    break

        except Exception as e:
            print(f"\n[ERROR] Failed to open audio stream: {e}", file=sys.stderr)
            self.running = False

    def cleanup(self):
        """Clean up resources."""
        print("\n[INFO] Cleaning up...")
        self.running = False

        if self.stream:
            try:
                self.stream.stop_stream()
                self.stream.close()
            except:
                pass

        if self.audio:
            try:
                self.audio.terminate()
            except:
                pass

        if self.ws:
            try:
                self.ws.close()
            except:
                pass

        print(f"[INFO] Transcription saved to: {self.log_file}")
        print(f"[INFO] Debug event log saved to: {self.debug_log_file}")
        print(f"[INFO] Total segments transcribed: {len(self.current_transcript)}")

    def run(self):
        """Start the transcription session."""
        self.running = True

        # Set up signal handlers for graceful shutdown
        signal.signal(signal.SIGINT, lambda sig, frame: self.cleanup() or sys.exit(0))
        signal.signal(signal.SIGTERM, lambda sig, frame: self.cleanup() or sys.exit(0))

        # Create WebSocket connection in transcription-only mode
        self.ws = websocket.WebSocketApp(
            "wss://api.openai.com/v1/realtime?intent=transcription",
            header={
                "Authorization": f"Bearer {self.api_key}",
                "OpenAI-Beta": "realtime=v1"
            },
            on_open=self.on_open,
            on_message=self.on_message,
            on_error=self.on_error,
            on_close=self.on_close
        )

        print("[INFO] Connecting to OpenAI...")

        try:
            # Run WebSocket connection with keepalive to prevent timeouts
            # ping_interval: Send ping every 20 seconds to keep connection alive
            # ping_timeout: Wait up to 10 seconds for pong response
            self.ws.run_forever(
                ping_interval=20,
                ping_timeout=10
            )
        except KeyboardInterrupt:
            pass
        finally:
            self.cleanup()


def main():
    """Main entry point."""
    # Parse command-line arguments
    parser = argparse.ArgumentParser(
        description="Real-time audio transcription with OpenAI API",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Available models:
  whisper-1            Whisper transcription model (default, most accurate)
  gpt-4o-transcribe    GPT-4o transcription (fast, high quality)
  gpt-4o-mini-transcribe  GPT-4o mini transcription (faster, lower cost)

Examples:
  transcribe                    # Use default whisper-1 model
  transcribe --model gpt-4o-transcribe
  transcribe -m whisper-1
  transcribe --allow-bye-thank-you    # Allow "Bye." and "Thank you."
  transcribe --allow-non-english      # Allow non-English characters

Filtering (enabled by default):
  By default, the following are filtered from transcriptions:
  - "Bye." and "Thank you." (common false positives)
  - Non-English (non-ASCII) characters
  Use the flags above to disable filtering if needed.
        """
    )
    parser.add_argument(
        "-m", "--model",
        type=str,
        default="whisper-1",
        choices=["whisper-1", "gpt-4o-transcribe", "gpt-4o-mini-transcribe"],
        help="Transcription model to use (default: whisper-1)"
    )
    parser.add_argument(
        "--allow-bye-thank-you",
        action="store_true",
        help='Allow "Bye." and "Thank you." in transcriptions (filtered by default)'
    )
    parser.add_argument(
        "--allow-non-english",
        action="store_true",
        help="Allow non-English (non-ASCII) characters in transcriptions (filtered by default)"
    )

    args = parser.parse_args()

    print("=" * 60)
    print("Real-Time Transcription with OpenAI")
    print("=" * 60)
    print()

    # Check system dependencies before proceeding
    if not check_system_dependencies():
        print("\n[ERROR] Cannot start transcription due to missing dependencies.", file=sys.stderr)
        sys.exit(1)

    # Get API key from environment
    api_key = os.getenv("OPENAI_API_KEY")

    if not api_key:
        print("\n[ERROR] OPENAI_API_KEY not found!", file=sys.stderr)
        print("[ERROR] Please create a .env file with:", file=sys.stderr)
        print("[ERROR]   OPENAI_API_KEY=your_api_key_here", file=sys.stderr)
        print("[ERROR] Or create a .secrets file with the same format.", file=sys.stderr)
        sys.exit(1)

    # Start transcription session with selected model and filtering options
    session = TranscriptionSession(
        api_key,
        model=args.model,
        allow_bye_thank_you=args.allow_bye_thank_you,
        allow_non_english=args.allow_non_english
    )

    try:
        session.run()
    except Exception as e:
        print(f"\n[ERROR] Fatal error: {e}", file=sys.stderr)
        session.cleanup()
        sys.exit(1)


if __name__ == "__main__":
    main()
