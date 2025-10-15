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
from datetime import datetime
from pathlib import Path
from typing import Optional
from dotenv import load_dotenv

# Load environment variables
load_dotenv()

# Also check for .secrets file
secrets_path = Path(__file__).parent.parent / ".secrets"
if secrets_path.exists():
    load_dotenv(secrets_path)


class TranscriptionSession:
    """Manages a real-time transcription session."""

    def __init__(self, api_key: str):
        self.api_key = api_key
        self.ws: Optional[websocket.WebSocketApp] = None
        self.audio = pyaudio.PyAudio()
        self.stream = None
        self.running = False
        self.audio_thread = None

        # Check if xdotool is available (better than pynput on Linux)
        self.use_xdotool = shutil.which("xdotool") is not None
        if not self.use_xdotool:
            print("[WARNING] xdotool not found. Keyboard typing may not work correctly.")
            print("[WARNING] Install with: sudo apt-get install xdotool")

        # Set up conversation logging
        self.conversations_dir = Path("conversations")
        self.conversations_dir.mkdir(exist_ok=True)

        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        self.log_file = self.conversations_dir / f"transcription_{timestamp}.txt"
        self.current_transcript = []

        # Buffer for partial transcripts
        self.transcript_buffer = ""

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

            self.current_transcript.append(text)

    def type_text(self, text: str):
        """Type text using xdotool (or fallback) for keyboard automation."""
        if not text.strip():
            return

        try:
            if self.use_xdotool:
                # Use xdotool for reliable typing on Linux
                # Add space after text
                text_with_space = text + " "
                subprocess.run(
                    ["xdotool", "type", "--clearmodifiers", "--", text_with_space],
                    check=True,
                    capture_output=True
                )
            else:
                # Fallback: save to clipboard or just skip
                print(f"\n[WARNING] xdotool not available. Text not typed: {text}", file=sys.stderr)
                print(f"[INFO] Text saved to: {self.log_file}", file=sys.stderr)

        except subprocess.CalledProcessError as e:
            print(f"\n[ERROR] Failed to type text: {e.stderr.decode() if e.stderr else str(e)}", file=sys.stderr)
            print(f"[RECOVERY] Text saved to: {self.log_file}", file=sys.stderr)
        except Exception as e:
            print(f"\n[ERROR] Failed to type text: {e}", file=sys.stderr)
            print(f"[RECOVERY] Text saved to: {self.log_file}", file=sys.stderr)

    def on_message(self, ws, message):
        """Handle incoming WebSocket messages."""
        try:
            data = json.loads(message)
            msg_type = data.get("type", "")

            # Handle input audio transcription (what the user is saying)
            if msg_type == "conversation.item.input_audio_transcription.completed":
                # Final transcript from user's speech
                transcript = data.get("transcript", "")
                if transcript:
                    self.log_transcript(transcript, partial=False)
                    self.type_text(transcript)
                    self.transcript_buffer = ""

            elif msg_type == "conversation.item.input_audio_transcription.delta":
                # Partial transcript (streaming) from user's speech
                delta = data.get("delta", "")
                if delta:
                    self.transcript_buffer += delta
                    self.log_transcript(self.transcript_buffer, partial=True)

            # Legacy response events (for compatibility)
            elif msg_type == "response.audio_transcript.done":
                transcript = data.get("transcript", "")
                if transcript:
                    self.log_transcript(transcript, partial=False)
                    self.type_text(transcript)
                    self.transcript_buffer = ""

            elif msg_type == "response.audio_transcript.delta":
                delta = data.get("delta", "")
                if delta:
                    self.transcript_buffer += delta
                    self.log_transcript(self.transcript_buffer, partial=True)

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
        print(f"\n[INFO] WebSocket closed (status: {close_status_code}, msg: {close_msg})")
        self.running = False

    def on_open(self, ws):
        """Handle WebSocket connection open."""
        print("[INFO] WebSocket connection established (transcription mode)")

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
                    "model": "gpt-4o-transcribe"
                }
            }
        }

        ws.send(json.dumps(session_config))
        print("[INFO] Transcription enabled")
        print("[INFO] Starting audio capture...")
        print("[INFO] Speak into your microphone. Transcription will be typed and logged.")
        print("[INFO] Press Ctrl+C to stop.\n")

        # Start audio streaming in a separate thread
        self.audio_thread = threading.Thread(target=self.stream_audio)
        self.audio_thread.daemon = True
        self.audio_thread.start()

    def stream_audio(self):
        """Capture and stream audio to OpenAI."""
        try:
            # Set up mic capture (24kHz, mono, PCM16)
            self.stream = self.audio.open(
                format=pyaudio.paInt16,
                channels=1,
                rate=24000,
                input=True,
                frames_per_buffer=1024
            )

            while self.running and self.ws:
                try:
                    audio_chunk = self.stream.read(1024, exception_on_overflow=False)
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
            # Run WebSocket connection (blocking)
            self.ws.run_forever()
        except KeyboardInterrupt:
            pass
        finally:
            self.cleanup()


def main():
    """Main entry point."""
    print("=" * 60)
    print("Real-Time Transcription with OpenAI")
    print("=" * 60)

    # Get API key from environment
    api_key = os.getenv("OPENAI_API_KEY")

    if not api_key:
        print("\n[ERROR] OPENAI_API_KEY not found!", file=sys.stderr)
        print("[ERROR] Please create a .env file with:", file=sys.stderr)
        print("[ERROR]   OPENAI_API_KEY=your_api_key_here", file=sys.stderr)
        print("[ERROR] Or create a .secrets file with the same format.", file=sys.stderr)
        sys.exit(1)

    # Start transcription session
    session = TranscriptionSession(api_key)

    try:
        session.run()
    except Exception as e:
        print(f"\n[ERROR] Fatal error: {e}", file=sys.stderr)
        session.cleanup()
        sys.exit(1)


if __name__ == "__main__":
    main()
