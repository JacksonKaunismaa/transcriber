"""Tool availability detection and window identification."""

import json
import shutil
import subprocess
from pathlib import Path
from typing import Optional

import yaml


def test_middle_click() -> bool:
    """Test if middle-click paste method is available (requires wl-copy and wlrctl)."""
    if not shutil.which("wl-copy"):
        return False
    if not shutil.which("wlrctl"):
        return False
    return True


def test_adaptive() -> bool:
    """Test if adaptive method is available (wtype + hyprctl for window detection)."""
    if not shutil.which("wl-copy"):
        return False
    if not shutil.which("wtype"):
        return False
    if not shutil.which("hyprctl"):
        return False
    return True


def test_shift_insert() -> bool:
    """Test if Shift+Insert paste method is available (requires wl-copy and wtype)."""
    if not shutil.which("wl-copy"):
        return False
    if not shutil.which("wtype"):
        return False
    return True


def test_ydotool() -> bool:
    """Test if ydotool is available and working."""
    if not shutil.which("ydotool"):
        return False

    try:
        result = subprocess.run(
            ["ydotool", "type", "--help"],
            capture_output=True,
            timeout=2
        )
        return result.returncode == 0
    except Exception:
        return False


def test_wtype() -> bool:
    """Test if wtype is available and working."""
    if not shutil.which("wtype"):
        return False

    try:
        subprocess.run(
            ["wtype", "-h"],
            capture_output=True,
            timeout=2
        )
        # wtype returns non-zero for -h, but that's ok if it exists
        return True
    except Exception:
        return False


def test_xdotool(display_server: str) -> bool:
    """Test if xdotool is available and working."""
    if not shutil.which("xdotool"):
        return False

    # On Wayland, xdotool might exist but not work
    if display_server == "wayland":
        return False

    try:
        result = subprocess.run(
            ["xdotool", "version"],
            capture_output=True,
            timeout=2
        )
        return result.returncode == 0
    except Exception:
        return False


def test_pynput() -> bool:
    """Test if pynput is available."""
    try:
        from pynput.keyboard import Controller  # noqa: F401
        return True
    except ImportError:
        return False


def test_clipboard(display_server: str) -> bool:
    """Test if clipboard tools are available."""
    if display_server == "wayland":
        return shutil.which("wl-copy") is not None
    else:
        return shutil.which("xclip") is not None


def get_focused_window_class() -> str:
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


class TyperRules:
    """Load and manage typer rules from YAML config with dynamic reload."""

    def __init__(self, config_path: Optional[Path] = None):
        if config_path is None:
            config_path = Path(__file__).parent.parent / "typer_rules.yaml"
        self._config_path = config_path
        self._mtime: float = 0
        self._rules: list = []
        self._default: str = "wtype"
        self._reload()

    def _reload(self):
        """Reload rules from config file if modified."""
        try:
            if not self._config_path.exists():
                return

            mtime = self._config_path.stat().st_mtime
            if mtime == self._mtime:
                return

            with open(self._config_path) as f:
                config = yaml.safe_load(f) or {}

            self._rules = config.get("rules", [])
            self._default = config.get("default", "wtype")
            self._mtime = mtime
        except Exception:
            pass

    def get_method_for_window(self, window_class: str) -> str:
        """Get the typing method to use for the given window class."""
        self._reload()

        window_lower = window_class.lower()
        for rule in self._rules:
            match = rule.get("match", "").lower()
            if match and match in window_lower:
                return rule.get("method", self._default)

        return self._default
