"""
Robust keyboard typing automation with multiple fallback methods.

Supports:
- Wayland (Shift+Insert paste via wl-copy, wtype, ydotool)
- X11 (xdotool)
- Python fallbacks (pynput)
- Clipboard fallback

The preferred method on Wayland is Shift+Insert paste, which is ~70x faster
than wtype keystroke simulation and doesn't trigger Claude Code crashes.
"""

import subprocess
import shutil
import os
import sys
import json
from typing import Optional


class KeyboardTyper:
    """Handles keyboard typing with automatic fallback between methods."""

    def __init__(self):
        self.display_server = os.environ.get("XDG_SESSION_TYPE", "unknown")
        self.method = None
        self.method_name = None

        # Try to find a working method
        self._detect_method()

    def _detect_method(self):
        """Detect and select the best available typing method."""

        if self.display_server == "wayland":
            # Wayland-specific tools
            # Adaptive is preferred: wtype for most apps, Shift+Insert for kitty
            if self._test_adaptive():
                self.method = self._type_with_adaptive
                self.method_name = "adaptive (wtype / Shift+Insert for kitty)"
                return

            # Middle-click fallback if hyprctl not available
            if self._test_middle_click():
                self.method = self._type_with_middle_click
                self.method_name = "middle-click paste (Wayland)"
                return

            # Shift+Insert fallback: Chromium ignores PRIMARY selection
            if self._test_shift_insert():
                self.method = self._type_with_shift_insert
                self.method_name = "Shift+Insert paste (Wayland)"
                return

            if self._test_wtype():
                self.method = self._type_with_wtype
                self.method_name = "wtype (Wayland)"
                return

            if self._test_ydotool():
                self.method = self._type_with_ydotool
                self.method_name = "ydotool (Wayland)"
                return

        elif self.display_server == "x11":
            # X11-specific tools
            if self._test_xdotool():
                self.method = self._type_with_xdotool
                self.method_name = "xdotool (X11)"
                return

        else:
            # Unknown display server, try all tools
            if self._test_adaptive():
                self.method = self._type_with_adaptive
                self.method_name = "adaptive (wtype / Shift+Insert for kitty)"
                return

            if self._test_middle_click():
                self.method = self._type_with_middle_click
                self.method_name = "middle-click paste (Wayland)"
                return

            if self._test_shift_insert():
                self.method = self._type_with_shift_insert
                self.method_name = "Shift+Insert paste (Wayland)"
                return

            if self._test_wtype():
                self.method = self._type_with_wtype
                self.method_name = "wtype (Wayland)"
                return

            if self._test_xdotool():
                self.method = self._type_with_xdotool
                self.method_name = "xdotool (X11)"
                return

            if self._test_ydotool():
                self.method = self._type_with_ydotool
                self.method_name = "ydotool (Wayland)"
                return

        # Try Python fallback
        if self._test_pynput():
            self.method = self._type_with_pynput
            self.method_name = "pynput (Python)"
            return

        # Last resort: clipboard
        if self._test_clipboard():
            self.method = self._type_with_clipboard
            self.method_name = "clipboard (fallback)"
            return

        # Nothing works
        self.method = None
        self.method_name = "none (no typing available)"

    def _test_middle_click(self) -> bool:
        """Test if middle-click paste method is available (requires wl-copy and wlrctl)."""
        if not shutil.which("wl-copy"):
            return False
        if not shutil.which("wlrctl"):
            return False
        return True

    def _test_adaptive(self) -> bool:
        """Test if adaptive method is available (middle-click + wtype + hyprctl)."""
        if not shutil.which("wl-copy"):
            return False
        if not shutil.which("wlrctl"):
            return False
        if not shutil.which("wtype"):
            return False
        if not shutil.which("hyprctl"):
            return False
        return True

    def _get_focused_window_class(self) -> str:
        """Get the class of the focused window via hyprctl."""
        try:
            result = subprocess.run(
                ["hyprctl", "activewindow", "-j"],
                capture_output=True,
                timeout=1,
            )
            if result.returncode != 0:
                return ""

            window_info = json.loads(result.stdout)
            return window_info.get("class", "").lower()
        except Exception:
            return ""

    def _is_chromium_focused(self) -> bool:
        """Check if the focused window is a Chromium-based app."""
        window_class = self._get_focused_window_class()
        chromium_classes = [
            "chromium", "google-chrome", "brave", "brave-browser",
            "microsoft-edge", "vivaldi", "opera", "electron",
        ]
        return any(c in window_class for c in chromium_classes)

    def _is_kitty_focused(self) -> bool:
        """Check if the focused window is kitty terminal."""
        window_class = self._get_focused_window_class()
        return "kitty" in window_class

    def _test_shift_insert(self) -> bool:
        """Test if Shift+Insert paste method is available (requires wl-copy and wtype)."""
        if not shutil.which("wl-copy"):
            return False
        if not shutil.which("wtype"):
            return False
        return True

    def _test_ydotool(self) -> bool:
        """Test if ydotool is available and working."""
        if not shutil.which("ydotool"):
            return False

        try:
            # Test ydotool by running a harmless command
            result = subprocess.run(
                ["ydotool", "type", "--help"],
                capture_output=True,
                timeout=2
            )
            return result.returncode == 0
        except:
            return False

    def _test_wtype(self) -> bool:
        """Test if wtype is available and working."""
        if not shutil.which("wtype"):
            return False

        try:
            # Test wtype by checking its help
            result = subprocess.run(
                ["wtype", "-h"],
                capture_output=True,
                timeout=2
            )
            # wtype returns non-zero for -h, but that's ok if it exists
            return True
        except:
            return False

    def _test_xdotool(self) -> bool:
        """Test if xdotool is available and working."""
        if not shutil.which("xdotool"):
            return False

        # On Wayland, xdotool might exist but not work
        if self.display_server == "wayland":
            return False

        try:
            result = subprocess.run(
                ["xdotool", "version"],
                capture_output=True,
                timeout=2
            )
            return result.returncode == 0
        except:
            return False

    def _test_pynput(self) -> bool:
        """Test if pynput is available."""
        try:
            from pynput.keyboard import Controller
            return True
        except ImportError:
            return False

    def _test_clipboard(self) -> bool:
        """Test if clipboard tools are available."""
        if self.display_server == "wayland":
            return shutil.which("wl-copy") is not None
        else:
            return shutil.which("xclip") is not None

    def _type_with_adaptive(self, text: str) -> bool:
        """
        Adaptive typing based on focused window.

        - Kitty: Shift+Insert (fast, keyboard focus, PRIMARY works)
        - Everything else: wtype (keyboard focus)
        """
        if self._is_kitty_focused():
            return self._type_with_shift_insert(text)
        else:
            return self._type_with_wtype(text)

    def _type_with_middle_click(self, text: str) -> bool:
        """
        Type text using middle-click paste via PRIMARY selection.

        This method works in all apps (terminals, Firefox, Chromium) unlike
        Shift+Insert which Chromium ignores. Text is chunked at 801 chars to
        avoid Claude Code's "[Pasted text]" display threshold.
        """
        # Chunk size: 801 chars shows actual text, 802+ shows "[Pasted text]"
        CHUNK_SIZE = 801

        text_with_space = text + " "

        try:
            # Split into chunks
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

    def _type_with_shift_insert(self, text: str) -> bool:
        """
        Type text using Shift+Insert paste via PRIMARY selection.

        This method is ~70x faster than wtype keystroke simulation and doesn't
        trigger crashes in Claude Code's TUI. Text is chunked at 801 chars to
        avoid Claude Code's "[Pasted text]" display threshold.
        """
        # Chunk size: 801 chars shows actual text, 802+ shows "[Pasted text]"
        CHUNK_SIZE = 801

        text_with_space = text + " "

        try:
            # Split into chunks
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

    def _type_with_ydotool(self, text: str) -> bool:
        """Type text using ydotool (Wayland)."""
        try:
            # ydotool needs the daemon running, but let's try anyway
            subprocess.run(
                ["ydotool", "type", text + " "],
                check=True,
                capture_output=True,
                timeout=5
            )
            return True
        except subprocess.CalledProcessError as e:
            # Check if it's a permission issue
            if b"permission" in e.stderr.lower() or b"failed to connect" in e.stderr.lower():
                print(f"\n[WARNING] ydotool permission denied. Run: sudo chmod 666 /tmp/.ydotool_socket", file=sys.stderr)
            raise

    def _type_with_wtype(self, text: str) -> bool:
        """Type text using wtype (Wayland), handling keycode 22 bug."""
        try:
            chunks = self._split_for_wtype_keycode22(text + " ")  # Include trailing space
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

    def _split_for_wtype_keycode22(self, text: str) -> list:
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
            # Find where position 14 would be for this chunk
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

            # Check if position 14 has unsafe punct
            if pos14_index is not None and text[pos14_index] in UNSAFE_AT_22:
                # Need to split before pos14. Find split point at last alnum.
                if last_alnum_before_14 is not None and last_alnum_before_14 > start:
                    # Split right after the character BEFORE last_alnum_before_14
                    # so next chunk starts with that alnum
                    chunks.append(text[start:last_alnum_before_14])
                    start = last_alnum_before_14
                    continue

            # No split needed, take the rest
            chunks.append(text[start:])
            break

        return chunks

    def _type_with_xdotool(self, text: str) -> bool:
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

    def _type_with_pynput(self, text: str) -> bool:
        """Type text using pynput Python library."""
        try:
            from pynput.keyboard import Controller
            keyboard = Controller()
            keyboard.type(text + " ")
            return True
        except Exception:
            raise

    def _type_with_clipboard(self, text: str) -> bool:
        """Copy text to clipboard as fallback."""
        try:
            if self.display_server == "wayland":
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

    def type_text(self, text: str) -> bool:
        """
        Type the given text using the best available method.

        Returns:
            bool: True if successful, False if failed
        """
        if not text.strip():
            return True

        if self.method is None:
            print(f"\n[ERROR] No typing method available!", file=sys.stderr)
            return False

        try:
            return self.method(text)
        except Exception as e:
            print(f"\n[ERROR] Typing failed with {self.method_name}: {e}", file=sys.stderr)
            return False

    def get_status_message(self) -> str:
        """Get a status message about the current typing method."""
        if self.method is None:
            return "⚠️  No typing method available - text will only be logged"
        else:
            return f"✓ Keyboard typing: {self.method_name}"

    def get_setup_instructions(self) -> Optional[str]:
        """Get setup instructions if the current method needs configuration."""
        if self.method is None:
            instructions = []

            if self.display_server == "wayland":
                instructions.append("For Wayland, install one of:")
                instructions.append("  • wlrctl + wl-clipboard (recommended): yay -S wlrctl && sudo pacman -S wl-clipboard")
                instructions.append("  • wtype + wl-clipboard: sudo pacman -S wtype wl-clipboard")
                instructions.append("  • ydotool: sudo pacman -S ydotool  (then: sudo systemctl enable --now ydotool)")
            elif self.display_server == "x11":
                instructions.append("For X11, install:")
                instructions.append("  • xdotool: sudo pacman -S xdotool")
            else:
                instructions.append("Install a typing tool:")
                instructions.append("  • For Wayland: yay -S wlrctl && sudo pacman -S wl-clipboard  (recommended)")
                instructions.append("  • For X11: sudo pacman -S xdotool")

            instructions.append("\nOr install Python fallback:")
            instructions.append("  • uv add pynput")

            return "\n".join(instructions)

        return None
