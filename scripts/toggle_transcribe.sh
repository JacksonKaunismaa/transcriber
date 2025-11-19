#!/bin/bash
# Toggle script for real-time transcription
# Starts transcription if not running, stops it if running

# Get the directory where this script lives
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PID_FILE="$SCRIPT_DIR/.transcribe.pid"
LOG_FILE="$SCRIPT_DIR/.transcribe.log"

# Function to send notifications
send_notification() {
    local title="$1"
    local message="$2"
    local icon="$3"

    # Try notify-send
    if command -v notify-send &> /dev/null; then
        # Set DBUS_SESSION_BUS_ADDRESS if not set (for KDE keybindings)
        if [ -z "$DBUS_SESSION_BUS_ADDRESS" ]; then
            local uid=$(id -u)
            local dbus_addr=$(find /run/user/$uid -name "bus" 2>/dev/null | head -1)
            if [ -n "$dbus_addr" ]; then
                export DBUS_SESSION_BUS_ADDRESS="unix:path=$dbus_addr"
            fi
        fi
        notify-send "$title" "$message" --icon="$icon" 2>/dev/null
    fi

    # Also try kdialog for KDE
    if command -v kdialog &> /dev/null; then
        kdialog --passivepopup "$message" 3 --title "$title" 2>/dev/null &
    fi
}

# Check if transcription is already running
if [ -f "$PID_FILE" ]; then
    PID=$(cat "$PID_FILE")

    # Check if the process is actually running
    if kill -0 "$PID" 2>/dev/null; then
        # Process is running, stop it
        kill -TERM "$PID"

        # Wait up to 5 seconds for graceful shutdown
        for i in {1..50}; do
            if ! kill -0 "$PID" 2>/dev/null; then
                break
            fi
            sleep 0.1
        done

        # Force kill if still running
        if kill -0 "$PID" 2>/dev/null; then
            kill -KILL "$PID"
        fi

        rm -f "$PID_FILE"
        send_notification "Transcription" "Stopped" "media-playback-stop"
        exit 0
    else
        # PID file exists but process is not running, clean up stale file
        rm -f "$PID_FILE"
    fi
fi

# Not running, start it
# Change to project root (parent of scripts directory)
cd "$SCRIPT_DIR/.."

# Ensure uv is in PATH (add common locations)
export PATH="/usr/bin:/usr/local/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

# Find uv
UV_PATH=$(which uv 2>/dev/null)
if [ -z "$UV_PATH" ]; then
    send_notification "Transcription" "Failed to start: uv not found" "dialog-error"
    exit 1
fi

# Start transcription in background and save PID
nohup "$UV_PATH" run transcribe > "$LOG_FILE" 2>&1 &
PID=$!

echo "$PID" > "$PID_FILE"

# Give it a moment to start
sleep 1

# Check if it actually started
if kill -0 "$PID" 2>/dev/null; then
    send_notification "Transcription" "Started - Speak into your microphone" "media-playback-start"
else
    rm -f "$PID_FILE"
    send_notification "Transcription" "Failed to start - check logs" "dialog-error"
    exit 1
fi
