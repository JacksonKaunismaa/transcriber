"""Tool availability detection and window identification."""

import json
import shutil
import subprocess


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


def is_kitty_focused() -> bool:
    """Check if the focused window is kitty terminal."""
    window_class = get_focused_window_class()
    return "kitty" in window_class
