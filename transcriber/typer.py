"""
Robust keyboard typing automation with multiple fallback methods.

Supports:
- Wayland (ydotool, wtype)
- X11 (xdotool)
- Python fallbacks (pynput)
- Clipboard fallback
"""

import subprocess
import shutil
import os
import sys
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
            # Wayland-specific tools (wtype first as it's more reliable)
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
            # Unknown display server, try all tools (prefer wtype due to reliability)
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
                instructions.append("  • wtype (recommended): sudo pacman -S wtype")
                instructions.append("  • ydotool: sudo pacman -S ydotool  (then: sudo systemctl enable --now ydotool)")
            elif self.display_server == "x11":
                instructions.append("For X11, install:")
                instructions.append("  • xdotool: sudo pacman -S xdotool")
            else:
                instructions.append("Install a typing tool:")
                instructions.append("  • For Wayland: sudo pacman -S wtype  (recommended)")
                instructions.append("  • For X11: sudo pacman -S xdotool")

            instructions.append("\nOr install Python fallback:")
            instructions.append("  • uv add pynput")

            return "\n".join(instructions)

        return None
