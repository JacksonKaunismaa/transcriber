# Real-Time Transcription Tool

A real-time audio transcription tool that captures your microphone input, transcribes it using OpenAI's Realtime API, and automatically types the text into any active window. All transcriptions are also saved to files and displayed in the terminal.

## Features

- **Real-time transcription** using OpenAI's GPT-4 Realtime API
- **Automatic typing** into any active window (works everywhere!)
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

For audio capture to work, you'll need PortAudio:

```bash
# Ubuntu/Debian
sudo apt-get install portaudio19-dev python3-pyaudio

# Fedora
sudo dnf install portaudio-devel

# Arch
sudo pacman -S portaudio
```

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

To see all options:
```bash
uv run transcribe --help
```

### What happens when you run it:

1. Connects to OpenAI's Realtime API
2. Starts capturing audio from your default microphone
3. Transcribes speech in real-time
4. Types the transcription into whatever window/app is active
5. Displays transcriptions in the terminal (with [PARTIAL] and [FINAL] markers)
6. Saves all final transcriptions to `conversations/transcription_YYYYMMDD_HHMMSS.txt`

### Stopping the transcription:

Press `Ctrl+C` to gracefully stop the session. The location of your saved transcription will be displayed.

## How It Works

### Keyboard Automation

The tool uses `pynput` to simulate keyboard typing. Text is typed in small chunks (10 characters at a time) with slight delays to ensure reliability across different applications.

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

### No audio input detected

Check your default microphone:
```bash
# List audio devices
uv run python -c "import pyaudio; pa = pyaudio.PyAudio(); [print(f'{i}: {pa.get_device_info_by_index(i)[\"name\"]}') for i in range(pa.get_device_count())]"
```

### Keyboard typing not working

- Make sure the target window is focused
- On some Linux systems, you may need to run with appropriate permissions
- Check that `pynput` has accessibility permissions (especially on macOS)

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
├── conversations/       # Saved transcription logs (created on first run)
└── transcriber/
    ├── __init__.py
    └── main.py          # Main transcription logic
```

## Dependencies

- `pyaudio` - Audio capture from microphone
- `websocket-client` - WebSocket connection to OpenAI
- `python-dotenv` - Environment variable management
- `pynput` - Keyboard automation for typing

## Tips

- Speak clearly and at a normal pace for best results
- The tool types with small delays - this is intentional for reliability
- All transcriptions are logged, so you can always recover text if typing fails
- You can have multiple transcription sessions - each creates a separate log file
- The `conversations/` directory is in `.gitignore` to keep your transcriptions private

## License

Use freely for personal or commercial projects.
