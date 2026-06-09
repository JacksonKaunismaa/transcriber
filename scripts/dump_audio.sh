#!/bin/bash
# Trigger the transcriber's rolling-buffer audio dump (SIGHUP).
# Bind to a Hyprland hotkey to capture audio whenever you notice the
# transcriber missed a word — the last ~30s gets written to
# conversations/audio_dumps/dump_YYYYMMDD_HHMMSS.wav

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PID_FILE="$SCRIPT_DIR/.transcribe.pid"

if [ ! -f "$PID_FILE" ]; then
    echo "transcriber not running (no PID file at $PID_FILE)" >&2
    exit 1
fi

PID=$(cat "$PID_FILE")
if ! kill -0 "$PID" 2>/dev/null; then
    echo "transcriber PID $PID is stale (process gone)" >&2
    exit 1
fi

kill -HUP "$PID"
echo "dump requested (SIGHUP -> PID $PID); check conversations/audio_dumps/"
