# Real-Time Transcription Tool

A real-time audio transcription tool that captures your microphone input, transcribes it using OpenAI's Realtime API, and automatically types the text into any active window. All transcriptions are also saved to files and displayed in the terminal.

## Design Philosophy

This tool is built for people who use AI assistants heavily and want to talk to them instead of type. The priorities are:

- **Simple** - One command to start, works with any application
- **Cheap** - Transcription costs ~$0.006/minute using the Whisper API
- **Open source** - No vendor lock-in, easy to modify
- **Reliable enough** - You shouldn't have to repeat yourself, but we don't obsess over perfect transcription accuracy. AI assistants can understand you even when the transcript is rough, so 95% accuracy is fine.

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

**Keyboard typing tool:**

For **Wayland** (install wtype and wl-clipboard):
```bash
# Arch
sudo pacman -S wtype wl-clipboard

# Ubuntu/Debian
sudo apt-get install wtype wl-clipboard

# From source (other distros)
# See: https://github.com/atx/wtype
```

For **X11** (install xdotool):
```bash
# Ubuntu/Debian
sudo apt-get install xdotool

# Arch
sudo pacman -S xdotool
```

The tool automatically detects your display server. On Wayland, it uses Shift+Insert paste (via wl-copy and wtype) which is faster and more reliable than keystroke simulation. On X11, it uses xdotool.

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

# Don't save transcriptions to conversations/ (privacy mode)
uv run transcribe --no-log
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

For Hyprland users with [Quickshell](https://quickshell.outfoxxed.me/) using the [end-4/dots-hyprland](https://github.com/end-4/dots-hyprland) config, you can add a status indicator to your bar.

```bash
./scripts/install_widget.sh
```

This adds a TranscriberStatus widget to your bar:
- **Gray** - Transcriber is off
- **Green** - Transcriber is running
- **Orange** - Running but mic is muted
- **Click** - Toggle transcriber on/off

Restart Quickshell after installing: `qs reload`

## How It Works

### Keyboard Automation

The tool detects your display server and uses the best available method:
- **Shift+Insert paste** on Wayland (preferred) - Uses wl-copy to set the PRIMARY selection, then wtype to send Shift+Insert. This is ~70x faster than keystroke simulation and avoids crashes in terminal TUIs like Claude Code.
- **wtype** on Wayland (fallback) - Direct keystroke simulation if wl-copy is unavailable.
- **xdotool** on X11

If typing fails, transcriptions are still saved to the log files.

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

**If typing isn't working:**
1. For **Wayland**: Install `wtype` and `wl-clipboard`
2. For **X11**: Install `xdotool`
3. Make sure the target window has focus
4. Transcriptions are still saved to `conversations/` even if typing fails

Some applications (like certain Electron apps) may block programmatic typing as a security feature.

### API key errors

- Verify your API key is correct in `.env`
- Make sure you have access to the OpenAI Realtime API
- Check your OpenAI account has sufficient credits

### Import errors

If you see module import errors, sync the dependencies:
```bash
uv sync
```

## Time Saved Analysis

The `scripts/time_saved.py` script generates a report showing how much time you've saved using voice transcription versus typing. It uses pre-computed ratios for speech-to-core-idea compression and compares your speaking speed to typing speed.

```bash
uv sync --extra analysis  # Install scipy/matplotlib
uv run python scripts/time_saved.py
```

## License

MIT License - use freely for any purpose.
