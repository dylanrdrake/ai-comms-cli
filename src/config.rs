use anyhow::{anyhow, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

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

/// How `effort_level` is sent to the provider:
/// - `flat`: top-level `reasoning_effort: "<level>"` (OrcaRouter's shape)
/// - `nested`: `reasoning: { "effort": "<level>" }` (OpenRouter's shape)
/// - `none`: don't send an effort field at all (providers that reject unknown fields)
pub const VALID_EFFORT_STYLES: [&str; 3] = ["flat", "nested", "none"];
pub const DEFAULT_EFFORT_STYLE: &str = "nested";

/// The model a request falls back to when neither a `--model` flag nor the
/// config names one. Still consulted at the point of use as well as seeded
/// into the config, because `comms model --clear` deliberately writes `null`
/// and that has to keep meaning "use this".
pub const DEFAULT_MODEL: &str = "openrouter/auto";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    /// Legacy field: API keys used to be stored here in plaintext. Only
    /// populated when reading an old config.json during migration; new
    /// keys are stored in the OS keychain via `get_api_key`/`set_api_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub default_model: Option<String>,
    #[serde(default)]
    pub approval: ApprovalSettings,
    /// `None` means no persistent default is configured at all — `ask`/
    /// `agent`/a new `session` then run with no iteration cap unless
    /// `--max-iterations` is passed for that call, which errors immediately
    /// in agent mode rather than guessing a number.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: Option<usize>,
    /// `None` means no persistent default is configured at all — a request
    /// is then sent with no `temperature` field, and the provider uses its
    /// own default.
    #[serde(default = "default_temperature")]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub effort_level: Option<String>,
    /// How to serialize `effort_level` for the current `base_url`'s provider.
    /// `None` falls back to `DEFAULT_EFFORT_STYLE` ("nested").
    #[serde(default = "default_effort_style")]
    pub effort_style: Option<String>,
    /// Extra HTTP headers sent with every API request, for providers that
    /// need something beyond `Authorization: Bearer <key>` (e.g. OpenRouter's
    /// optional `HTTP-Referer`/`X-Title` attribution headers).
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    /// Whether new sessions start showing full tool-call detail. Off by
    /// default; `/verbose` toggles it for the session you're in, and that
    /// choice is remembered per session rather than changing this.
    #[serde(default)]
    pub verbose: bool,
    /// Whether a session shows a band behind your own messages. A display
    /// preference rather than a behaviour, so it changes nothing a turn
    /// does — but it is per-session like `verbose`, because a session you
    /// read back through and one you are working in want different amounts
    /// of decoration.
    #[serde(default = "default_true")]
    pub highlight: bool,
    /// Whether the launch screen bands its selected row. Global only: the
    /// launch screen belongs to no session.
    #[serde(default = "default_true")]
    pub selection: bool,
    /// Whether the agent's file writes are confined to the working
    /// directory. On by default; turning it off lets its write tools touch
    /// any path the process can. Reads are never bounded either way — they
    /// mutate nothing, and confining them would break ordinary work like
    /// reading a file under `/etc`.
    ///
    /// This gates the agent's tools only. The app's own state —
    /// `config.json`, `chats.db`, `errors.log` — is written directly and is
    /// unaffected at any setting.
    #[serde(default = "default_true")]
    pub sandbox: bool,
    /// Whether to stream responses token-by-token. On by default; turn it off
    /// for providers that handle streaming (especially streaming alongside
    /// tool calls) badly, which falls back to waiting for the whole reply.
    #[serde(default = "default_true")]
    pub stream: bool,
}

pub fn default_base_url() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

/// The model used when nothing else names one. A seed rather than a bare
/// `None`, so a config written from defaults says which model it will
/// actually use instead of leaving `null` next to a literal buried in
/// `resolve_model`.
pub fn default_model() -> Option<String> {
    Some(DEFAULT_MODEL.to_string())
}

/// Same deal: the shape effort is serialized in when the config doesn't say.
/// See [`DEFAULT_EFFORT_STYLE`].
pub fn default_effort_style() -> Option<String> {
    Some(DEFAULT_EFFORT_STYLE.to_string())
}

/// The factory default for a fresh install (no `config.json` yet) and for
/// migrating an old `config.json` written before this field existed. Once a
/// user explicitly clears it with `comms max-iterations --clear`, it stays
/// `None` — this is never consulted again after that.
pub fn default_max_iterations() -> Option<usize> {
    Some(20)
}

/// Same deal as [`default_max_iterations`].
pub fn default_temperature() -> Option<f32> {
    Some(0.7)
}

impl Default for Config {
    fn default() -> Self {
        Config {
            api_key: None,
            base_url: default_base_url(),
            default_model: default_model(),
            approval: ApprovalSettings::default(),
            max_iterations: default_max_iterations(),
            temperature: default_temperature(),
            sandbox: true,
            verbose: false,
            highlight: true,
            selection: true,
            effort_level: None,
            effort_style: default_effort_style(),
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

/// Parses `config.json`, naming the file and the position when it can't be.
///
/// Split from [`load_config`] so it's testable without moving `HOME` around,
/// and separate from the file-missing path, which is not an error: an absent
/// config means "use the defaults", a malformed one means "this says
/// something I can't read".
fn parse_config(content: &str, path: &Path) -> Result<Config> {
    serde_json::from_str(content).map_err(|e| {
        anyhow!(
            "Could not parse {}: {e}\n\n\
             Fix the file, or delete it to start from defaults.",
            path.display()
        )
    })
}

pub fn load_config() -> Result<Config> {
    let config_path = get_config_path()?;

    let mut config = if config_path.exists() {
        let content = fs::read_to_string(&config_path)?;
        // Refused rather than defaulted. Carrying on would mean sending the
        // API key to whatever `base_url` defaults to instead of the provider
        // that was configured — and worse, the next setting command would
        // save defaults-plus-one-change over the file, destroying everything
        // else in it. Nothing is written here: the file stays exactly as it
        // was typed so it can be fixed.
        parse_config(&content, &config_path)?
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

/// A live view of a session's safety controls, rather than a copy of them.
///
/// The agent loop runs on its own task, so it used to be handed a snapshot
/// taken when the turn was spawned — which meant a `/approval write off`
/// typed while a turn was running had no effect until the *next* turn, even
/// though the settings row updated immediately and said otherwise. Sharing
/// the settings instead lets each tool call read what they say right now,
/// which is what someone flipping a gate mid-turn is asking for.
///
/// Both controls live here for the same reason: they decide what a tool is
/// allowed to do, so a turn in progress is exactly when a change to one
/// matters most. Settings that only shape the *next* request — model,
/// effort, temperature — are deliberately not here, and still apply from the
/// next turn.
///
/// Cheap to clone: every clone reads and writes the same state.
#[derive(Clone, Debug, Default)]
pub struct SessionGates {
    approval: Arc<Mutex<ApprovalSettings>>,
    sandbox: Arc<AtomicBool>,
}

impl SessionGates {
    pub fn new(approval: ApprovalSettings, sandbox: bool) -> Self {
        Self {
            approval: Arc::new(Mutex::new(approval)),
            sandbox: Arc::new(AtomicBool::new(sandbox)),
        }
    }

    /// The approval gates as they stand. Cloned out rather than handing back
    /// a guard, so a caller can't hold the lock across an await.
    pub fn approval(&self) -> ApprovalSettings {
        self.lock().clone()
    }

    pub fn set_approval(&self, approval: ApprovalSettings) {
        *self.lock() = approval;
    }

    /// Whether the agent's file writes are confined to the working directory.
    pub fn sandbox(&self) -> bool {
        self.sandbox.load(Ordering::Relaxed)
    }

    pub fn set_sandbox(&self, sandbox: bool) {
        self.sandbox.store(sandbox, Ordering::Relaxed);
    }

    /// A poisoned lock still holds perfectly good settings — the panic that
    /// poisoned it happened elsewhere — and refusing to read them would turn
    /// an unrelated panic into a dead approval gate.
    fn lock(&self) -> MutexGuard<'_, ApprovalSettings> {
        self.approval.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_config_keeps_its_values_and_seeds_the_rest() {
        // Hand-writing one key is a supported way to configure this, so the
        // keys that are there must survive and the rest must come from
        // their seeds — not from `Config::default()` wholesale.
        let config = parse_config(
            r#"{"temperature": 1.5, "base_url": "https://example.test/v1"}"#,
            Path::new("config.json"),
        )
        .expect("a partial config is valid");

        assert_eq!(config.temperature, Some(1.5));
        assert_eq!(config.base_url, "https://example.test/v1");
        // Untouched keys take their seeds, including the two that used to
        // sit at `null` while a literal supplied the real value.
        assert_eq!(config.default_model.as_deref(), Some(DEFAULT_MODEL));
        assert_eq!(config.effort_style.as_deref(), Some(DEFAULT_EFFORT_STYLE));
        assert_eq!(config.max_iterations, Some(20));
        assert!(config.sandbox);
    }

    #[test]
    fn an_explicit_null_is_not_the_seed() {
        // serde only defaults an *absent* key. `comms model --clear` writes
        // null deliberately, and that has to keep meaning "cleared" rather
        // than being quietly refilled.
        let config = parse_config(r#"{"default_model": null}"#, Path::new("config.json"))
            .expect("null is valid");
        assert_eq!(config.default_model, None);
    }

    #[test]
    fn a_malformed_config_is_refused_and_says_where() {
        let error = parse_config(
            "{\n  \"temperature\": 1.9,\n}",
            Path::new("/tmp/config.json"),
        )
        .expect_err("a trailing comma is not valid json");
        let message = error.to_string();

        // Names the file, so it's obvious which one to open...
        assert!(message.contains("/tmp/config.json"), "{message}");
        // ...where the problem is...
        assert!(message.contains("line"), "{message}");
        // ...and how to get out of it.
        assert!(message.contains("delete it"), "{message}");
    }

    #[test]
    fn a_gate_flipped_on_one_handle_is_seen_through_another() {
        // The whole point: the running turn holds a clone, and the worker
        // that answers `/approval` or `/sandbox` holds the original. A write
        // through one has to be visible through the other, or the turn keeps
        // running on the gates it started with.
        let worker = SessionGates::new(ApprovalSettings::default(), true);
        let running_turn = worker.clone();
        assert!(running_turn.approval().write_disk);
        assert!(running_turn.sandbox());

        worker.set_approval(worker.approval().with_category("write", false));
        worker.set_sandbox(false);

        assert!(!running_turn.approval().write_disk);
        assert!(!running_turn.sandbox());
        // Only the category asked for moves.
        assert!(running_turn.approval().read_disk);
        assert!(running_turn.approval().terminal);
    }

    #[test]
    fn gates_survive_a_poisoned_lock() {
        // A panic somewhere else must not leave the gates unreadable — the
        // settings behind the lock are still perfectly good, and failing
        // here would turn an unrelated panic into a dead approval gate.
        let gates = SessionGates::new(ApprovalSettings::default(), true);
        let poisoner = gates.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("poison the lock");
        })
        .join();

        assert!(gates.approval().read_disk);
        gates.set_approval(gates.approval().with_category("read", false));
        assert!(!gates.approval().read_disk);
    }
}
