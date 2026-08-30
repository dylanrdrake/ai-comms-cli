use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ApprovalSettings {
    #[serde(default = "default_true")]
    pub read_disk: bool,
    #[serde(default = "default_true")]
    pub write_disk: bool,
    #[serde(default = "default_true")]
    pub terminal: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ApprovalSettings {
    fn default() -> Self {
        ApprovalSettings {
            read_disk: true,
            write_disk: true,
            terminal: true,
        }
    }
}

pub const VALID_EFFORT_LEVELS: [&str; 3] = ["low", "medium", "high"];

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub api_key: Option<String>,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub approval: ApprovalSettings,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
    #[serde(default)]
    pub effort_level: Option<String>,
}

fn default_base_url() -> String {
    "https://api.orcarouter.ai/v1".to_string()
}

pub fn default_max_iterations() -> usize {
    20
}

impl Default for Config {
    fn default() -> Self {
        Config {
            api_key: None,
            base_url: "https://api.orcarouter.ai/v1".to_string(),
            default_model: None,
            approval: ApprovalSettings::default(),
            max_iterations: default_max_iterations(),
            effort_level: None,
        }
    }
}

pub fn get_config_dir() -> Result<PathBuf> {
    let config_dir = home::home_dir()
        .ok_or(anyhow!("Could not determine home directory"))?
        .join(".orcacli");

    fs::create_dir_all(&config_dir)?;
    Ok(config_dir)
}

pub fn get_config_path() -> Result<PathBuf> {
    Ok(get_config_dir()?.join("config.json"))
}

pub fn load_config() -> Result<Config> {
    let config_path = get_config_path()?;

    if config_path.exists() {
        let content = fs::read_to_string(&config_path)?;
        match serde_json::from_str(&content) {
            Ok(config) => Ok(config),
            Err(_) => {
                // If deserialization fails, return default and let the user fix it
                Ok(Config::default())
            }
        }
    } else {
        Ok(Config::default())
    }
}

pub fn save_config(config: &Config) -> Result<()> {
    let config_path = get_config_path()?;
    let json = serde_json::to_string_pretty(config)?;
    fs::write(&config_path, json)?;
    Ok(())
}
