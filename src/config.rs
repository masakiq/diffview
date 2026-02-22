use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffConfig {
    #[serde(default = "default_tool")]
    pub tool: String,
}

fn default_tool() -> String {
    "raw".to_string()
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self { tool: default_tool() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeybindingsConfig {
    #[serde(default = "key_add")]
    pub add: String,
    #[serde(default = "key_revert")]
    pub revert: String,
    #[serde(default = "key_add_all")]
    pub add_all: String,
    #[serde(default = "key_revert_all")]
    pub revert_all: String,
    #[serde(default = "key_select_mode")]
    pub select_mode: String,
    #[serde(default = "key_toggle_fold")]
    pub toggle_fold: String,
    #[serde(default = "key_next_hunk")]
    pub next_hunk: String,
    #[serde(default = "key_prev_hunk")]
    pub prev_hunk: String,
}

fn key_add() -> String { "a".to_string() }
fn key_revert() -> String { "r".to_string() }
fn key_add_all() -> String { "A".to_string() }
fn key_revert_all() -> String { "R".to_string() }
fn key_select_mode() -> String { "v".to_string() }
fn key_toggle_fold() -> String { " ".to_string() }
fn key_next_hunk() -> String { "n".to_string() }
fn key_prev_hunk() -> String { "p".to_string() }

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            add: key_add(),
            revert: key_revert(),
            add_all: key_add_all(),
            revert_all: key_revert_all(),
            select_mode: key_select_mode(),
            toggle_fold: key_toggle_fold(),
            next_hunk: key_next_hunk(),
            prev_hunk: key_prev_hunk(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub diff: DiffConfig,
    #[serde(default)]
    pub keybindings: KeybindingsConfig,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    pub fn config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".config")
            .join("diffview")
            .join("config.toml")
    }
}
