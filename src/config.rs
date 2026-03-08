use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitActionConfig {
    #[serde(default = "default_commit_key")]
    pub key: String,
    #[serde(default = "default_commit_command")]
    pub command: Vec<String>,
}

fn default_commit_key() -> String {
    "C".to_string()
}

fn default_commit_command() -> Vec<String> {
    vec!["git".to_string(), "commit".to_string(), "-v".to_string()]
}

impl Default for CommitActionConfig {
    fn default() -> Self {
        Self {
            key: default_commit_key(),
            command: default_commit_command(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffConfig {
    #[serde(default = "default_tool")]
    pub tool: String,
    #[serde(default = "default_tree_width_percentage")]
    pub tree_width_percentage: u16,
    #[serde(default)]
    pub commit: CommitActionConfig,
}

fn default_tool() -> String {
    "raw".to_string()
}

fn default_tree_width_percentage() -> u16 {
    25
}

impl DiffConfig {
    pub fn tree_width_percentage(&self) -> u16 {
        self.tree_width_percentage.clamp(10, 90)
    }
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            tool: default_tool(),
            tree_width_percentage: default_tree_width_percentage(),
            commit: CommitActionConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub diff: DiffConfig,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_config_defaults_include_layout_and_commit_action() {
        let config = Config::default();

        assert_eq!(config.diff.tool, "raw");
        assert_eq!(config.diff.tree_width_percentage(), 25);
        assert_eq!(config.diff.commit.key, "C");
        assert_eq!(config.diff.commit.command, vec!["git", "commit", "-v"]);
    }

    #[test]
    fn config_parses_nested_commit_and_layout_settings() {
        let config: Config = toml::from_str(
            r#"
[diff]
tool = "delta"
tree_width_percentage = 40

[diff.commit]
key = "ctrl-g"
command = ["git", "commit", "--amend"]
"#,
        )
        .unwrap();

        assert_eq!(config.diff.tool, "delta");
        assert_eq!(config.diff.tree_width_percentage(), 40);
        assert_eq!(config.diff.commit.key, "ctrl-g");
        assert_eq!(config.diff.commit.command, vec!["git", "commit", "--amend"]);
    }
}
