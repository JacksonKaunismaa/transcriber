#!/bin/bash
# Toggle script for real-time transcription
# Starts transcription if not running, stops it if running

# Get the directory where this script lives
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PID_FILE="$SCRIPT_DIR/.transcribe.pid"
LOG_FILE="$SCRIPT_DIR/.transcribe.log"

# Function to send notifications
# Uses --app-icon (not --icon) because --icon maps to the image-path D-Bus
# hint, which Quickshell renders on top of a Material Symbol background.
# --app-icon sets the app_icon D-Bus parameter, which renders cleanly.
send_notification() {
    local title="$1"
    local message="$2"
    local icon="$3"

    # Set DBUS_SESSION_BUS_ADDRESS if not set (for KDE keybindings)
    if [ -z "$DBUS_SESSION_BUS_ADDRESS" ]; then
        local uid=$(id -u)
        local dbus_addr=$(find /run/user/$uid -name "bus" 2>/dev/null | head -1)
        if [ -n "$dbus_addr" ]; then
            export DBUS_SESSION_BUS_ADDRESS="unix:path=$dbus_addr"
        fi
    fi

    if command -v notify-send &> /dev/null; then
        notify-send "$title" "$message" --app-icon="$icon" 2>/dev/null
    elif command -v kdialog &> /dev/null; then
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

# Use the Rust binary (release build)
TRANSCRIBER="$SCRIPT_DIR/../transcriber-rs/target/release/transcriber"

if [ ! -x "$TRANSCRIBER" ]; then
    # Fallback to Python if Rust binary not built
    export PATH="/usr/bin:/usr/local/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
    UV_PATH=$(which uv 2>/dev/null)
    if [ -z "$UV_PATH" ]; then
        send_notification "Transcription" "Failed to start: no binary found" "dialog-error"
        exit 1
    fi
    TRANSCRIBER="$UV_PATH run transcribe"
fi

# Start transcription in background and save PID
nohup $TRANSCRIBER > "$LOG_FILE" 2>&1 &
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
