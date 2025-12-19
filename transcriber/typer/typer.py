"""
Robust keyboard typing automation with multiple fallback methods.

Supports:
- Wayland (Shift+Insert paste via wl-copy, wtype, ydotool)
- X11 (xdotool)
- Python fallbacks (pynput)
- Clipboard fallback

The preferred method on Wayland is adaptive: wtype for most apps,
Shift+Insert for kitty (which is faster and uses keyboard focus).
"""

import os
import sys
from typing import Callable, Optional

from . import backends
from . import detection


class KeyboardTyper:
    """Handles keyboard typing with automatic fallback between methods."""

    def __init__(self):
        self.display_server = os.environ.get("XDG_SESSION_TYPE", "unknown")
        self.method: Optional[Callable[[str], bool]] = None
        self.method_name: Optional[str] = None

        self._detect_method()

    def _detect_method(self):
        """Detect and select the best available typing method."""

        if self.display_server == "wayland":
            # Adaptive is preferred: wtype for most apps, Shift+Insert for kitty
            if detection.test_adaptive():
                self.method = backends.type_with_adaptive
                self.method_name = "adaptive (wtype / Shift+Insert for kitty)"
                return

            # Middle-click fallback if hyprctl not available
            if detection.test_middle_click():
                self.method = backends.type_with_middle_click
                self.method_name = "middle-click paste (Wayland)"
                return

            if detection.test_shift_insert():
                self.method = backends.type_with_shift_insert
                self.method_name = "Shift+Insert paste (Wayland)"
                return

            if detection.test_wtype():
                self.method = backends.type_with_wtype
                self.method_name = "wtype (Wayland)"
                return

            if detection.test_ydotool():
                self.method = backends.type_with_ydotool
                self.method_name = "ydotool (Wayland)"
                return

        elif self.display_server == "x11":
            if detection.test_xdotool(self.display_server):
                self.method = backends.type_with_xdotool
                self.method_name = "xdotool (X11)"
                return

        else:
            # Unknown display server, try all tools
            if detection.test_adaptive():
                self.method = backends.type_with_adaptive
                self.method_name = "adaptive (wtype / Shift+Insert for kitty)"
                return

            if detection.test_middle_click():
                self.method = backends.type_with_middle_click
                self.method_name = "middle-click paste (Wayland)"
                return

            if detection.test_shift_insert():
                self.method = backends.type_with_shift_insert
                self.method_name = "Shift+Insert paste (Wayland)"
                return

            if detection.test_wtype():
                self.method = backends.type_with_wtype
                self.method_name = "wtype (Wayland)"
                return

            if detection.test_xdotool(self.display_server):
                self.method = backends.type_with_xdotool
                self.method_name = "xdotool (X11)"
                return

            if detection.test_ydotool():
                self.method = backends.type_with_ydotool
                self.method_name = "ydotool (Wayland)"
                return

        # Python fallback
        if detection.test_pynput():
            self.method = backends.type_with_pynput
            self.method_name = "pynput (Python)"
            return

        # Last resort: clipboard
        if detection.test_clipboard(self.display_server):
            self.method = self._type_with_clipboard_wrapper
            self.method_name = "clipboard (fallback)"
            return

        self.method = None
        self.method_name = "none (no typing available)"

    def _type_with_clipboard_wrapper(self, text: str) -> bool:
        """Wrapper to pass display_server to clipboard backend."""
        return backends.type_with_clipboard(text, self.display_server)

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
            return "No typing method available - text will only be logged"
        else:
            return f"Keyboard typing: {self.method_name}"

    def get_setup_instructions(self) -> Optional[str]:
        """Get setup instructions if the current method needs configuration."""
        if self.method is None:
            instructions = []

            if self.display_server == "wayland":
                instructions.append("For Wayland, install one of:")
                instructions.append("  - wtype + wl-clipboard: sudo pacman -S wtype wl-clipboard")
                instructions.append("  - ydotool: sudo pacman -S ydotool  (then: sudo systemctl enable --now ydotool)")
            elif self.display_server == "x11":
                instructions.append("For X11, install:")
                instructions.append("  - xdotool: sudo pacman -S xdotool")
            else:
                instructions.append("Install a typing tool:")
                instructions.append("  - For Wayland: sudo pacman -S wtype wl-clipboard")
                instructions.append("  - For X11: sudo pacman -S xdotool")

            instructions.append("\nOr install Python fallback:")
            instructions.append("  - uv add pynput")

            return "\n".join(instructions)

        return None
