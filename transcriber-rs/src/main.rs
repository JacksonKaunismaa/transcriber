mod audio_buffer;
mod audio_device;
mod config;
mod deps;
mod error;
mod filters;
mod messages;
mod metrics;
mod transcript;
mod typer;
mod websocket;

use std::path::PathBuf;

use chrono::Local;
use clap::Parser;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

use config::Config;
use messages::{AudioChunk, AudioEvent, MetricsEvent, TranscriptEvent, TypeCommand, WsCommand};

/// Channel buffer sizes.
/// Audio: 1024 slots ≈ 43s at 24kHz/1024 frames.
/// Events: smaller since they're event-driven, not continuous.
const AUDIO_CHANNEL_SIZE: usize = 1024;
const EVENT_CHANNEL_SIZE: usize = 256;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config = Config::parse();
    let api_key = Config::api_key().ok_or(error::TranscriberError::MissingApiKey)?;

    // Set up logging
    let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let debug_log_file = if config.no_log {
        None
    } else {
        let dir = PathBuf::from("conversations");
        std::fs::create_dir_all(&dir)?;
        Some(dir.join(format!("debug_events_{timestamp}.jsonl")))
    };

    setup_tracing(&debug_log_file);

    println!();
    println!("{}", "=".repeat(60));
    println!("Real-Time Transcription with OpenAI (Rust)");
    println!("{}", "=".repeat(60));
    println!();

    deps::check_system_dependencies();

    info!(api_key_len = api_key.len(), model = %config.model, "Session starting");

    // ── Create channels ──────────────────────────────────────────────

    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(AUDIO_CHANNEL_SIZE);
    let (ws_cmd_tx, mut ws_cmd_rx) = mpsc::channel::<WsCommand>(EVENT_CHANNEL_SIZE);
    let (audio_event_tx, audio_event_rx) = mpsc::channel::<AudioEvent>(EVENT_CHANNEL_SIZE);
    let (transcript_tx, transcript_rx) = mpsc::channel::<TranscriptEvent>(EVENT_CHANNEL_SIZE);
    let (type_tx, type_rx) = mpsc::channel::<TypeCommand>(EVENT_CHANNEL_SIZE);
    let (metrics_tx, metrics_rx) = mpsc::channel::<MetricsEvent>(EVENT_CHANNEL_SIZE);

    // ── Cancellation ─────────────────────────────────────────────────

    let root_token = CancellationToken::new();

    let shutdown_token = root_token.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        println!("\n[INFO] Ctrl+C received, shutting down...");
        shutdown_token.cancel();
    });

    // SIGUSR1 = pause, SIGUSR2 = resume (used by mic-toggle.sh)
    // Directional signals avoid desync when rapid keypresses race the toggle script.
    tokio::spawn(async {
        let mut pause_sig =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
                .expect("failed to register SIGUSR1 handler");
        let mut resume_sig =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined2())
                .expect("failed to register SIGUSR2 handler");
        loop {
            tokio::select! {
                _ = pause_sig.recv() => {
                    let now_paused = audio_device::set_paused(true);
                    info!(paused = now_paused, "SIGUSR1: audio capture paused");
                }
                _ = resume_sig.recv() => {
                    let now_paused = audio_device::set_paused(false);
                    info!(paused = now_paused, "SIGUSR2: audio capture resumed");
                }
            }
        }
    });

    // ── Spawn long-lived tasks ───────────────────────────────────────

    let metrics_handle = tokio::spawn(metrics::run_metrics_task(
        metrics_rx,
        root_token.child_token(),
    ));

    let typer_handle = tokio::spawn(typer::run_typer_task(
        type_rx,
        root_token.child_token(),
    ));

    let transcript_handle = tokio::spawn(transcript::run_transcript_task(
        transcript_rx,
        type_tx,
        metrics_tx.clone(),
        root_token.child_token(),
        config.clone(),
    ));

    let audio_router_handle = tokio::spawn(audio_buffer::run_audio_router_task(
        audio_rx,
        audio_event_rx,
        ws_cmd_tx,
        transcript_tx.clone(),
        metrics_tx.clone(),
        root_token.child_token(),
        api_key.clone(),
    ));

    // Watchdog: if any critical task exits, log the cause to JSONL and kill the process.
    // tokio::spawn silently swallows panics — this makes them fatal and diagnosable.
    let watchdog_cancel = root_token.clone();
    tokio::spawn(async move {
        let died = tokio::select! {
            _ = watchdog_cancel.cancelled() => None,
            r = transcript_handle => Some(("transcript", r)),
            r = typer_handle => Some(("typer", r)),
            r = audio_router_handle => Some(("audio_router", r)),
        };
        if let Some((name, result)) = died {
            let reason = match result {
                Ok(()) => "exited cleanly (unexpected)".to_string(),
                Err(e) if e.is_panic() => {
                    let payload = e.into_panic();
                    if let Some(s) = payload.downcast_ref::<&str>() {
                        format!("panicked: {s}")
                    } else if let Some(s) = payload.downcast_ref::<String>() {
                        format!("panicked: {s}")
                    } else {
                        "panicked (unknown payload)".to_string()
                    }
                }
                Err(e) => format!("cancelled: {e}"),
            };
            // Log to JSONL (via tracing) so the cause is preserved in debug events
            tracing::error!("FATAL: {name} task died: {reason}");
            eprintln!("[FATAL] {name} task died: {reason}");
            std::process::exit(1);
        }
    });

    // Start audio capture
    audio_device::start_audio_capture(audio_tx, root_token.child_token())?;
    println!("[INFO] Starting audio capture...");

    // ── Reconnection loop ────────────────────────────────────────────
    //
    // Unlike spawned tasks, the WebSocket connection runs inline here.
    // ws_cmd_rx is passed by &mut so it survives across reconnections.

    let mut consecutive_failures: u32 = 0;

    loop {
        if root_token.is_cancelled() {
            break;
        }


        if consecutive_failures == 0 {
            println!("[INFO] Connecting to OpenAI...");
        } else {
            println!("[INFO] Reconnecting to OpenAI (attempt {consecutive_failures})...");
        }

        metrics_tx.send(MetricsEvent::ConnectionAttempt).await.ok();

        let result = websocket::run_connection(
            &api_key,
            &config.model,
            &mut ws_cmd_rx,
            &audio_event_tx,
            &transcript_tx,
            &metrics_tx,
            &root_token,
        )
        .await;

        match result {
            websocket::ConnectionResult::Done => break,
            websocket::ConnectionResult::Reconnect => {
                // Was connected then lost — reset backoff, reconnect quickly
                if root_token.is_cancelled() {
                    break;
                }
                consecutive_failures = 0;
                println!("[INFO] Connection lost, reconnecting in 1s...");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                metrics_tx
                    .send(MetricsEvent::ReconnectionAttempt)
                    .await
                    .ok();
            }
            websocket::ConnectionResult::ConnectFailed { backoff } => {
                if root_token.is_cancelled() {
                    break;
                }
                consecutive_failures += 1;
                let delay = if backoff {
                    // Server error (rate limit, 4xx, 5xx) — exponential backoff
                    let d = 2.0_f64 * 2.0_f64.powi(consecutive_failures.min(4) as i32 - 1);
                    d.min(30.0)
                } else {
                    // Local error (DNS, timeout, no route) — fast retry
                    1.0
                };
                println!("[INFO] Connection failed, retrying in {delay:.1}s...");
                tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
                metrics_tx
                    .send(MetricsEvent::ReconnectionAttempt)
                    .await
                    .ok();
            }
        }
    }

    // ── Shutdown ─────────────────────────────────────────────────────

    println!("\n[INFO] Shutting down...");
    root_token.cancel();

    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let _ = metrics_handle.await;
        // Other handles are owned by the watchdog task (which will exit via cancel)
    })
    .await;

    if let Some(ref log) = debug_log_file {
        println!("[INFO] Debug log saved to: {}", log.display());
    }
    println!("[INFO] Session ended.");

    Ok(())
}

struct LocalTimer;

impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z"))
    }
}

fn setup_tracing(debug_log_file: &Option<PathBuf>) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if let Some(path) = debug_log_file {
        let file = {
            #[cfg(unix)]
            {
                use std::fs::OpenOptions;
                use std::os::unix::fs::OpenOptionsExt;
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(path)
                    .expect("Failed to create debug log file")
            }
            #[cfg(not(unix))]
            {
                std::fs::File::create(path).expect("Failed to create debug log file")
            }
        };
        let file_layer = fmt::layer()
            .json()
            .with_timer(LocalTimer)
            .with_writer(std::sync::Mutex::new(file))
            .with_target(false);

        let stderr_layer = fmt::layer()
            .with_timer(LocalTimer)
            .with_writer(std::io::stderr)
            .with_target(false)
            .with_level(true)
            .with_filter(EnvFilter::new("warn"));

        tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .with(stderr_layer)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_timer(LocalTimer)
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .init();
    }
}
