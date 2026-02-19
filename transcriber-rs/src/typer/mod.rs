mod backends;
mod detection;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::messages::TypeCommand;
use detection::TyperRules;

/// Run the Typer task.
///
/// Receives TypeCommand messages and types text into the focused window.
/// Uses `spawn_blocking` for subprocess calls to keep the async runtime responsive.
pub async fn run_typer_task(
    mut rx: mpsc::Receiver<TypeCommand>,
    cancel: CancellationToken,
) {
    let rules = TyperRules::load(None);

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

                        let rules_clone = rules.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            let window_class = detection::get_focused_window_class();
                            let method = rules_clone.get_method_for_window(&window_class);
                            debug!(window = %window_class, method = %method, "TYPER");
                            backends::type_with_adaptive(&text, &method)
                        })
                        .await;

                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => {
                                error!("Typing failed: {e}");
                            }
                            Err(e) => {
                                error!("Typer spawn_blocking panicked: {e}");
                            }
                        }
                    }
                    None => break,
                }
            }
        }
    }
}
