use std::path::{Path, PathBuf};

use fancy_regex::Regex;
use serde::Deserialize;
use tracing::{info, warn};

/// A single filter definition from YAML.
#[derive(Deserialize, Debug)]
struct FilterDef {
    pattern: String,
    #[serde(default)]
    flags: String,
}

/// Top-level filters.yaml structure.
#[derive(Deserialize, Debug, Default)]
struct FiltersConfig {
    #[serde(default)]
    hallucinations: Vec<FilterDef>,
    #[serde(default)]
    fillers: Vec<FilterDef>,
    #[serde(default)]
    non_ascii: Vec<FilterDef>,
}

/// A compiled filter pattern using fancy-regex (supports backreferences).
pub struct CompiledFilter {
    pub regex: Regex,
}

/// All compiled filter sets, loaded from filters.yaml.
pub struct Filters {
    pub hallucinations: Vec<CompiledFilter>,
    pub fillers: Vec<CompiledFilter>,
    pub non_ascii: Vec<CompiledFilter>,
    config_path: PathBuf,
    last_mtime: Option<std::time::SystemTime>,
}

impl Filters {
    /// Load and compile filters from the given YAML path.
    pub fn load(config_path: &Path) -> Self {
        let mut filters = Filters {
            hallucinations: Vec::new(),
            fillers: Vec::new(),
            non_ascii: Vec::new(),
            config_path: config_path.to_path_buf(),
            last_mtime: None,
        };
        filters.reload();
        filters
    }

    /// Reload filters from disk if the file was modified.
    pub fn reload(&mut self) -> bool {
        let mtime = std::fs::metadata(&self.config_path)
            .ok()
            .and_then(|m| m.modified().ok());

        if mtime == self.last_mtime && self.last_mtime.is_some() {
            return false;
        }

        match self.do_reload() {
            Ok(()) => {
                if self.last_mtime.is_some() {
                    info!("Reloaded filters from {}", self.config_path.display());
                }
                self.last_mtime = mtime;
                true
            }
            Err(e) => {
                warn!("Failed to reload filters: {e}");
                false
            }
        }
    }

    fn do_reload(&mut self) -> anyhow::Result<()> {
        let contents = std::fs::read_to_string(&self.config_path)?;
        let config: FiltersConfig = serde_yaml::from_str(&contents)?;

        self.hallucinations = compile_filter_list(&config.hallucinations);
        self.fillers = compile_filter_list(&config.fillers);
        self.non_ascii = compile_filter_list(&config.non_ascii);

        Ok(())
    }
}

fn compile_filter_list(defs: &[FilterDef]) -> Vec<CompiledFilter> {
    let mut compiled = Vec::with_capacity(defs.len());

    for def in defs {
        if def.pattern.is_empty() {
            continue;
        }

        let mut pattern = String::new();
        let flags = def.flags.to_lowercase();
        if flags.contains("ignorecase") {
            pattern.push_str("(?i)");
        }
        if flags.contains("multiline") {
            pattern.push_str("(?m)");
        }
        if flags.contains("dotall") {
            pattern.push_str("(?s)");
        }
        pattern.push_str(&def.pattern);

        match Regex::new(&pattern) {
            Ok(regex) => {
                compiled.push(CompiledFilter { regex });
            }
            Err(e) => {
                warn!("Invalid filter pattern '{}': {e}", def.pattern);
            }
        }
    }

    compiled
}
