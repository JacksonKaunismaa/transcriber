use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// Check if adaptive typing tools are available.
pub fn test_adaptive() -> bool {
    which("wl-copy") && which("wtype") && which("hyprctl")
}

/// Get the window class of the currently focused window via hyprctl.
pub fn get_focused_window_class() -> String {
    let output = Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            #[derive(Deserialize)]
            struct WindowInfo {
                #[serde(default)]
                class: String,
            }

            serde_json::from_slice::<WindowInfo>(&o.stdout)
                .map(|w| w.class.to_lowercase())
                .unwrap_or_default()
        }
        _ => String::new(),
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
    "wtype".into()
}

impl TyperRules {
    pub fn load(config_path: Option<&Path>) -> Self {
        let path = config_path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("transcriber/typer_rules.yaml"));

        let mut rules = TyperRules {
            rules: Vec::new(),
            default: "wtype".into(),
            config_path: path,
            mtime: None,
        };
        rules.reload();
        rules
    }

    fn reload(&mut self) {
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
        // NOTE: hot-reload is skipped here because this is called from spawn_blocking.
        // The rules are reloaded when the TyperRules is cloned for each typing operation.
        let lower = window_class.to_lowercase();
        for rule in &self.rules {
            if lower.contains(&rule.match_str.to_lowercase()) {
                return rule.method.clone();
            }
        }
        self.default.clone()
    }
}

fn which(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .is_ok_and(|o| o.status.success())
}
