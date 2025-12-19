"""Typing backend implementations for different tools and platforms."""

import subprocess
import sys

from .detection import is_kitty_focused

# Chunk size: 801 chars shows actual text, 802+ shows "[Pasted text]" in Claude Code
CHUNK_SIZE = 801


def type_with_adaptive(text: str) -> bool:
    """
    Adaptive typing based on focused window.

    - Kitty: Shift+Insert (fast, keyboard focus, PRIMARY works)
    - Everything else: wtype (keyboard focus)
    """
    if is_kitty_focused():
        return type_with_shift_insert(text)
    else:
        return type_with_wtype(text)


def type_with_middle_click(text: str) -> bool:
    """
    Type text using middle-click paste via PRIMARY selection.

    This method works in all apps (terminals, Firefox, Chromium) unlike
    Shift+Insert which Chromium ignores. Text is chunked at 801 chars to
    avoid Claude Code's "[Pasted text]" display threshold.
    """
    text_with_space = text + " "

    try:
        chunks = [
            text_with_space[i : i + CHUNK_SIZE]
            for i in range(0, len(text_with_space), CHUNK_SIZE)
        ]

        for chunk in chunks:
            # Copy to PRIMARY selection (--trim-newline prevents trailing newline)
            subprocess.run(
                ["wl-copy", "--primary", "--trim-newline"],
                input=chunk.encode(),
                check=True,
                timeout=2,
            )
            # Paste via middle-click
            subprocess.run(
                ["wlrctl", "pointer", "click", "middle"],
                check=True,
                capture_output=True,
                timeout=5,
            )
        return True
    except Exception:
        raise


def type_with_shift_insert(text: str) -> bool:
    """
    Type text using Shift+Insert paste via PRIMARY selection.

    This method is ~70x faster than wtype keystroke simulation and doesn't
    trigger crashes in Claude Code's TUI. Text is chunked at 801 chars to
    avoid Claude Code's "[Pasted text]" display threshold.
    """
    text_with_space = text + " "

    try:
        chunks = [
            text_with_space[i : i + CHUNK_SIZE]
            for i in range(0, len(text_with_space), CHUNK_SIZE)
        ]

        for chunk in chunks:
            # Copy to PRIMARY selection
            subprocess.run(
                ["wl-copy", "--primary"],
                input=chunk.encode(),
                check=True,
                timeout=2,
            )
            # Paste via Shift+Insert
            subprocess.run(
                ["wtype", "-M", "shift", "-k", "Insert", "-m", "shift"],
                check=True,
                capture_output=True,
                timeout=5,
            )
        return True
    except Exception:
        raise


def type_with_ydotool(text: str) -> bool:
    """Type text using ydotool (Wayland)."""
    try:
        subprocess.run(
            ["ydotool", "type", text + " "],
            check=True,
            capture_output=True,
            timeout=5
        )
        return True
    except subprocess.CalledProcessError as e:
        if b"permission" in e.stderr.lower() or b"failed to connect" in e.stderr.lower():
            print(f"\n[WARNING] ydotool permission denied. Run: sudo chmod 666 /tmp/.ydotool_socket", file=sys.stderr)
        raise


def type_with_wtype(text: str) -> bool:
    """Type text using wtype (Wayland), handling keycode 22 bug."""
    try:
        chunks = _split_for_wtype_keycode22(text + " ")
        for chunk in chunks:
            subprocess.run(
                ["wtype", chunk],
                check=True,
                capture_output=True,
                timeout=5
            )
        return True
    except Exception:
        raise


def _split_for_wtype_keycode22(text: str) -> list:
    """
    Fix wtype keycode 22 bug where punctuation at position 14 triggers BackSpace.

    wtype assigns keycodes starting at 9. If a punctuation char is the 14th
    unique character (keycode 9+13=22), it can be interpreted as BackSpace.

    Fix: split text so unsafe punct never lands at position 14. Split point is
    chosen so the new chunk starts with an alphanumeric.
    """
    UNSAFE_AT_22 = set(' !"#$\'()*+,-./:;=>?@[\\]^_')

    if not text:
        return []

    chunks = []
    start = 0

    while start < len(text):
        seen = set()
        pos14_index = None
        last_alnum_before_14 = None

        for i in range(start, len(text)):
            char = text[i]
            if char not in seen:
                seen.add(char)
                if len(seen) == 14:
                    pos14_index = i
                    break
            if char.isalnum():
                last_alnum_before_14 = i

        if pos14_index is not None and text[pos14_index] in UNSAFE_AT_22:
            if last_alnum_before_14 is not None and last_alnum_before_14 > start:
                chunks.append(text[start:last_alnum_before_14])
                start = last_alnum_before_14
                continue

        chunks.append(text[start:])
        break

    return chunks


def type_with_xdotool(text: str) -> bool:
    """Type text using xdotool (X11)."""
    try:
        subprocess.run(
            ["xdotool", "type", "--clearmodifiers", "--", text + " "],
            check=True,
            capture_output=True,
            timeout=5
        )
        return True
    except Exception:
        raise


def type_with_pynput(text: str) -> bool:
    """Type text using pynput Python library."""
    try:
        from pynput.keyboard import Controller
        keyboard = Controller()
        keyboard.type(text + " ")
        return True
    except Exception:
        raise


def type_with_clipboard(text: str, display_server: str) -> bool:
    """Copy text to clipboard as fallback."""
    try:
        if display_server == "wayland":
            subprocess.run(
                ["wl-copy"],
                input=(text + " ").encode(),
                check=True,
                timeout=2
            )
        else:
            subprocess.run(
                ["xclip", "-selection", "clipboard"],
                input=(text + " ").encode(),
                check=True,
                timeout=2
            )

        print(f"\n[INFO] Text copied to clipboard (typing not available): {text}", file=sys.stderr)
        return True
    except Exception:
        raise
