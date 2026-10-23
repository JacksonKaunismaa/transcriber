use std::path::{Path, PathBuf};
#[cfg(not(target_os = "macos"))]
use std::process::Command;

#[cfg(not(target_os = "macos"))]
use anyhow::{Context, Result};
use serde::Deserialize;
#[cfg(not(target_os = "macos"))]
use tracing::warn;

#[cfg(target_os = "macos")]
pub use super::macos::WindowDetector;

/// Per-platform rules file: window classes on Hyprland, bundle IDs on macOS.
#[cfg(not(target_os = "macos"))]
pub const RULES_FILE: &str = "typer_rules.yaml";
#[cfg(target_os = "macos")]
pub const RULES_FILE: &str = "typer_rules.macos.yaml";

#[cfg(not(target_os = "macos"))]
const DEFAULT_METHOD: &str = "wtype";
#[cfg(target_os = "macos")]
const DEFAULT_METHOD: &str = "cmd-v";

#[cfg(not(target_os = "macos"))]
pub const TYPING_REQUIREMENTS: &str = "Adaptive typing requires:
  - wl-copy: sudo pacman -S wl-clipboard
  - wtype: sudo pacman -S wtype
  - hyprctl: comes with Hyprland";
#[cfg(target_os = "macos")]
pub const TYPING_REQUIREMENTS: &str = "Pasting needs the Accessibility permission for the app that \
launched the transcriber (your terminal, or the hotkey launcher):
  System Settings > Privacy & Security > Accessibility, then restart the transcriber";

/// Check if adaptive typing tools are available.
#[cfg(not(target_os = "macos"))]
pub fn test_adaptive() -> bool {
    which("wl-copy") && which("wtype") && which("hyprctl")
}

/// On macOS the tools are built in; what can be missing is the permission.
#[cfg(target_os = "macos")]
pub fn test_adaptive() -> bool {
    super::macos::accessibility_trusted()
}

/// Focused-window lookup via `hyprctl`, resilient to Hyprland restarting
/// underneath us.
///
/// `hyprctl` locates the compositor through `HYPRLAND_INSTANCE_SIGNATURE`,
/// inherited at process start. If Hyprland restarts while we keep running,
/// that value goes stale and `hyprctl activewindow` prints
/// "Couldn't connect to …/.socket.sock" — with exit status 0 — so a status
/// check passes and only the JSON parse fails. 2026-07-31 → 08-10 exactly this
/// happened: 571 utterances (all of Claude Code) silently fell through to the
/// default wtype path with nothing logged. On failure we now re-resolve the
/// live instance via `hyprctl instances -j` (which scans the runtime dir and
/// doesn't need the env var) and retry with `-i <signature>`.
#[cfg(not(target_os = "macos"))]
pub struct WindowDetector {
    /// Instance signature to pass as `hyprctl -i`, set once the inherited env
    /// var has been found stale.
    instance_override: Option<String>,
}

#[cfg(not(target_os = "macos"))]
#[derive(Deserialize)]
struct WindowInfo {
    #[serde(default)]
    class: String,
}

#[cfg(not(target_os = "macos"))]
#[derive(Deserialize)]
struct InstanceInfo {
    instance: String,
    time: u64,
}

#[cfg(not(target_os = "macos"))]
impl WindowDetector {
    pub fn new() -> Self {
        Self {
            instance_override: None,
        }
    }

    /// Lowercased class of the focused window. `Ok("")` when Hyprland reports
    /// no focused window; `Err` when hyprctl can't be reached even after
    /// re-resolving the instance.
    pub fn focused_window_class(&mut self) -> Result<String> {
        let first_err = match self.query_active_window() {
            Ok(class) => return Ok(class),
            Err(e) => e,
        };
        let sig = self
            .rediscover_instance()
            .with_context(|| format!("hyprctl activewindow failed ({first_err:#})"))?;
        warn!(
            "hyprctl activewindow failed ({first_err:#}); re-resolved Hyprland instance to \
             {sig} — compositor restarted under us?"
        );
        self.query_active_window()
    }

    fn query_active_window(&self) -> Result<String> {
        let mut cmd = Command::new("hyprctl");
        if let Some(sig) = &self.instance_override {
            cmd.args(["-i", sig]);
        }
        let out = cmd
            .args(["activewindow", "-j"])
            .output()
            .context("spawn hyprctl")?;
        // hyprctl exits 0 even when it can't reach the socket, so the exit
        // status is meaningless here; the JSON parse is the real check.
        let info: WindowInfo = serde_json::from_slice(&out.stdout).with_context(|| {
            format!(
                "non-JSON output: stdout={:?} stderr={:?}",
                String::from_utf8_lossy(&out.stdout).trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            )
        })?;
        Ok(info.class.to_lowercase())
    }

    /// Newest running instance per `hyprctl instances -j`, which reads
    /// `$XDG_RUNTIME_DIR/hypr/` directly and works with a stale env var.
    fn rediscover_instance(&mut self) -> Result<String> {
        let out = Command::new("hyprctl")
            .args(["instances", "-j"])
            .output()
            .context("spawn hyprctl instances")?;
        let instances: Vec<InstanceInfo> = serde_json::from_slice(&out.stdout)
            .with_context(|| {
                format!(
                    "hyprctl instances returned non-JSON: {:?}",
                    String::from_utf8_lossy(&out.stdout).trim()
                )
            })?;
        let newest = instances
            .into_iter()
            .max_by_key(|i| i.time)
            .context("hyprctl reports no running Hyprland instances")?;
        self.instance_override = Some(newest.instance.clone());
        Ok(newest.instance)
    }
}

/// Typer rules loaded from typer_rules.yaml with hot-reload.
#[derive(Clone)]
pub struct TyperRules {
    rules: Vec<Rule>,
    default: String,
    config_path: PathBuf,
    mtime: Option<std::time::SystemTime>,
}

#[derive(Deserialize, Clone)]
struct Rule {
    #[serde(rename = "match")]
    match_str: String,
    method: String,
}

#[derive(Deserialize)]
struct RulesConfig {
    #[serde(default)]
    rules: Vec<Rule>,
    #[serde(default = "default_method")]
    default: String,
}

fn default_method() -> String {
    DEFAULT_METHOD.into()
}

impl TyperRules {
    pub fn load(config_path: Option<&Path>) -> Self {
        let path = config_path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(RULES_FILE));

        let mut rules = TyperRules {
            rules: Vec::new(),
            default: DEFAULT_METHOD.into(),
            config_path: path,
            mtime: None,
        };
        rules.reload();
        rules
    }

    pub fn reload(&mut self) {
        let mtime = std::fs::metadata(&self.config_path)
            .ok()
            .and_then(|m| m.modified().ok());

        if mtime == self.mtime && self.mtime.is_some() {
            return;
        }

        if let Ok(contents) = std::fs::read_to_string(&self.config_path) {
            if let Ok(config) = serde_yaml::from_str::<RulesConfig>(&contents) {
                self.rules = config.rules;
                self.default = config.default;
            }
        }

        self.mtime = mtime;
    }

    /// Get the typing method for the given window class.
    pub fn get_method_for_window(&self, window_class: &str) -> String {
        let lower = window_class.to_lowercase();
        for rule in &self.rules {
            if lower.contains(&rule.match_str.to_lowercase()) {
                return rule.method.clone();
            }
        }
        self.default.clone()
    }
}

#[cfg(not(target_os = "macos"))]
fn which(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .is_ok_and(|o| o.status.success())
}
