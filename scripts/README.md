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

## Adding New Scripts

When adding utility scripts:
1. Use descriptive names with `.sh` extension for shell scripts
2. Add a header comment explaining purpose and usage
3. Make scripts executable: `chmod +x scripts/your_script.sh`
4. Update this README with documentation
