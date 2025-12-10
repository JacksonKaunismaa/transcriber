# Real-Time Transcription Tool

A real-time audio transcription tool that captures your microphone input, transcribes it using OpenAI's Realtime API, and automatically types the text into any active window. All transcriptions are also saved to files and displayed in the terminal.

## Features

- **Real-time transcription** using OpenAI's GPT-4 Realtime API
- **Automatic typing** into any active window (works everywhere!)
- **Smart filtering** removes common false positives ("Bye.", "Thank you.") and non-English characters by default
- **Triple redundancy**: Terminal display + file logging + keyboard typing
- **Conversation archiving** in timestamped files
- **Graceful error handling** with recovery mechanisms
- **Zero activation needed** - uses `uv run` for instant execution

## Setup

### 1. Install uv (if you haven't already)

```bash
curl -LsSf https://astral.sh/uv/install.sh | sh
```

### 2. Clone or navigate to this directory

```bash
cd transcriber
```

### 3. Set up your API key

Create a `.env` file in the project root:

```bash
cp .env.example .env
```

Then edit `.env` and add your OpenAI API key:

```
OPENAI_API_KEY=sk-proj-...your-key-here...
```

Alternatively, you can create a `.secrets` file with the same format.

### 4. Install system dependencies (Linux)

For the transcriber to work properly, you'll need:

**PortAudio** (for audio capture):
```bash
# Ubuntu/Debian
sudo apt-get install portaudio19-dev python3-pyaudio

# Fedora
sudo dnf install portaudio-devel

# Arch
sudo pacman -S portaudio
```

**Keyboard typing tools** (automatically detects the best available):

For **Wayland** (recommended - one of these):
```bash
# ydotool - Most compatible
sudo pacman -S ydotool
sudo systemctl enable --now ydotool  # Required to start the daemon

# OR wtype - Simple alternative
sudo pacman -S wtype
```

For **X11**:
```bash
# Ubuntu/Debian
sudo apt-get install xdotool

# Fedora
sudo dnf install xdotool

# Arch
sudo pacman -S xdotool

# openSUSE
sudo zypper install xdotool
```

**Note:**
- The tool automatically detects your display server (Wayland/X11) and uses the best available typing method
- Priority order: **wtype** (Wayland, most reliable) → xdotool (X11) → ydotool → pynput → clipboard fallback
- All dependencies are checked on startup with helpful installation instructions
- **wtype is recommended** for Wayland as it's more reliable than ydotool

## Usage

No need to activate a virtual environment! Just run:

```bash
uv run transcribe
```

### Model Selection

The tool supports three transcription models. Use the `--model` or `-m` flag to select:

```bash
# Default: Whisper (most accurate)
uv run transcribe

# GPT-4o transcription (fast, high quality)
uv run transcribe --model gpt-4o-transcribe

# GPT-4o mini (faster, lower cost)
uv run transcribe -m gpt-4o-mini-transcribe
```

**Available models:**
- `whisper-1` (default) - Most accurate Whisper transcription model
- `gpt-4o-transcribe` - Fast, high-quality GPT-4o-based transcription
- `gpt-4o-mini-transcribe` - Faster, lower-cost alternative

### Automatic Filtering

By default, the tool filters out common false positives to improve transcription quality:

**Filtered by default:**
- `"Bye."` - Often appears as a false positive
- `"Thank you."` - Often appears as a false positive
- Non-English (non-ASCII) characters

**Disable filtering when needed:**
```bash
# Allow "Bye." and "Thank you." in transcriptions
uv run transcribe --allow-bye-thank-you

# Allow non-English characters
uv run transcribe --allow-non-english

# Disable all filtering
uv run transcribe --allow-bye-thank-you --allow-non-english
```

To see all options:
```bash
uv run transcribe --help
```

### What happens when you run it:

1. Connects to OpenAI's Realtime API with keepalive enabled
2. Starts capturing audio from your default microphone
3. Transcribes speech in real-time
4. Types the transcription into whatever window/app is active
5. Displays transcriptions in the terminal (with [PARTIAL] and [FINAL] markers)
6. Saves all final transcriptions to `conversations/transcription_YYYYMMDD_HHMMSS.txt`

**The session will run indefinitely** - it won't timeout during periods of silence. The WebSocket connection sends keepalive pings every 20 seconds to maintain the connection, so you can leave it running as long as you need.

### Stopping the transcription:

Press `Ctrl+C` to gracefully stop the session. The location of your saved transcription will be displayed.

## Toggle Keybinding (Recommended)

For quick access, you can set up a keyboard shortcut to toggle transcription on/off. A toggle script is provided in `scripts/toggle_transcribe.sh` that starts transcription when it's not running and stops it when it is.

### KDE Setup:

1. Open **System Settings** → **Keyboard** → **Shortcuts** → **Custom Shortcuts**
2. Click **Edit** → **New** → **Global Shortcut** → **Command/URL**
3. Set the trigger to **Ctrl+F6** (or your preferred key combination)
4. Set the command to:
   ```
   /full/path/to/transcriber/scripts/toggle_transcribe.sh
   ```
   (Replace with the actual full path to your transcriber directory)
5. Click **Apply**

Now pressing **Ctrl+F6** will:
- **Start** transcription if it's not running (shows a notification)
- **Stop** transcription if it's already running (shows a notification)

### Other Desktop Environments:

**GNOME:**
- Settings → Keyboard → Keyboard Shortcuts → Custom Shortcuts
- Add new shortcut pointing to `toggle_transcribe.sh`

**i3/sway:**
Add to your config:
```
bindsym Control+F6 exec /path/to/transcriber/scripts/toggle_transcribe.sh
```

**xbindkeys:**
Add to `~/.xbindkeysrc`:
```
"/path/to/transcriber/scripts/toggle_transcribe.sh"
  Control+F6
```

### Toggle Script Features:

- **Desktop notifications** when starting/stopping (uses both `notify-send` and `kdialog` for KDE)
- **PID tracking** to ensure only one instance runs at a time
- **Graceful shutdown** with automatic cleanup
- **Transcription log** at `.transcribe.log` with full output from the transcription session

**Important:** Make sure to click **Apply** in KDE System Settings after adding the shortcut!

## Quickshell Bar Widget (Optional)

If you use [Quickshell](https://quickshell.outfoxxed.me/) with the ii config, you can add a status indicator to your bar that shows whether the transcriber is running.

### Installation

```bash
./scripts/install_widget.sh
```

This creates a symlink from `bar/TranscriberStatus.qml` to your Quickshell bar config.

### Widget Features

- **Gray** - Transcriber is off
- **Green** - Transcriber is running
- **Orange** - Transcriber is running but mic is muted
- **Click** - Toggle transcriber on/off

After installing, restart Quickshell: `qs reload`

## How It Works

### Keyboard Automation

The tool uses a **robust multi-method keyboard typing system** that automatically detects and uses the best available method for your system:

**Priority order:**
1. **wtype** (Wayland) - Most reliable for modern Linux systems
2. **xdotool** (X11) - Standard for traditional X11 systems
3. **ydotool** (Wayland fallback) - Alternative Wayland option
4. **pynput** (Python library) - Cross-platform fallback
5. **Clipboard** - Last resort (copies text instead of typing)

The system automatically detects your display server (Wayland vs X11) and selects the appropriate tool. If one method fails, it provides clear instructions on what to install.

### File Logging

Every transcription session creates a new timestamped file in the `conversations/` directory:

```
conversations/
├── transcription_20250315_143022.txt
├── transcription_20250315_150534.txt
└── ...
```

Each file contains timestamped entries:

```
[2025-03-15 14:30:22] Hello, this is my first transcription.
[2025-03-15 14:30:35] This is another segment of speech.
```

### Terminal Display

Two types of output:
- `[PARTIAL]` - Streaming transcription as you speak (may change)
- `[FINAL]` - Confirmed transcription (saved to file and typed)

### Error Recovery

If keyboard typing fails (permissions, focus issues, etc.):
- Error is logged to terminal
- Transcription is still saved to file
- Session continues running
- You can retrieve lost text from the conversation log

## Troubleshooting

### Connection randomly drops or stops

The transcriber now includes keepalive functionality to prevent timeouts during silence. If the connection still drops:

1. **Check the terminal output** - it will show the close code and reason:
   - Code 1000: Normal closure (you or the server intentionally stopped)
   - Code 1006: Network issue or timeout (check your internet connection)
   - Other codes: Check the error message for details

2. **Check your internet connection** - unstable network can cause drops

3. **Check the logs** - Look at `.transcribe.log` (if using toggle script) or the terminal output for error messages

4. **API issues** - Rarely, OpenAI's API might have issues. Check their status page.

If you see repeated disconnections, please note the close code and message - this helps diagnose the issue.

### No audio input detected

Check your default microphone:
```bash
# List audio devices
uv run python -c "import pyaudio; pa = pyaudio.PyAudio(); [print(f'{i}: {pa.get_device_info_by_index(i)[\"name\"]}') for i in range(pa.get_device_count())]"
```

### Keyboard typing not working

The tool will automatically detect and show which typing method it's using on startup:
- `✓ Keyboard typing: wtype (Wayland)` - Working correctly
- `⚠️ No typing method available` - Need to install a tool

**If typing isn't working:**
1. Check the startup message to see which method is being used
2. For **Wayland**: Install `wtype` (recommended) or `ydotool`
3. For **X11**: Install `xdotool`
4. Make sure the target window has focus
5. For ydotool: Ensure the daemon is running (`sudo systemctl status ydotool`)
6. If all else fails, text is still saved to the conversation log files

**Known issues:**
- `ydotool` sometimes types incorrect characters (use `wtype` instead)
- Some applications may block programmatic typing (security feature)

### API key errors

- Verify your API key is correct in `.env` or `.secrets`
- Make sure you have access to the OpenAI Realtime API
- Check your OpenAI account has sufficient credits

### Import errors

If you see module import errors, sync the dependencies:
```bash
uv sync
```

## Project Structure

```
transcriber/
├── .env.example          # Template for environment variables
├── .gitignore           # Git ignore rules
├── pyproject.toml       # Project configuration and dependencies
├── README.md            # This file
├── bar/                 # Quickshell bar widget
│   └── TranscriberStatus.qml  # Status indicator widget
├── conversations/       # Saved transcription logs (created on first run)
├── scripts/             # Utility scripts
│   ├── toggle_transcribe.sh  # Toggle transcription on/off
│   └── install_widget.sh     # Install Quickshell widget
├── tools/               # Diagnostic and testing tools
│   ├── test_audio_quality.py  # Test microphone audio quality
│   ├── test_typing.py   # Test keyboard typing functionality
│   └── analyze_ordering.py    # Analyze transcription ordering
└── transcriber/         # Main package
    ├── __init__.py
    ├── main.py          # Main transcription logic
    ├── deps.py          # System dependency checking
    └── typer.py         # Keyboard typing abstraction
```

## Dependencies

- `pyaudio` - Audio capture from microphone
- `websocket-client` - WebSocket connection to OpenAI
- `python-dotenv` - Environment variable management
- `pynput` - Keyboard automation for typing

## Diagnostic Tools

The `tools/` directory contains utilities for testing and debugging:

```bash
# Test microphone audio quality
uv run python tools/test_audio_quality.py

# Test keyboard typing functionality
uv run python tools/test_typing.py

# Analyze transcription ordering in debug logs
uv run python tools/analyze_ordering.py
```

See `tools/README.md` for more information.

## Tips

- Speak clearly and at a normal pace for best results
- The tool types with small delays - this is intentional for reliability
- All transcriptions are logged, so you can always recover text if typing fails
- You can have multiple transcription sessions - each creates a separate log file
- The `conversations/` directory is in `.gitignore` to keep your transcriptions private

## License

Use freely for personal or commercial projects.
