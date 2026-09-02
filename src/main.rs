mod agent;
mod client;
mod config;
mod conversation;
mod crypto;
mod error_log;
mod session;
mod spinner;
mod store;
mod terminal_ui;
mod tools;
mod tui;
mod ui;
mod wrap;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;
use rustyline::DefaultEditor;
use std::io::{self, Write};
use std::sync::Arc;

use client::{ChatMessage, Client};
use config::{
    clear_api_key, get_api_key, get_config_path, load_config, save_config, set_api_key,
    ApprovalSettings, SessionGates, VALID_EFFORT_STYLES,
};
use session::ChatSession;
use spinner::Spinner;
use store::{KIND_AGENT_CHAT, KIND_CHAT};
use terminal_ui::TerminalAgentUi;
use ui::{parse_bool, response_label};

#[derive(Parser)]
#[command(name = "comms")]
#[command(about = "AI Comms CLI - An OpenAI-compatible frontend for any LLM provider", long_about = None)]
#[command(version = "0.1.0")]
struct Cli {
    /// With no subcommand at all, `comms` launches the full-screen TUI on
    /// its launch screen — the only way in; there's no `tui` subcommand or
    /// flags to skip straight into a new or resumed session.
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Set up your API key
    Login,

    /// Remove stored API key
    Logout,

    /// Check configuration status
    Status,

    /// List available models
    Models,

    /// View or set the persistent default model
    Model {
        /// Model to set as the default (omit to show the current default)
        name: Option<String>,

        /// Clear the stored default model (falls back to openrouter/auto)
        #[arg(long)]
        clear: bool,
    },

    /// View or set the API base URL, to point at any OpenAI-compatible service
    Endpoint {
        /// Base URL to use, e.g. https://openrouter.ai/api/v1 (omit to show the current value)
        url: Option<String>,

        /// Clear the stored endpoint (falls back to the OpenRouter default)
        #[arg(long)]
        clear: bool,
    },

    /// View or set how the reasoning effort level is sent to the provider
    EffortStyle {
        /// Style to set: flat, nested, or none (omit to show the current value)
        value: Option<String>,

        /// Clear the stored effort style (falls back to "nested")
        #[arg(long)]
        clear: bool,
    },

    /// Manage extra HTTP headers sent with every API request
    Headers {
        #[command(subcommand)]
        action: Option<HeaderCommands>,
    },

    /// Configure approval settings for agentic actions
    Approval {
        #[command(subcommand)]
        action: Option<ApprovalCommands>,
    },

    /// View or set the persistent default max agent iterations
    MaxIterations {
        /// Value to set as the default (omit to show the current default)
        value: Option<usize>,

        /// Clear the stored default — `ask`/`agent`/a new `session` then run
        /// with no cap unless `--max-iterations` is passed for that call
        #[arg(long)]
        clear: bool,
    },

    /// View or set the persistent default sampling temperature
    Temperature {
        /// Value to set as the default (omit to show the current default)
        value: Option<f32>,

        /// Clear the stored default — requests are then sent with no
        /// temperature field, and the provider uses its own default
        #[arg(long)]
        clear: bool,
    },

    /// View or set whether responses stream in as they're generated
    Stream {
        /// on/off (also accepts true/false, yes/no, 1/0). Omit to show the
        /// current setting.
        #[arg(value_parser = parse_bool)]
        value: Option<bool>,
    },

    /// View or set whether the agent's file writes are confined to the
    /// working directory and your home directory
    Sandbox {
        /// on/off (also accepts true/false, yes/no, 1/0). Omit to show the
        /// current setting.
        #[arg(value_parser = parse_bool)]
        value: Option<bool>,
    },

    /// View or set the persistent default reasoning effort level (low, medium, high)
    EffortLevel {
        /// Effort level to set as the default (omit to show the current default)
        value: Option<String>,

        /// Clear the stored effort level (falls back to provider default)
        #[arg(long)]
        clear: bool,
    },

    /// Send a prompt to the LLM
    Ask {
        /// Your prompt/question
        prompt: String,

        /// Model to use (overrides the persistent default for this call)
        #[arg(short, long)]
        model: Option<String>,

        /// Sampling temperature (overrides the persistent default for this call)
        #[arg(long)]
        temperature: Option<f32>,

        /// Reasoning effort (overrides the persistent default for this
        /// call). Not checked against a fixed list — pass whatever your
        /// model accepts.
        #[arg(long)]
        effort_level: Option<String>,
    },

    /// Interactive session — starts in plain ask mode; /agent turns on
    /// tools (read/write files, run commands) from inside it. Also /model,
    /// /effort, and /verbose. Same experience as `tui`, minus the screen.
    Session {
        /// Model to use for a new session (overrides the persistent
        /// default; ignored when resuming, which keeps its saved model)
        #[arg(short, long)]
        model: Option<String>,

        /// Maximum number of tool-calling iterations per turn while in
        /// agent mode (overrides the persistent default for this call)
        #[arg(long)]
        max_iterations: Option<usize>,

        /// Sampling temperature for this session (overrides the persistent
        /// default for this call)
        #[arg(long)]
        temperature: Option<f32>,

        /// Reasoning effort for a new session (overrides the persistent
        /// default; ignored when resuming, which keeps its saved value).
        /// Not checked against a fixed list — pass whatever your model
        /// accepts.
        #[arg(long)]
        effort_level: Option<String>,

        /// Resume a saved session by id (or unique id prefix); pass with no
        /// value to pick from a list of all your saved sessions
        #[arg(long, num_args = 0..=1, default_missing_value = PICK_SESSION_SENTINEL)]
        resume: Option<String>,
    },

    /// Run an agentic task (can write/read files)
    Agent {
        /// The task to execute
        task: String,

        /// Model to use (overrides the persistent default for this call)
        #[arg(short, long)]
        model: Option<String>,

        /// Show detailed agent iterations
        #[arg(short, long)]
        verbose: bool,

        /// Maximum number of iterations (overrides the persistent default for this call)
        #[arg(long)]
        max_iterations: Option<usize>,

        /// Sampling temperature (overrides the persistent default for this call)
        #[arg(long)]
        temperature: Option<f32>,

        /// Reasoning effort (overrides the persistent default for this
        /// call). Not checked against a fixed list — pass whatever your
        /// model accepts.
        #[arg(long)]
        effort_level: Option<String>,
    },

    /// Manage saved chat sessions
    Sessions {
        #[command(subcommand)]
        action: Option<SessionCommands>,
    },
}

#[derive(Subcommand)]
enum HeaderCommands {
    /// Show current extra headers
    Show,
    /// Set (or overwrite) a header
    Set {
        /// Header name, e.g. HTTP-Referer
        name: String,
        /// Header value
        value: String,
    },
    /// Remove a header
    Unset {
        /// Header name to remove
        name: String,
    },
}

#[derive(Subcommand)]
enum SessionCommands {
    /// List saved sessions
    List,
    /// Show a session's full message history
    Show {
        /// Session id (or unique id prefix)
        id: String,
    },
    /// Delete a saved session
    Delete {
        /// Session id (or unique id prefix)
        id: String,
    },
}

#[derive(Subcommand)]
enum ApprovalCommands {
    /// Show current approval settings
    Show,
    /// Set approval for reading from disk (read_file, list_files)
    Read {
        /// Enable or disable approval prompts
        enabled: String,
    },
    /// Set approval for writing to disk (write_file, replace_in_file)
    Write {
        /// Enable or disable approval prompts
        enabled: String,
    },
    /// Set approval for terminal commands (run_terminal_command)
    Terminal {
        /// Enable or disable approval prompts
        enabled: String,
    },
    /// Set all approval settings at once
    All {
        /// Enable or disable all approval prompts
        enabled: String,
    },
}

/// Sentinel value for `--resume` passed with no id, meaning "show a picker".
/// Never collides with a real session id since those are lowercase-hex UUIDs
/// (no 'p', 'i', 'c', 'k' in hex).
const PICK_SESSION_SENTINEL: &str = "pick";

/// Resolves a `--resume` value (an id/prefix, or the "pick" sentinel) to the
/// session it refers to, prompting the user to choose from a numbered list
/// of their saved sessions of the given kind when no id was given.
fn resolve_resume_target(
    conn: &rusqlite::Connection,
    id_or_prefix: &str,
) -> Result<store::SessionSummary> {
    if id_or_prefix != PICK_SESSION_SENTINEL {
        return store::find_session(conn, id_or_prefix)?
            .ok_or_else(|| anyhow::anyhow!("No session found matching '{}'", id_or_prefix));
    }

    let sessions = store::list_sessions(conn)?;
    if sessions.is_empty() {
        anyhow::bail!("No saved sessions to resume");
    }

    println!("{}\n", "Select a session to resume:".blue());
    for (i, s) in sessions.iter().enumerate() {
        let mode = if s.kind == KIND_AGENT_CHAT {
            "agent"
        } else {
            "chat"
        };
        println!(
            "  {}. {}  {:<6}{}",
            i + 1,
            (&s.id[..8]).bright_black(),
            mode,
            s.title
        );
    }

    print!("\n{} ", "Session number:".blue());
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let choice: usize = input
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid selection: '{}'", input.trim()))?;

    choice
        .checked_sub(1)
        .and_then(|i| sessions.into_iter().nth(i))
        .ok_or_else(|| anyhow::anyhow!("Invalid selection: {}", choice))
}

fn resolve_model(config: &config::Config, cli_model: Option<String>) -> String {
    cli_model
        .or_else(|| config.default_model.clone())
        .unwrap_or_else(|| config::DEFAULT_MODEL.to_string())
}

/// `None` if neither the flag nor the config default is set — genuinely no
/// value, not a hardcoded floor. `ask`/`agent`/a new `session` pass this
/// straight through: a request goes out with no `temperature` field, and an
/// agent-mode turn with no cap errors immediately rather than guessing one.
fn resolve_max_iterations(config: &config::Config, cli_value: Option<usize>) -> Option<usize> {
    cli_value.or(config.max_iterations)
}

/// Same deal as [`resolve_max_iterations`].
fn resolve_temperature(config: &config::Config, cli_value: Option<f32>) -> Option<f32> {
    cli_value.or(config.temperature)
}

/// Same deal as [`resolve_max_iterations`] — `None` is itself a meaningful
/// value (no effort field sent), not just "unset".
fn resolve_effort_level(config: &config::Config, cli_value: Option<String>) -> Option<String> {
    cli_value.or_else(|| config.effort_level.clone())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => cmd_tui().await?,
        Some(Commands::Login) => cmd_login().await?,
        Some(Commands::Logout) => cmd_logout().await?,
        Some(Commands::Status) => cmd_status().await?,
        Some(Commands::Models) => cmd_models().await?,
        Some(Commands::Model { name, clear }) => cmd_model(name, clear).await?,
        Some(Commands::Endpoint { url, clear }) => cmd_endpoint(url, clear).await?,
        Some(Commands::EffortStyle { value, clear }) => cmd_effort_style(value, clear).await?,
        Some(Commands::Headers { action }) => cmd_headers(action).await?,
        Some(Commands::Approval { action }) => cmd_approval(action).await?,
        Some(Commands::MaxIterations { value, clear }) => cmd_max_iterations(value, clear).await?,
        Some(Commands::Temperature { value, clear }) => cmd_temperature(value, clear).await?,
        Some(Commands::Stream { value }) => cmd_stream(value).await?,
        Some(Commands::Sandbox { value }) => cmd_sandbox(value).await?,
        Some(Commands::EffortLevel { value, clear }) => cmd_effort_level(value, clear).await?,
        Some(Commands::Ask {
            prompt,
            model,
            temperature,
            effort_level,
        }) => cmd_ask(&prompt, model, temperature, effort_level).await?,
        Some(Commands::Session {
            model,
            max_iterations,
            temperature,
            effort_level,
            resume,
        }) => cmd_session(model, max_iterations, temperature, effort_level, resume).await?,
        Some(Commands::Agent {
            task,
            model,
            verbose,
            max_iterations,
            temperature,
            effort_level,
        }) => {
            cmd_agent(
                &task,
                model,
                verbose,
                max_iterations,
                temperature,
                effort_level,
            )
            .await?
        }
        Some(Commands::Sessions { action }) => cmd_sessions(action).await?,
    }

    Ok(())
}

async fn cmd_login() -> Result<()> {
    let mut config = load_config()?;

    // Pre-filled with the current endpoint (which is itself the configured
    // default until `comms endpoint` changes it), so accepting it is just
    // pressing Enter — only typing something else actually changes it.
    let mut rl = DefaultEditor::new()?;
    let endpoint = match rl.readline_with_initial("Endpoint URL: ", (&config.base_url, "")) {
        Ok(line) => line,
        Err(rustyline::error::ReadlineError::Interrupted)
        | Err(rustyline::error::ReadlineError::Eof) => {
            println!("{} Login cancelled", "✗".red());
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let endpoint = endpoint.trim().trim_end_matches('/').to_string();
    if !endpoint.is_empty() && endpoint != config.base_url {
        config.base_url = endpoint;
        save_config(&config)?;
        println!("{} Endpoint set to {}\n", "✓".green(), config.base_url);
    }

    print!("{} ", "Enter your API key:".blue());
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let api_key = input.trim();

    if api_key.is_empty() {
        eprintln!("{} API key cannot be empty", "✗".red());
        std::process::exit(1);
    }

    set_api_key(api_key)?;

    println!("{} API key saved to OS keychain", "✓".green());
    println!(
        "{} {}",
        "Config location:".bright_black(),
        get_config_path()?.display()
    );

    Ok(())
}

async fn cmd_logout() -> Result<()> {
    clear_api_key()?;
    println!("{} API key removed", "✓".green());
    Ok(())
}

async fn cmd_status() -> Result<()> {
    let config = load_config()?;
    println!("\n{}", "AI Comms CLI Configuration:".blue());
    println!("  Base URL: {}", config.base_url);
    println!(
        "  API Key: {}",
        if get_api_key()?.is_some() {
            format!("{} Set (OS keychain)", "✓".green())
        } else {
            format!("{} Not set", "✗".red())
        }
    );
    println!(
        "  Default model: {}",
        config
            .default_model
            .as_deref()
            .unwrap_or(config::DEFAULT_MODEL)
    );
    println!(
        "  Max iterations: {}",
        config
            .max_iterations
            .map(|n| n.to_string())
            .unwrap_or_else(|| "(not set)".to_string())
    );
    println!(
        "  Temperature: {}",
        config
            .temperature
            .map(|n| n.to_string())
            .unwrap_or_else(|| "(not set)".to_string())
    );
    println!(
        "  Effort level: {}",
        config
            .effort_level
            .as_deref()
            .unwrap_or("(not set, provider default)")
    );
    println!(
        "  Effort style: {}",
        config
            .effort_style
            .as_deref()
            .unwrap_or(config::DEFAULT_EFFORT_STYLE)
    );
    println!("  Streaming: {}", if config.stream { "on" } else { "off" });
    println!("  Sandbox: {}", if config.sandbox { "on" } else { "off" });
    if !config.extra_headers.is_empty() {
        println!("  Extra headers: {}", config.extra_headers.len());
    }
    println!("  Config file: {}", get_config_path()?.display());
    println!("\n{}", "Approval Settings:".blue());
    print_approval_status(&config.approval);
    println!();
    Ok(())
}

fn format_approval(enabled: bool) -> String {
    if enabled {
        format!("{} Ask", "✓".green())
    } else {
        format!("{} Auto", "✗".yellow())
    }
}

fn print_approval_status(approval: &ApprovalSettings) {
    println!(
        "  Read from disk:    {}",
        format_approval(approval.read_disk)
    );
    println!(
        "  Write to disk:     {}",
        format_approval(approval.write_disk)
    );
    println!(
        "  Terminal commands: {}",
        format_approval(approval.terminal)
    );
}

async fn cmd_approval(action: Option<ApprovalCommands>) -> Result<()> {
    let mut config = load_config()?;

    match action {
        None | Some(ApprovalCommands::Show) => {
            println!("{}", "Approval Settings:".blue());
            print_approval_status(&config.approval);
            println!("\n{}", "Usage:".bright_black());
            println!("  comms approval read <on|off>     Set read approval");
            println!("  comms approval write <on|off>    Set write approval");
            println!("  comms approval terminal <on|off> Set terminal approval");
            println!("  comms approval all <on|off>      Set all approvals");
        }
        Some(ApprovalCommands::Read { enabled }) => {
            let value = parse_bool(&enabled).map_err(|e| anyhow::anyhow!(e))?;
            config.approval.read_disk = value;
            save_config(&config)?;
            println!(
                "{} Read approval set to {}",
                "✓".green(),
                format_approval(value)
            );
        }
        Some(ApprovalCommands::Write { enabled }) => {
            let value = parse_bool(&enabled).map_err(|e| anyhow::anyhow!(e))?;
            config.approval.write_disk = value;
            save_config(&config)?;
            println!(
                "{} Write approval set to {}",
                "✓".green(),
                format_approval(value)
            );
        }
        Some(ApprovalCommands::Terminal { enabled }) => {
            let value = parse_bool(&enabled).map_err(|e| anyhow::anyhow!(e))?;
            config.approval.terminal = value;
            save_config(&config)?;
            println!(
                "{} Terminal approval set to {}",
                "✓".green(),
                format_approval(value)
            );
        }
        Some(ApprovalCommands::All { enabled }) => {
            let value = parse_bool(&enabled).map_err(|e| anyhow::anyhow!(e))?;
            config.approval.read_disk = value;
            config.approval.write_disk = value;
            config.approval.terminal = value;
            save_config(&config)?;
            println!(
                "{} All approvals set to {}",
                "✓".green(),
                format_approval(value)
            );
        }
    }

    Ok(())
}

async fn cmd_model(name: Option<String>, clear: bool) -> Result<()> {
    let mut config = load_config()?;

    if clear {
        config.default_model = None;
        save_config(&config)?;
        println!(
            "{} Default model cleared, falling back to {}",
            "✓".green(),
            config::DEFAULT_MODEL
        );
        return Ok(());
    }

    match name {
        Some(name) => {
            config.default_model = Some(name.clone());
            save_config(&config)?;
            println!("{} Default model set to {}", "✓".green(), name);
        }
        None => {
            println!(
                "Current default model: {}",
                config
                    .default_model
                    .as_deref()
                    .unwrap_or(config::DEFAULT_MODEL)
            );
        }
    }

    Ok(())
}

async fn cmd_endpoint(url: Option<String>, clear: bool) -> Result<()> {
    let mut config = load_config()?;

    if clear {
        config.base_url = config::default_base_url();
        save_config(&config)?;
        println!(
            "{} Endpoint cleared, falling back to {}",
            "✓".green(),
            config.base_url
        );
        return Ok(());
    }

    match url {
        Some(url) => {
            let trimmed = url.trim_end_matches('/').to_string();
            config.base_url = trimmed.clone();
            save_config(&config)?;
            println!("{} Endpoint set to {}", "✓".green(), trimmed);
            println!(
                "{} Remember to run `comms login` if this provider uses a different API key",
                "i".bright_black()
            );
        }
        None => {
            println!("Current endpoint: {}", config.base_url);
        }
    }

    Ok(())
}

async fn cmd_effort_style(value: Option<String>, clear: bool) -> Result<()> {
    let mut config = load_config()?;

    if clear {
        config.effort_style = None;
        save_config(&config)?;
        println!(
            "{} Effort style cleared, falling back to {}",
            "✓".green(),
            config::DEFAULT_EFFORT_STYLE
        );
        return Ok(());
    }

    match value {
        Some(value) => {
            let normalized = value.to_lowercase();
            if !VALID_EFFORT_STYLES.contains(&normalized.as_str()) {
                eprintln!(
                    "{} Invalid effort style '{}'. Valid values: {}",
                    "✗".red(),
                    value,
                    VALID_EFFORT_STYLES.join(", ")
                );
                std::process::exit(1);
            }
            config.effort_style = Some(normalized.clone());
            save_config(&config)?;
            println!("{} Effort style set to {}", "✓".green(), normalized);
        }
        None => {
            println!(
                "Current effort style: {}",
                config
                    .effort_style
                    .as_deref()
                    .unwrap_or(config::DEFAULT_EFFORT_STYLE)
            );
        }
    }

    Ok(())
}

async fn cmd_headers(action: Option<HeaderCommands>) -> Result<()> {
    let mut config = load_config()?;

    match action.unwrap_or(HeaderCommands::Show) {
        HeaderCommands::Show => {
            if config.extra_headers.is_empty() {
                println!("No extra headers set.");
            } else {
                println!("{}\n", "Extra headers:".blue());
                for (key, value) in &config.extra_headers {
                    println!("  {}: {}", key, value);
                }
            }
        }
        HeaderCommands::Set { name, value } => {
            config.extra_headers.insert(name.clone(), value.clone());
            save_config(&config)?;
            println!("{} Header set: {}: {}", "✓".green(), name, value);
        }
        HeaderCommands::Unset { name } => {
            if config.extra_headers.remove(&name).is_some() {
                save_config(&config)?;
                println!("{} Header removed: {}", "✓".green(), name);
            } else {
                println!("No header named '{}' was set.", name);
            }
        }
    }

    Ok(())
}

/// The persistent default for confining the agent's file writes. A session
/// snapshots this when it's created, so changing it here affects new
/// sessions; `/sandbox` changes the one you're in.
async fn cmd_sandbox(value: Option<bool>) -> Result<()> {
    let mut config = load_config()?;

    match value {
        Some(enabled) => {
            config.sandbox = enabled;
            save_config(&config)?;
            println!("{} {}", "✓".green(), ui::sandbox_notice(enabled, true));
        }
        None => {
            println!("{}", ui::sandbox_notice(config.sandbox, false));
        }
    }

    Ok(())
}

async fn cmd_stream(value: Option<bool>) -> Result<()> {
    let mut config = load_config()?;

    match value {
        Some(enabled) => {
            config.stream = enabled;
            save_config(&config)?;
            println!(
                "{} Streaming responses {}",
                "✓".green(),
                if enabled { "enabled" } else { "disabled" }
            );
        }
        None => {
            println!(
                "Streaming responses: {}",
                if config.stream { "on" } else { "off" }
            );
        }
    }

    Ok(())
}

async fn cmd_max_iterations(value: Option<usize>, clear: bool) -> Result<()> {
    let mut config = load_config()?;

    if clear {
        config.max_iterations = None;
        save_config(&config)?;
        println!(
            "{} Default max iterations cleared — agent mode now needs one set per call \
             (--max-iterations) or per session (/max-iterations) to run at all",
            "✓".green()
        );
        return Ok(());
    }

    match value {
        Some(0) => {
            eprintln!("{} max-iterations must be greater than 0", "✗".red());
            std::process::exit(1);
        }
        Some(value) => {
            config.max_iterations = Some(value);
            save_config(&config)?;
            println!("{} Default max iterations set to {}", "✓".green(), value);
        }
        None => {
            println!(
                "Current default max iterations: {}",
                config
                    .max_iterations
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "(not set)".to_string())
            );
        }
    }

    Ok(())
}

async fn cmd_temperature(value: Option<f32>, clear: bool) -> Result<()> {
    let mut config = load_config()?;

    if clear {
        config.temperature = None;
        save_config(&config)?;
        println!(
            "{} Default temperature cleared — requests now have no temperature field \
             unless set per call (--temperature) or per session (/temperature)",
            "✓".green()
        );
        return Ok(());
    }

    match value {
        Some(value) if !(0.0..=2.0).contains(&value) => {
            eprintln!("{} temperature must be between 0 and 2", "✗".red());
            std::process::exit(1);
        }
        Some(value) => {
            config.temperature = Some(value);
            save_config(&config)?;
            println!("{} Default temperature set to {}", "✓".green(), value);
        }
        None => {
            println!(
                "Current default temperature: {}",
                config
                    .temperature
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "(not set)".to_string())
            );
        }
    }

    Ok(())
}

async fn cmd_effort_level(value: Option<String>, clear: bool) -> Result<()> {
    let mut config = load_config()?;

    if clear {
        config.effort_level = None;
        save_config(&config)?;
        println!(
            "{} Effort level cleared, falling back to provider default",
            "✓".green()
        );
        return Ok(());
    }

    match value {
        Some(value) => {
            // Not checked against a fixed low/medium/high list — models
            // vary in what reasoning-effort values they actually accept,
            // and this is easy to correct with another `comms effort-level`
            // if it turns out wrong for whatever you're pointed at.
            config.effort_level = Some(value.clone());
            save_config(&config)?;
            println!("{} Effort level set to {}", "✓".green(), value);
        }
        None => {
            println!(
                "Current effort level: {}",
                config
                    .effort_level
                    .as_deref()
                    .unwrap_or("(not set, provider default)")
            );
        }
    }

    Ok(())
}

async fn cmd_models() -> Result<()> {
    let config = load_config()?;
    let client = Client::new(config)?;

    let spinner = Spinner::start("Fetching models...");
    let models = client.list_models().await;
    spinner.stop().await;
    let models = models?;

    println!("{} ", "✓".green());
    println!(
        "\n{}\n",
        format!("Available models ({}):", models.len()).blue()
    );

    for (i, model) in models.iter().take(20).enumerate() {
        println!("  {}. {}", i + 1, model);
    }

    if models.len() > 20 {
        println!("  ... and {} more", models.len() - 20);
    }

    Ok(())
}

async fn cmd_ask(
    prompt: &str,
    model: Option<String>,
    temperature: Option<f32>,
    effort_level: Option<String>,
) -> Result<()> {
    let config = load_config()?;
    let model = resolve_model(&config, model);
    let effort_level = resolve_effort_level(&config, effort_level);
    let temperature = resolve_temperature(&config, temperature);
    let client = Client::new(config)?;

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(prompt.to_string()),
        tool_calls: None,
        tool_call_id: None,
        ..Default::default()
    }];

    let spinner = Spinner::start("Thinking...");
    let response = client
        .chat(
            model.clone(),
            messages,
            temperature,
            None,
            effort_level.clone(),
        )
        .await;
    spinner.stop().await;
    let response = response?;

    println!("{} ", "✓".green());
    println!("\n{}:", response_label(&model, &effort_level).cyan());
    let choice = &response.choices[0];
    if choice.message.has_visible_content() {
        println!("{}", wrap::wrap(choice.message.content.as_deref().unwrap()));
    }

    Ok(())
}

/// Prints a saved transcript's user/assistant turns (tool and system messages
/// are omitted since they're internal bookkeeping, not conversation content).
/// No model label on replies, matching the TUI transcript — current model
/// is `/model`'s job, not every reply's.
fn print_transcript(messages: &[store::StoredMessage]) {
    for sm in messages {
        let m = &sm.message;
        match m.role.as_str() {
            "user" => {
                if let Some(content) = &m.content {
                    println!(
                        "{} {}",
                        "❯".green().bold(),
                        wrap::wrap_indented(content, "  ")
                    );
                }
            }
            "assistant" => {
                if let Some(content) = &m.content {
                    println!("\n{} {}\n", "●".cyan(), wrap::wrap_indented(content, "  "));
                }
            }
            _ => {}
        }
    }
}

/// Pulls the user turns out of a resumed session's history, in order, so
/// they can seed the readline history and stay recallable with Up/Down.
fn user_prompts(messages: &[store::StoredMessage]) -> Vec<String> {
    messages
        .iter()
        .filter(|sm| sm.message.role == "user")
        .filter_map(|sm| sm.message.content.clone())
        .collect()
}

/// Handles one non-message line — a `/model`, `/agent`, `/ask`, `/effort`,
/// `/verbose`, `/max-iterations`, `/temperature`, or `/approval` command — updating the session (and
/// `ui`'s live verbosity, which isn't session state) and printing a
/// confirmation in the same "set to X" / "already X" style the TUI's status
/// notices use, so the two front ends read the same way.
#[allow(clippy::too_many_arguments)]
fn apply_submission(
    submission: ui::Submission,
    session: &mut ChatSession,
    ui: &mut TerminalAgentUi,
    default_max_iterations: Option<usize>,
    default_temperature: Option<f32>,
    default_effort_level: Option<String>,
) -> Result<()> {
    match submission {
        ui::Submission::Message(_) => unreachable!("handled by the caller"),
        ui::Submission::SetModel(model) => {
            let changed = model != session.model();
            session.set_model(model)?;
            println!(
                "{} Model {} {}",
                "✓".green(),
                if changed { "set to" } else { "is" },
                session.model()
            );
        }
        ui::Submission::ShowModel => {
            println!(
                "Model: {}",
                response_label(session.model(), &session.effort_level().map(String::from))
            );
        }
        ui::Submission::SetAgentic(agentic) => {
            let changed = agentic != session.is_agentic();
            session.set_agentic(agentic)?;
            let label = if agentic {
                "agent mode (tools enabled)"
            } else {
                "ask mode (no tools)"
            };
            println!(
                "{} {} {label}",
                "✓".green(),
                if changed { "Switched to" } else { "Already in" }
            );
        }
        ui::Submission::SetEffort(effort_level) => {
            let changed = effort_level != session.effort_level().map(String::from);
            session.set_effort_level(effort_level)?;
            let label = session.effort_level().unwrap_or("default").to_string();
            println!(
                "{} Effort {} {label}",
                "✓".green(),
                if changed { "set to" } else { "is" }
            );
        }
        ui::Submission::ResetEffort => {
            let changed = default_effort_level != session.effort_level().map(String::from);
            session.set_effort_level(default_effort_level)?;
            let label = session.effort_level().unwrap_or("default").to_string();
            println!(
                "{} Effort {} {label}",
                "✓".green(),
                if changed { "set to" } else { "is" }
            );
        }
        ui::Submission::ToggleVerbose => {
            let verbose = !session.verbose();
            session.set_verbose(verbose)?;
            ui.set_verbose(verbose);
            println!(
                "{} Verbose mode {}",
                "✓".green(),
                if verbose { "on" } else { "off" }
            );
        }
        ui::Submission::SetMaxIterations(max_iterations) => {
            let changed = max_iterations != session.max_iterations();
            session.set_max_iterations(max_iterations)?;
            let label = session
                .max_iterations()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "default".to_string());
            println!(
                "{} Max iterations {} {label}",
                "✓".green(),
                if changed { "set to" } else { "is" }
            );
        }
        ui::Submission::ResetMaxIterations => {
            let changed = default_max_iterations != session.max_iterations();
            session.set_max_iterations(default_max_iterations)?;
            let label = default_max_iterations
                .map(|n| n.to_string())
                .unwrap_or_else(|| "(not set)".to_string());
            println!(
                "{} Max iterations {} {label}",
                "✓".green(),
                if changed { "set to" } else { "is" }
            );
        }
        ui::Submission::SetTemperature(temperature) => {
            let changed = temperature != session.temperature();
            session.set_temperature(temperature)?;
            let label = session
                .temperature()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "default".to_string());
            println!(
                "{} Temperature {} {label}",
                "✓".green(),
                if changed { "set to" } else { "is" }
            );
        }
        ui::Submission::ResetTemperature => {
            let changed = default_temperature != session.temperature();
            session.set_temperature(default_temperature)?;
            let label = default_temperature
                .map(|n| n.to_string())
                .unwrap_or_else(|| "(not set)".to_string());
            println!(
                "{} Temperature {} {label}",
                "✓".green(),
                if changed { "set to" } else { "is" }
            );
        }
        ui::Submission::SetApproval { category, enabled } => {
            let updated = session.approval().with_category(&category, enabled);
            let changed = updated != *session.approval();
            session.set_approval(updated)?;
            println!(
                "{} Approval {} {}",
                "✓".green(),
                if changed { "set to" } else { "is" },
                format_approval(enabled)
            );
        }
        ui::Submission::ShowApproval => {
            println!("{}", "Approval Settings:".blue());
            print_approval_status(session.approval());
        }
        ui::Submission::SetSandbox(sandbox) => {
            session.set_sandbox(sandbox)?;
            println!("{}", ui::sandbox_notice(sandbox, true).blue());
        }
        ui::Submission::ShowStatus => {
            let approval = session.approval().clone();
            let rows = ui::session_settings_rows(&ui::SessionSettings {
                id: session.short_id(),
                title: session.title(),
                model: session.model(),
                agentic: session.is_agentic(),
                effort_level: session.effort_level(),
                temperature: session.temperature(),
                max_iterations: session.max_iterations(),
                verbose: session.verbose(),
                sandbox: session.sandbox(),
                approval: &approval,
            });
            let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
            println!("\n{}", "Session:".blue());
            for (label, value) in rows {
                // Padded before colouring: the escape codes count toward a
                // format width, so colouring first misaligns the column.
                println!("  {}  {value}", format!("{label:<width$}").bright_black());
            }
            println!();
        }
        ui::Submission::ShowSandbox => {
            println!("{}", ui::sandbox_notice(session.sandbox(), false).blue());
        }
        ui::Submission::UnknownCommand(message) => {
            println!("{} {}", "✗".red(), message);
        }
    }
    Ok(())
}

async fn cmd_session(
    model: Option<String>,
    max_iterations: Option<usize>,
    temperature: Option<f32>,
    effort_level: Option<String>,
    resume: Option<String>,
) -> Result<()> {
    let config = load_config()?;
    let conn = store::open_db()?;

    // The merge of the global config default with any `--flag` override,
    // used as the concrete snapshot for a brand new session below, and as
    // the target `/max-iterations default`/`/temperature default`/
    // `/effort default` resolve to for the rest of this run.
    let default_max_iterations = resolve_max_iterations(&config, max_iterations);
    let default_temperature = resolve_temperature(&config, temperature);
    let default_effort_level = resolve_effort_level(&config, effort_level);

    let mut prior_prompts: Vec<String> = Vec::new();
    let mut session = match resume {
        Some(id_or_prefix) => {
            let summary = resolve_resume_target(&conn, &id_or_prefix)?;
            // A resumed session keeps its own saved settings; `-m` (like any
            // other override flag) only ever applies to a brand new one.
            if model.is_some() {
                println!(
                    "{} Ignoring --model: resumed sessions keep their saved model",
                    "note:".bright_black()
                );
            }
            println!(
                "{} Resuming session {} ({})\n",
                "✓".green(),
                summary.id,
                summary.title
            );
            let (session, history) = ChatSession::resume(conn, &summary, summary.model.clone())?;
            print_transcript(&history);
            prior_prompts = user_prompts(&history);
            session
        }
        // Every new session starts in plain ask mode, same as the TUI's
        // "New session" — `/agent` turns tools on from inside it.
        None => {
            let model = resolve_model(&config, model);
            ChatSession::create(
                conn,
                model,
                KIND_CHAT,
                default_effort_level.clone(),
                default_max_iterations,
                default_temperature,
                config.approval.clone(),
                config.sandbox,
            )?
        }
    };

    let client = Client::new(config)?;

    println!("{}\n", "Starting session (type 'exit' to quit)".blue());

    let mut rl = DefaultEditor::new()?;
    // So Up/Down can recall prompts from before this resume, not just what's
    // typed in the current sitting.
    for prompt in prior_prompts {
        let _ = rl.add_history_entry(prompt);
    }
    // No `-v` here (unlike `agent`) — matching the TUI, a session always
    // starts quiet; `/verbose` is the only way to turn it on. No model
    // label either, again matching the TUI transcript.
    let mut ui = TerminalAgentUi::new(false, false);

    loop {
        let readline = rl.readline(&format!("{} ", "❯".green().bold()));

        let line = match readline {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("{} Session ended", "✓".green());
                break;
            }
            Err(e) => {
                eprintln!("{} Error: {}", "✗".red(), e);
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(line.as_str());

        if line.to_lowercase() == "exit" {
            println!("{} Session ended", "✓".green());
            break;
        }

        match ui::classify(&line) {
            ui::Submission::Message(text) => {
                session.push_user(text);
                if let Err(e) = session.persist_pending() {
                    eprintln!("{} Failed to save message: {}", "✗".red(), e);
                }

                println!();
                let model = session.model().to_string();
                let effort_level = session.effort_level().map(str::to_string);
                let temperature = session.temperature();
                let turn = if session.is_agentic() {
                    let max_iterations = session.max_iterations();
                    let gates = SessionGates::new(session.approval().clone(), session.sandbox());
                    agent::run_agent_turn(
                        &client,
                        &mut ui,
                        session.messages_mut(),
                        &model,
                        max_iterations,
                        temperature,
                        &gates,
                        effort_level,
                    )
                    .await
                } else {
                    agent::run_chat_turn(
                        &client,
                        &mut ui,
                        session.messages_mut(),
                        &model,
                        temperature,
                        effort_level,
                    )
                    .await
                };

                match turn {
                    // The reply itself, and its trailing blank line, are
                    // printed by the UI now — one blank line after every
                    // transcript unit, matching the TUI.
                    Ok(Some(_)) | Ok(None) => {}
                    Err(e) => println!("{} {}\n", "✗".red(), e),
                }

                if let Err(e) = session.persist_pending() {
                    eprintln!("{} Failed to save message: {}", "✗".red(), e);
                }
            }
            submission => {
                if let Err(e) = apply_submission(
                    submission,
                    &mut session,
                    &mut ui,
                    default_max_iterations,
                    default_temperature,
                    default_effort_level.clone(),
                ) {
                    println!("{} {}", "✗".red(), e);
                }
                // One blank line after every transcript unit, matching a
                // message reply and the TUI's own Notice spacing.
                println!();
            }
        }
    }

    println!(
        "{} Session saved. Resume with: comms session --resume {}",
        "✓".green(),
        session.short_id()
    );

    Ok(())
}

async fn cmd_agent(
    task: &str,
    model: Option<String>,
    verbose: bool,
    max_iterations: Option<usize>,
    temperature: Option<f32>,
    effort_level: Option<String>,
) -> Result<()> {
    let config = load_config()?;
    let model = resolve_model(&config, model);
    let max_iterations = resolve_max_iterations(&config, max_iterations);
    let temperature = resolve_temperature(&config, temperature);
    let approval = config.approval.clone();
    let sandbox = config.sandbox;
    let effort_level = resolve_effort_level(&config, effort_level);
    let client = Client::new(config)?;

    println!("{}\n", "Starting agent task...".blue());

    // Unlike `session`, a one-shot task has no other way to show which
    // model answered, so it keeps the label `session`/`tui` dropped.
    let mut ui = TerminalAgentUi::new(verbose, true);
    agent::run_agent(
        &client,
        &mut ui,
        task,
        &model,
        max_iterations,
        temperature,
        &SessionGates::new(approval, sandbox),
        effort_level,
    )
    .await?;

    Ok(())
}

async fn cmd_tui() -> Result<()> {
    let config = load_config()?;

    let context = tui::Context {
        default_model: resolve_model(&config, None),
        effort_level: config.effort_level.clone(),
        max_iterations: config.max_iterations,
        temperature: config.temperature,
        approval: config.approval.clone(),
        sandbox: config.sandbox,
        client: Arc::new(Client::new(config)?),
    };

    tui::run(context).await
}

async fn cmd_sessions(action: Option<SessionCommands>) -> Result<()> {
    let conn = store::open_db()?;

    match action.unwrap_or(SessionCommands::List) {
        SessionCommands::List => {
            let sessions = store::list_sessions(&conn)?;
            if sessions.is_empty() {
                println!("No saved sessions.");
                return Ok(());
            }

            println!("{}\n", "Saved sessions:".blue());
            for s in &sessions {
                println!(
                    "  {}  {}  {}  {}",
                    (&s.id[..8]).bright_black(),
                    format!("[{}]", s.kind).bright_black(),
                    s.model,
                    s.title
                );
            }
        }
        SessionCommands::Show { id } => {
            let summary = store::find_session(&conn, &id)?
                .ok_or_else(|| anyhow::anyhow!("No session found matching '{}'", id))?;
            let messages = store::load_messages(&conn, &summary.id)?;

            println!(
                "{} {} ({}, {})\n",
                "Session:".blue(),
                summary.id,
                summary.kind,
                summary.model
            );
            print_transcript(&messages);
        }
        SessionCommands::Delete { id } => {
            let summary = store::find_session(&conn, &id)?
                .ok_or_else(|| anyhow::anyhow!("No session found matching '{}'", id))?;
            store::delete_session(&conn, &summary.id)?;
            println!(
                "{} Deleted session {} ({})",
                "✓".green(),
                summary.id,
                summary.title
            );
        }
    }

    Ok(())
}
