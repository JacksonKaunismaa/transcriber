"""
Keyboard typing automation with configurable window-based method selection.

Methods are configured in typer_rules.yaml and selected per-window.
"""

import logging
import sys
from typing import Optional

from . import backends
from . import detection
from .detection import TyperRules


class KeyboardTyper:
    """Handles keyboard typing with per-window method selection via typer_rules.yaml."""

    def __init__(self):
        self.logger: Optional[logging.Logger] = None
        self._rules = TyperRules()

    def set_logger(self, logger: logging.Logger):
        """Set logger for typing debug output."""
        self.logger = logger

    def type_text(self, text: str) -> bool:
        """
        Type the given text using the method configured for the focused window.

        Returns:
            bool: True if successful, False if failed
        """
        if not text.strip():
            return True

        window_class = detection.get_focused_window_class()
        method = self._rules.get_method_for_window(window_class)

        if self.logger:
            self.logger.debug(f'"TYPER window={window_class!r} method={method}"')

        try:
            return backends.type_with_adaptive(text, self._rules, window_class)
        except Exception as e:
            print(f"\n[ERROR] Typing failed: {e}", file=sys.stderr)
            return False

    def get_status_message(self) -> str:
        """Get a status message about the current typing method."""
        return "Keyboard typing: adaptive (per typer_rules.yaml)"

    def get_setup_instructions(self) -> Optional[str]:
        """Get setup instructions if tools are missing."""
        if not detection.test_adaptive():
            return (
                "Adaptive typing requires:\n"
                "  - wl-copy: sudo pacman -S wl-clipboard\n"
                "  - wtype: sudo pacman -S wtype\n"
                "  - hyprctl: comes with Hyprland"
            )
        return None
