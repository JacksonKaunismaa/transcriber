mod backends;
mod detection;

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::messages::TypeCommand;
use detection::{TyperRules, WindowDetector};

/// Run the Typer task.
///
/// Receives TypeCommand messages and types text into the focused window.
/// Uses `spawn_blocking` for subprocess calls to keep the async runtime responsive.
pub async fn run_typer_task(
    mut rx: mpsc::Receiver<TypeCommand>,
    cancel: CancellationToken,
) {
    let mut rules = TyperRules::load(None);
    // Shared with the blocking closures so the re-resolved Hyprland instance
    // survives across utterances.
    let detector = Arc::new(Mutex::new(WindowDetector::new()));

    // Print initial status
    let status = if detection::test_adaptive() {
        "Keyboard typing: adaptive (per typer_rules.yaml)"
    } else {
        "Keyboard typing: not available (missing wl-copy/wtype/hyprctl)"
    };
    println!("[INFO] {status}");

    if !detection::test_adaptive() {
        eprintln!(
            "[WARNING] Keyboard typing not available!\n\
             Adaptive typing requires:\n  \
             - wl-copy: sudo pacman -S wl-clipboard\n  \
             - wtype: sudo pacman -S wtype\n  \
             - hyprctl: comes with Hyprland"
        );
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Typer task shutting down");
                break;
            }
            cmd = rx.recv() => {
                match cmd {
                    Some(TypeCommand { text }) => {
                        if text.trim().is_empty() {
                            continue;
                        }

                        rules.reload();
                        let rules_clone = rules.clone();
                        let detector = Arc::clone(&detector);
                        let blocking = tokio::task::spawn_blocking(move || {
                            let window_class = match detector.lock().unwrap().focused_window_class() {
                                Ok(class) => class,
                                Err(e) => {
                                    // Fall through to the default method, but loudly:
                                    // this exact silence hid a 10-day misroute in Aug 2026.
                                    warn!("Window detection failed, using default typing method: {e:#}");
                                    String::new()
                                }
                            };
                            let method = rules_clone.get_method_for_window(&window_class);
                            info!(window = %window_class, method = %method, "TYPER");
                            backends::type_with_adaptive(&text, &method)
                        });

                        // Timeout: don't let a hung subprocess block the typer forever
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            blocking,
                        )
                        .await
                        {
                            Ok(Ok(Ok(()))) => {}
                            Ok(Ok(Err(e))) => {
                                error!("Typing failed: {e}");
                            }
                            Ok(Err(e)) => {
                                error!("Typer spawn_blocking panicked: {e}");
                            }
                            Err(_) => {
                                error!("Typing timed out after 5s (subprocess may be hung)");
                            }
                        }
                    }
                    None => break,
                }
            }
        }
    }
}
