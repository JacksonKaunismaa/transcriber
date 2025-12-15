# Scripts

Utility scripts for managing and using the transcriber.

## Available Scripts

### `toggle_transcribe.sh`

Toggle transcription on/off with desktop notifications.

**Usage:**
```bash
./scripts/toggle_transcribe.sh
```

**Features:**
- Starts transcription if not running
- Stops transcription if already running
- Shows desktop notifications (KDE and libnotify)
- Tracks PID to ensure single instance
- Logs output to `.transcribe.log`

**Setting up a keyboard shortcut:**

See the main README for instructions on binding this to a hotkey (e.g., Ctrl+F6) in various desktop environments.

### `install_widget.sh`

Install the TranscriberStatus widget into a Quickshell bar.

**Usage:**
```bash
./scripts/install_widget.sh
```

**What it does:**
- Symlinks `bar/TranscriberStatus.qml` into your Quickshell ii bar config
- Adds the widget import to `BarContent.qml` if not present
- Shows transcriber status in your bar (gray=off, green=running, orange=mic muted)

**Requirements:**
- Quickshell with the ii config installed at `~/.config/quickshell/ii/`

### `time_saved.py`

Generate a report showing time saved by voice transcription vs manual typing.

**Usage:**
```bash
uv run scripts/time_saved.py
```

**What it does:**
- Analyzes `conversations/typing_log.txt` to measure actual typing time
- Uses pre-computed ratio distributions from `ratio_distributions.json`
- Estimates how long manual typing would have taken
- Generates a plot showing cumulative time saved over time

**Output:**
- Prints summary statistics to terminal
- Saves plot to `scripts/time_saved.png`

## Adding New Scripts

When adding utility scripts:
1. Use descriptive names with `.sh` extension for shell scripts
2. Add a header comment explaining purpose and usage
3. Make scripts executable: `chmod +x scripts/your_script.sh`
4. Update this README with documentation
