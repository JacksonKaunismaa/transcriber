use std::process::Command;

/// Check for required system dependencies and print warnings.
/// Mirrors the Python `deps.py` — warns about missing typing tools but doesn't hard-fail.
pub fn check_system_dependencies() {
    let display_server = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());

    let mut missing = Vec::new();
    let mut warnings = Vec::new();

    // Check for keyboard typing tools (Linux only)
    if cfg!(target_os = "linux") {
        let has_typing_tool = match display_server.as_str() {
            "wayland" => which("wtype") || which("ydotool"),
            "x11" => which("xdotool"),
            _ => which("xdotool") || which("ydotool") || which("wtype"),
        };

        if !has_typing_tool {
            missing.push((
                "wtype or ydotool",
                "keyboard typing automation",
                "sudo pacman -S wtype",
            ));
        }
    }

    // Check for notify-send (optional)
    if cfg!(target_os = "linux") && !which("notify-send") {
        warnings.push(("notify-send (libnotify)", "desktop notifications (optional)"));
    }

    if !missing.is_empty() || !warnings.is_empty() {
        println!("{}", "=".repeat(70));
        println!("SYSTEM DEPENDENCY CHECK");
        println!("{}", "=".repeat(70));
    }

    if !missing.is_empty() {
        println!("\n\u{26a0}\u{fe0f}  MISSING DEPENDENCIES:\n");
        for (name, purpose, install) in &missing {
            println!("  \u{2022} {name} - needed for {purpose}");
            println!("    Install: {install}");
            println!();
        }
    }

    if !warnings.is_empty() {
        println!("\n\u{26a1} OPTIONAL DEPENDENCIES:\n");
        for (name, purpose) in &warnings {
            println!("  \u{2022} {name} - {purpose}");
        }
        println!();
    }

    if !missing.is_empty() || !warnings.is_empty() {
        println!("{}", "=".repeat(70));
        println!();
    }
}

fn which(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .is_ok_and(|o| o.status.success())
}
