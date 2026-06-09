# Real-Time Transcription Tool

A real-time audio transcription tool that captures your microphone input, transcribes it using OpenAI's Realtime API, and automatically types the text into any active window. All transcriptions are also saved to log files and displayed in the terminal.

## Design Philosophy

This tool is built for people who use AI assistants heavily and want to talk to them instead of type. The priorities are:

- **Simple** - One command to start, works with any application
- **Cheap** - Local Silero VAD gates audio to OpenAI, so you only pay for
  detected-speech minutes (not mic-on minutes). At $0.017/min for
  `gpt-realtime-whisper`, typical dictation runs well under $1/hr of usage.
- **Open source** - No vendor lock-in, easy to modify
- **Reliable enough** - You shouldn't have to repeat yourself, but we don't obsess over perfect transcription accuracy. AI assistants can understand you even when the transcript is rough, so 95% accuracy is fine.

## Features

- **Real-time transcription** using OpenAI's `gpt-realtime-whisper` (streaming Whisper)
- **Local Silero VAD** - utterance-aware sending; only billed for detected speech
- **Automatic typing** into any active window (works everywhere!)
- **Smart filtering** removes hallucinations, filler words, and non-ASCII characters by default
- **Hot-reload filters** - edit `filters.yaml` while running, no restart needed
- **Adaptive typing** - detects the focused window and picks the best typing method
- **Triple redundancy**: Terminal display + file logging + keyboard typing
- **Conversation archiving** in timestamped JSONL files
- **Graceful error handling** with recovery mechanisms

## Setup

### 1. Install Rust (if you haven't already)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. Clone and build

```bash
git clone https://github.com/JacksonKaunismaa/transcriber.git
cd transcriber
cd transcriber-rs && cargo build --release
```

The binary will be at `transcriber-rs/target/release/transcriber`.

The build pulls in a locally-patched copy of `cpal` from `transcriber-rs/vendor/cpal/`
via `[patch.crates-io]`. The patch fixes an upstream busy-spin
([cpal#785](https://github.com/RustAudio/cpal/issues/785)) that pegs one CPU
core whenever the mic is muted on PipeWire-Pulse setups — see
`transcriber-rs/vendor/cpal/PATCH.md` for what changed and how to update.

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

Run the binary from the project root (so it can find `filters.yaml` and `.env`):

```bash
./transcriber-rs/target/release/transcriber
```

### Latency / accuracy

Transcription uses OpenAI's `gpt-realtime-whisper` — the streaming Whisper
variant in the GA Realtime API. It pairs with local Silero VAD to endpoint
utterances (the API rejects server-side VAD for this model).

```bash
# Default — delay=high (most accurate)
./transcriber-rs/target/release/transcriber

# Snappier feel; slightly lower accuracy at sentence starts
./transcriber-rs/target/release/transcriber --delay low
```

`--delay` controls a server-side latency/accuracy tradeoff (`minimal`,
`low`, `medium`, `high`, `xhigh`). Higher = more audio buffered before
emission = more accurate, more lag. Default is `high`.

### Automatic Filtering

By default, the tool filters out common false positives to improve transcription quality:

**Filtered by default:**
- **Hallucinations** - "Bye.", "Thank you.", website URLs, subtitle watermarks, plus generic detection of repetitive text and impossibly fast speech
- **Filler words** - um, uh, hmm, mhm, etc.
- **Non-ASCII characters** - emojis, accented characters, CJK, etc.

Filter patterns are defined in `filters.yaml` and hot-reloaded when the file changes.

**Disable filtering when needed:**
```bash
# Disable hallucination filtering
./transcriber-rs/target/release/transcriber --allow-bye-thank-you

# Allow filler words (um, uh, hmm)
./transcriber-rs/target/release/transcriber --allow-fillers

# Allow non-ASCII characters
./transcriber-rs/target/release/transcriber --allow-non-ascii

# Don't save transcriptions to conversations/ (privacy mode)
./transcriber-rs/target/release/transcriber --no-log
```

The transcriber always writes a live VAD probability snapshot to
`$XDG_RUNTIME_DIR/transcriber_vad.json` (~160ms updates, atomic rename).
The `bar/VadDebugPanel.qml` Quickshell widget reads this via inotify
and renders a sparkline with threshold lines — useful for diagnosing
missed soft speech.

### Dumping audio on demand

Bind `scripts/dump_audio.sh` to a hotkey (e.g. in Hyprland) to send the
running transcriber a SIGHUP. The audio router writes its rolling 30s
fallback buffer to `conversations/audio_dumps/dump_YYYYMMDD_HHMMSS.wav`
(24kHz mono, raw PCM). Useful for capturing audio behind missed
transcriptions so they can be re-fed through different VADs.

To see all options:
```bash
./transcriber-rs/target/release/transcriber --help
```

### What happens when you run it:

1. Connects to OpenAI's Realtime API with keepalive enabled
2. Starts capturing audio from your default microphone
3. Transcribes speech in real-time
4. Types the transcription into whatever window/app is active
5. Displays transcriptions in the terminal (with [PARTIAL] and [FINAL] markers)
6. Saves all events to `conversations/debug_events_YYYYMMDD_HHMMSS.jsonl`

**The session will run indefinitely** - it won't timeout during periods of silence. The WebSocket connection sends keepalive pings every 5 seconds to maintain the connection, so you can leave it running as long as you need.

### Stopping the transcription:

Press `Ctrl+C` to gracefully stop the session.

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

### Voice Activity Detection

`gpt-realtime-whisper` doesn't support server-side VAD (the server rejects
`turn_detection` for that model), so the client has to endpoint utterances
itself. The transcriber runs the [Silero VAD](https://github.com/snakers4/silero-vad)
model locally on each 32ms audio frame:

- **Audio is gated by VAD state.** Bytes only flow to OpenAI while Silero
  says we're in speech. Silent periods are buffered in a 1-second rolling
  prefix that gets flushed when speech starts, so sentence-initial words
  aren't clipped.
- **Hysteresis** prevents flicker: 3 consecutive speech frames (96ms)
  declare speech started; 30 consecutive silence frames (960ms) declare
  it stopped and trigger an `input_audio_buffer.commit`.
- **Diagnostics** are logged into the JSONL — per-second heartbeats with
  silence-counter, min/max probability, and `peak_silence_in_burst` —
  so if a missed utterance ever happens, the cause (silero borderline,
  threshold too high, etc.) is recoverable from the log.

### Keyboard Automation

The tool detects your display server and uses the best available method:
- **Shift+Insert paste** on Wayland (preferred) - Uses wl-copy to set the PRIMARY selection, then wtype to send Shift+Insert. This is ~70x faster than keystroke simulation and avoids crashes in terminal TUIs like Claude Code.
- **wtype** on Wayland (fallback) - Direct keystroke simulation if wl-copy is unavailable.
- **xdotool** on X11

Per-window typing behavior can be customized in `typer_rules.yaml`.

If typing fails, transcriptions are still saved to the log files.

### File Logging

Every transcription session creates a new timestamped JSONL file in the `conversations/` directory:

```
conversations/
├── debug_events_20250315_143022.jsonl
├── debug_events_20250315_150534.jsonl
└── ...
```

Each file contains structured JSON events including transcriptions, metrics, and debug information.

### Terminal Display

Two types of output:
- `[PARTIAL]` - Streaming transcription as you speak (may change)
- `[FINAL]` - Confirmed transcription (saved to file and typed)

### Error Recovery

If keyboard typing fails (permissions, focus issues, etc.):
- Error is logged to terminal
- Transcription is still saved to the JSONL log
- Session continues running
- You can retrieve lost text from the conversation log

## Troubleshooting

### Connection randomly drops or stops

The transcriber includes keepalive functionality to prevent timeouts during silence. If the connection still drops:

1. **Check the terminal output** - it will show the close code and reason:
   - Code 1000: Normal closure (you or the server intentionally stopped)
   - Code 1006: Network issue or timeout (check your internet connection)
   - Other codes: Check the error message for details

2. **Check your internet connection** - unstable network can cause drops

3. **Check the logs** - Look at `.transcribe.log` (if using toggle script) or the JSONL files in `conversations/` for error details

4. **API issues** - Rarely, OpenAI's API might have issues. Check their status page.

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

## Time Saved Analysis (Optional)

The `scripts/time_saved.py` script generates a report showing how much time you've saved using voice transcription versus typing. Requires Python and [uv](https://docs.astral.sh/uv/):

```bash
uv run scripts/time_saved.py
```

Dependencies (matplotlib) are installed automatically via inline script metadata.

## License

MIT License - use freely for any purpose.
