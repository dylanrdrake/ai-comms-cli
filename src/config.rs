use anyhow::{anyhow, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

const KEYRING_SERVICE: &str = "ai-comms-cli";
const KEYRING_USERNAME: &str = "api_key";

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
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

impl ApprovalSettings {
    /// Returns a copy with `category` (`"read"`/`"write"`/`"terminal"`/`"all"`)
    /// switched to `enabled`. Shared by `comms approval`'s global-default
    /// commands and a session's `/approval` override, so both parse the
    /// same category words the same way.
    pub fn with_category(&self, category: &str, enabled: bool) -> Self {
        let mut updated = self.clone();
        match category {
            "read" => updated.read_disk = enabled,
            "write" => updated.write_disk = enabled,
            "terminal" => updated.terminal = enabled,
            "all" => {
                updated.read_disk = enabled;
                updated.write_disk = enabled;
                updated.terminal = enabled;
            }
            _ => {}
        }
        updated
    }
}

pub const VALID_EFFORT_LEVELS: [&str; 3] = ["low", "medium", "high"];

/// How `effort_level` is sent to the provider:
/// - `flat`: top-level `reasoning_effort: "<level>"` (OrcaRouter's shape)
/// - `nested`: `reasoning: { "effort": "<level>" }` (OpenRouter's shape)
/// - `none`: don't send an effort field at all (providers that reject unknown fields)
pub const VALID_EFFORT_STYLES: [&str; 3] = ["flat", "nested", "none"];
pub const DEFAULT_EFFORT_STYLE: &str = "nested";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    /// Legacy field: API keys used to be stored here in plaintext. Only
    /// populated when reading an old config.json during migration; new
    /// keys are stored in the OS keychain via `get_api_key`/`set_api_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub approval: ApprovalSettings,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default)]
    pub effort_level: Option<String>,
    /// How to serialize `effort_level` for the current `base_url`'s provider.
    /// `None` falls back to `DEFAULT_EFFORT_STYLE` ("nested").
    #[serde(default)]
    pub effort_style: Option<String>,
    /// Extra HTTP headers sent with every API request, for providers that
    /// need something beyond `Authorization: Bearer <key>` (e.g. OpenRouter's
    /// optional `HTTP-Referer`/`X-Title` attribution headers).
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    /// Whether to stream responses token-by-token. On by default; turn it off
    /// for providers that handle streaming (especially streaming alongside
    /// tool calls) badly, which falls back to waiting for the whole reply.
    #[serde(default = "default_true")]
    pub stream: bool,
}

pub fn default_base_url() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

pub fn default_max_iterations() -> usize {
    20
}

pub fn default_temperature() -> f32 {
    0.7
}

impl Default for Config {
    fn default() -> Self {
        Config {
            api_key: None,
            base_url: default_base_url(),
            default_model: None,
            approval: ApprovalSettings::default(),
            max_iterations: default_max_iterations(),
            temperature: default_temperature(),
            effort_level: None,
            effort_style: None,
            extra_headers: HashMap::new(),
            stream: true,
        }
    }
}

pub fn get_config_dir() -> Result<PathBuf> {
    let config_dir = home::home_dir()
        .ok_or(anyhow!("Could not determine home directory"))?
        .join(".comms");

    fs::create_dir_all(&config_dir)?;
    Ok(config_dir)
}

pub fn get_config_path() -> Result<PathBuf> {
    Ok(get_config_dir()?.join("config.json"))
}

pub fn load_config() -> Result<Config> {
    let config_path = get_config_path()?;

    let mut config = if config_path.exists() {
        let content = fs::read_to_string(&config_path)?;
        // If deserialization fails, fall back to defaults and let the user fix it
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Config::default()
    };

    // Migrate a plaintext key from an older config.json into the OS keychain.
    if let Some(legacy_key) = config.api_key.take() {
        set_api_key(&legacy_key)?;
        save_config(&config)?;
    }

    Ok(config)
}

pub fn save_config(config: &Config) -> Result<()> {
    let config_path = get_config_path()?;
    let json = serde_json::to_string_pretty(config)?;
    fs::write(&config_path, json)?;
    Ok(())
}

fn keyring_entry() -> Result<Entry> {
    Ok(Entry::new(KEYRING_SERVICE, KEYRING_USERNAME)?)
}

/// Reads the API key from the OS keychain (macOS Keychain, Windows
/// Credential Manager, or the Linux Secret Service). Returns `Ok(None)`
/// if no key has been stored yet.
pub fn get_api_key() -> Result<Option<String>> {
    match keyring_entry()?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!("Failed to read API key from OS keychain: {e}")),
    }
}

/// Stores the API key in the OS keychain.
pub fn set_api_key(key: &str) -> Result<()> {
    keyring_entry()?
        .set_password(key)
        .map_err(|e| anyhow!("Failed to save API key to OS keychain: {e}"))
}

/// Removes the API key from the OS keychain, if present.
pub fn clear_api_key() -> Result<()> {
    match keyring_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow!("Failed to remove API key from OS keychain: {e}")),
    }
}
