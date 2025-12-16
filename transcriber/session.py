"""
Real-time transcription session management.

Handles:
- WebSocket connection to OpenAI Realtime API
- Audio capture and streaming
- Message routing to transcript manager and audio buffer
- Automatic reconnection on session expiration
"""

import pyaudio
import websocket
import json
import base64
import signal
import sys
import threading
import time
import logging
from datetime import datetime
from pathlib import Path
from typing import Optional
from openai import OpenAI

from transcriber.typer import KeyboardTyper
from transcriber.audio_device import open_audio_stream
from transcriber.noise_reduction import create_audio_processor
from transcriber.transcript import TranscriptManager
from transcriber.audio_buffer import AudioBuffer
from transcriber.metrics import TranscriptionMetrics


class TranscriptionSession:
    """Manages a real-time transcription session."""

    def __init__(self, api_key: str, model: str = "whisper-1",
                 allow_bye_thank_you: bool = False, allow_non_ascii: bool = False,
                 allow_fillers: bool = False,
                 noise_suppression: int = 0, auto_gain: float = 1.0,
                 no_log: bool = False):
        self.api_key = api_key
        self.model = model
        self.no_log = no_log
        self.ws: Optional[websocket.WebSocketApp] = None
        self.audio = pyaudio.PyAudio()
        self.stream = None
        self.running = False
        self.audio_thread = None

        # Audio processing options
        self.noise_suppression = noise_suppression
        self.gain = auto_gain
        self.audio_processor = None

        # Initialize keyboard typer
        self.typer = KeyboardTyper()
        print(f"[INFO] {self.typer.get_status_message()}")

        instructions = self.typer.get_setup_instructions()
        if instructions:
            print(f"[WARNING] Keyboard typing not available!")
            print(instructions)

        # Set up logging (skip if --no-log)
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        if no_log:
            self.conversations_dir = None
            self.log_file = None
            self.debug_log_file = None
        else:
            self.conversations_dir = Path("conversations")
            self.conversations_dir.mkdir(exist_ok=True)
            self.log_file = self.conversations_dir / f"transcription_{timestamp}.txt"
            self.debug_log_file = self.conversations_dir / f"debug_events_{timestamp}.jsonl"

        self.logger = logging.getLogger(f"transcriber.{timestamp}")
        self.logger.setLevel(logging.DEBUG)
        if not no_log:
            file_handler = logging.FileHandler(self.debug_log_file, encoding="utf-8")
            file_handler.setLevel(logging.DEBUG)
            file_handler.setFormatter(logging.Formatter(
                '{"timestamp": "%(asctime)s", "level": "%(levelname)s", "message": %(message)s}'
            ))
            self.logger.addHandler(file_handler)

        # Buffer for partial transcripts (display only)
        self.transcript_buffer = ""
        self.event_counter = 0

        # Initialize OpenAI client
        self.openai_client = OpenAI(api_key=api_key)

        # Metrics tracking
        self.metrics = TranscriptionMetrics()

        # Initialize transcript manager
        self.transcript_manager = TranscriptManager(
            typer=self.typer,
            log_file=self.log_file,
            logger=self.logger,
            allow_bye_thank_you=allow_bye_thank_you,
            allow_non_ascii=allow_non_ascii,
            allow_fillers=allow_fillers,
            metrics=self.metrics
        )

        # Initialize audio buffer for fallback transcription
        self.audio_buffer = AudioBuffer(
            openai_client=self.openai_client,
            logger=self.logger,
            on_transcript_complete=self.transcript_manager.handle_completed_transcript,
            timeout_seconds=2.5,
            timestamp_margin_ms=200,
            min_duration_ms=300,
            metrics=self.metrics
        )

        # Share item_speech_times between managers
        self.transcript_manager.set_item_speech_times(self.audio_buffer.item_speech_times)

        # Reconnection settings
        self.should_reconnect = False
        self.reconnect_attempts = 0
        self.max_reconnect_attempts = 10
        self.reconnect_delay = 1.0

    def log_debug_event(self, data: dict):
        """Log detailed event information for debugging."""
        self.event_counter += 1
        debug_entry = {
            "local_sequence": self.event_counter,
            "event_id": data.get("event_id"),
            "type": data.get("type"),
            "item_id": data.get("item_id"),
            "content_index": data.get("content_index"),
            "delta": data.get("delta"),
            "transcript": data.get("transcript"),
            "full_event": data
        }
        self.logger.debug(json.dumps(debug_entry))

    def on_message(self, ws, message):
        """Handle incoming WebSocket messages."""
        try:
            data = json.loads(message)
            msg_type = data.get("type", "")

            self.log_debug_event(data)

            if msg_type == "conversation.item.created":
                item_data = data.get("item", {})
                item_id = item_data.get("id")
                if item_id:
                    self.transcript_manager.track_item_creation(item_id)

            elif msg_type == "input_audio_buffer.speech_started":
                item_id = data.get("item_id")
                audio_start_ms = data.get("audio_start_ms", 0)
                if item_id:
                    self.audio_buffer.record_speech_started(item_id, audio_start_ms)

            elif msg_type == "input_audio_buffer.speech_stopped":
                item_id = data.get("item_id")
                audio_end_ms = data.get("audio_end_ms", 0)
                if item_id:
                    self.audio_buffer.record_speech_stopped(item_id, audio_end_ms)

            elif msg_type == "conversation.item.input_audio_transcription.completed":
                item_id = data.get("item_id")
                transcript = data.get("transcript", "")
                self.metrics.record_realtime_transcription()
                if transcript:
                    self.transcript_manager.handle_completed_transcript(item_id, transcript)
                    self.transcript_buffer = ""

            elif msg_type == "conversation.item.input_audio_transcription.delta":
                delta = data.get("delta", "")
                if delta:
                    self.transcript_buffer += delta
                    filtered_buffer = self.transcript_manager.filter_text(self.transcript_buffer)
                    if filtered_buffer:
                        self.transcript_manager.log_transcript(filtered_buffer, partial=True)

            # Legacy response events (compatibility)
            elif msg_type == "response.audio_transcript.done":
                item_id = data.get("item_id")
                transcript = data.get("transcript", "")
                self.metrics.record_realtime_transcription()
                if transcript:
                    self.transcript_manager.handle_completed_transcript(item_id, transcript)
                    self.transcript_buffer = ""

            elif msg_type == "response.audio_transcript.delta":
                delta = data.get("delta", "")
                if delta:
                    self.transcript_buffer += delta
                    filtered_buffer = self.transcript_manager.filter_text(self.transcript_buffer)
                    if filtered_buffer:
                        self.transcript_manager.log_transcript(filtered_buffer, partial=True)

            elif msg_type == "error":
                error_data = data.get("error", {})
                error_code = error_data.get("code", "")
                error_message = error_data.get("message", str(error_data))

                if error_code == "session_expired":
                    self.logger.warning(f'"Session expired: {error_message}, will reconnect"')
                    self.metrics.record_session_expiration()
                    self.should_reconnect = True
                    if self.ws:
                        self.ws.close()
                else:
                    self.logger.error(f'"OpenAI API error: {error_data}"')
                    self.metrics.record_api_error()

            elif msg_type == "session.created":
                print("[INFO] Session created successfully")

            elif msg_type == "session.updated":
                print("[INFO] Session configuration updated")

        except json.JSONDecodeError:
            self.logger.exception('"Failed to parse WebSocket message"')
        except Exception:
            self.logger.exception('"Message handler error"')

    def on_error(self, ws, error):
        """Handle WebSocket errors."""
        self.logger.error(f'"WebSocket error: {error}"')
        self.metrics.record_websocket_error()

    def on_close(self, ws, close_status_code, close_msg):
        """Handle WebSocket connection close."""
        if close_status_code == 1000:
            # Normal closure (intentional), don't reconnect
            self.logger.info(f'"WebSocket closed normally, code={close_status_code}, msg={close_msg}"')
        elif close_status_code == 1006 or close_status_code is None:
            # Abnormal closure / connection lost - reconnect
            self.logger.error(f'"WebSocket connection lost unexpectedly, code={close_status_code}, msg={close_msg}, will reconnect"')
            self.should_reconnect = True
        else:
            # Other closure codes - try to reconnect
            self.logger.warning(f'"WebSocket closed, code={close_status_code}, msg={close_msg}, will reconnect"')
            self.should_reconnect = True

        if not self.should_reconnect:
            self.running = False

    def on_open(self, ws):
        """Handle WebSocket connection open."""
        self.metrics.record_connection_success()
        print(f"[INFO] WebSocket connection established (transcription mode, model: {self.model})")

        session_config = {
            "type": "transcription_session.update",
            "session": {
                "input_audio_transcription": {
                    "model": self.model
                }
            }
        }

        ws.send(json.dumps(session_config))
        print("[INFO] Transcription enabled (keepalive: active)")
        print("[INFO] Fallback transcription enabled (2.5s timeout)")
        print("[INFO] Starting audio capture...")
        print("[INFO] Speak into your microphone. Transcription will be typed and logged.")
        print("[INFO] The session will stay active indefinitely - silence is OK!")
        print("[INFO] Press Ctrl+C to stop.\n")

        self.audio_thread = threading.Thread(target=self.stream_audio)
        self.audio_thread.daemon = True
        self.audio_thread.start()

        self.audio_buffer.start()

    def stream_audio(self):
        """Capture and stream audio to OpenAI."""
        try:
            self.stream = open_audio_stream(self.audio, rate=24000, verbose=True)
            if self.stream is None:
                self.running = False
                return

            if self.noise_suppression > 0 or self.gain != 1.0:
                self.audio_processor = create_audio_processor(
                    noise_suppression_level=self.noise_suppression,
                    gain_multiplier=self.gain,
                )
                if self.audio_processor:
                    print(f"[INFO] Audio processing enabled (noise suppression: {self.noise_suppression}/4, gain: {self.gain}x)")
                else:
                    print("[WARNING] webrtc-noise-gain not available, audio processing disabled")

            self.audio_buffer.session_start_time = time.time()

            while self.running and self.ws:
                try:
                    audio_chunk = self.stream.read(1024, exception_on_overflow=False)

                    if self.audio_processor:
                        processed_chunk = self.audio_processor.process_chunk(audio_chunk)
                        if processed_chunk:
                            audio_chunk = processed_chunk
                        else:
                            continue

                    self.audio_buffer.add_audio_chunk(audio_chunk)

                    b64_audio = base64.b64encode(audio_chunk).decode()
                    self.ws.send(json.dumps({
                        "type": "input_audio_buffer.append",
                        "audio": b64_audio
                    }))
                    self.metrics.record_audio_chunk_sent()

                except Exception:
                    if self.running:
                        self.logger.exception('"Audio streaming error"')
                    break

        except Exception:
            self.logger.exception('"Failed to open audio stream"')
            self.running = False

    def reset_session_state(self):
        """Reset internal state for a new session while preserving log files."""
        self.transcript_buffer = ""
        self.transcript_manager.reset()
        self.audio_buffer.stop()  # Stop timeout thread before reset to prevent duplicate threads
        self.audio_buffer.reset()
        # Re-link shared dict after reset (reset creates new dict)
        self.transcript_manager.set_item_speech_times(self.audio_buffer.item_speech_times)

        # Close audio stream to prevent leaking recording instances on reconnect
        if self.stream:
            try:
                self.stream.stop_stream()
                self.stream.close()
            except Exception:
                pass
            self.stream = None

        # Wait for audio thread to finish before starting a new one
        if self.audio_thread and self.audio_thread.is_alive():
            self.audio_thread.join(timeout=2.0)
        self.audio_thread = None

        self.ws = None
        self.audio_processor = None
        self.logger.info('"Session state reset for reconnection"')

    def cleanup(self):
        """Clean up resources."""
        self.logger.info('"Cleaning up..."')
        self.running = False
        self.audio_buffer.stop()

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

        # Stop periodic metrics logging and write final summary
        self.metrics.stop()
        if self.conversations_dir is not None:
            metrics_file = self.metrics.write_summary(self.conversations_dir)
            self.logger.info(f'"Session ended. Transcription: {self.log_file}, Metrics: {metrics_file}"')
        else:
            self.logger.info('"Session ended (no-log mode, files not saved)"')

    def run(self):
        """Start the transcription session with automatic reconnection."""
        self.running = True
        self.metrics.start_session(logger=self.logger)

        signal.signal(signal.SIGINT, lambda sig, frame: self.cleanup() or sys.exit(0))
        signal.signal(signal.SIGTERM, lambda sig, frame: self.cleanup() or sys.exit(0))

        while self.running:
            self.should_reconnect = False

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

            self.metrics.record_connection_attempt()
            if self.reconnect_attempts == 0:
                print("[INFO] Connecting to OpenAI...")
            else:
                print(f"[INFO] Reconnecting to OpenAI (attempt {self.reconnect_attempts})...")

            try:
                self.ws.run_forever(ping_interval=20, ping_timeout=10)
            except KeyboardInterrupt:
                self.running = False
                break

            if self.should_reconnect and self.running:
                self.reconnect_attempts += 1
                self.metrics.record_reconnection_attempt()

                if self.reconnect_attempts > self.max_reconnect_attempts:
                    self.logger.error(f'"Max reconnection attempts ({self.max_reconnect_attempts}) exceeded"')
                    self.running = False
                    break

                delay = min(self.reconnect_delay * (2 ** (self.reconnect_attempts - 1)), 30.0)
                print(f"[INFO] Reconnecting in {delay:.1f} seconds...")
                time.sleep(delay)

                self.reset_session_state()
                self.reconnect_attempts = 0
            else:
                break

        self.cleanup()
