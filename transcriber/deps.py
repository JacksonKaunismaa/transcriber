"""System dependency checking for transcriber."""

import shutil
import sys
import platform
import os


def check_system_dependencies() -> bool:
    """
    Check for required system dependencies and print warnings if missing.

    Returns:
        bool: True if all critical dependencies are present, False otherwise
    """
    missing_deps = []
    warnings = []

    system = platform.system()
    display_server = os.environ.get("XDG_SESSION_TYPE", "unknown")

    # Check for keyboard typing tools (Linux only)
    if system == "Linux":
        has_typing_tool = False

        if display_server == "wayland":
            # Check for Wayland typing tools (prefer wtype as it's more reliable)
            if shutil.which("wtype"):
                has_typing_tool = True
            elif shutil.which("ydotool"):
                has_typing_tool = True

            if not has_typing_tool:
                missing_deps.append({
                    "name": "wtype or ydotool",
                    "purpose": "keyboard typing automation (Wayland)",
                    "install": {
                        "Recommended": "sudo pacman -S wtype  (most reliable)",
                        "Ubuntu/Debian": "sudo apt-get install wtype  OR  sudo apt-get install ydotool",
                        "Fedora": "sudo dnf install wtype  OR  sudo dnf install ydotool",
                        "Arch": "sudo pacman -S wtype  OR  sudo pacman -S ydotool",
                        "Note": "wtype is preferred; ydotool requires: sudo systemctl enable --now ydotool"
                    },
                    "critical": False  # Not critical since we have fallbacks
                })

        elif display_server == "x11":
            # Check for X11 typing tools
            if shutil.which("xdotool"):
                has_typing_tool = True

            if not has_typing_tool:
                missing_deps.append({
                    "name": "xdotool",
                    "purpose": "keyboard typing automation (X11)",
                    "install": {
                        "Ubuntu/Debian": "sudo apt-get install xdotool",
                        "Fedora": "sudo dnf install xdotool",
                        "Arch": "sudo pacman -S xdotool",
                        "openSUSE": "sudo zypper install xdotool"
                    },
                    "critical": False
                })

        else:
            # Unknown display server - check for any typing tool
            if shutil.which("xdotool") or shutil.which("ydotool") or shutil.which("wtype"):
                has_typing_tool = True

            if not has_typing_tool:
                missing_deps.append({
                    "name": "xdotool, ydotool, or wtype",
                    "purpose": "keyboard typing automation",
                    "install": {
                        "For Wayland": "sudo pacman -S ydotool  OR  sudo pacman -S wtype",
                        "For X11": "sudo pacman -S xdotool"
                    },
                    "critical": False
                })

    # Check for notify-send (optional, for desktop notifications)
    if system == "Linux":
        if not shutil.which("notify-send"):
            warnings.append({
                "name": "notify-send (libnotify)",
                "purpose": "desktop notifications (optional)",
                "install": {
                    "Ubuntu/Debian": "sudo apt-get install libnotify-bin",
                    "Fedora": "sudo dnf install libnotify",
                    "Arch": "sudo pacman -S libnotify"
                },
                "critical": False
            })

    # Print warnings
    has_critical_missing = any(dep["critical"] for dep in missing_deps)

    if missing_deps or warnings:
        print("=" * 70)
        print("SYSTEM DEPENDENCY CHECK")
        print("=" * 70)

    if missing_deps:
        print("\n⚠️  MISSING REQUIRED DEPENDENCIES:\n")
        for dep in missing_deps:
            print(f"  • {dep['name']} - needed for {dep['purpose']}")
            print(f"    Installation instructions:")
            for distro, cmd in dep['install'].items():
                print(f"      {distro:20s} {cmd}")
            print()

        if has_critical_missing:
            print("⛔ CRITICAL: The transcriber will not function properly without these tools!")
            print("   Install the required dependencies and try again.\n")
            print("=" * 70)
            return False

    if warnings:
        if missing_deps:
            print("-" * 70)
        print("\n⚡ OPTIONAL DEPENDENCIES (recommended but not required):\n")
        for dep in warnings:
            print(f"  • {dep['name']} - {dep['purpose']}")
            print(f"    Installation instructions:")
            for distro, cmd in dep['install'].items():
                print(f"      {distro:20s} {cmd}")
            print()

    if missing_deps or warnings:
        print("=" * 70)
        print()

    return True
