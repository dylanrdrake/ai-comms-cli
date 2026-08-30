mod agent;
mod client;
mod config;
mod crypto;
mod spinner;
mod store;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;
use rustyline::DefaultEditor;
use std::io::{self, Write};

use client::{ChatMessage, Client};
use config::{
    clear_api_key, get_api_key, get_config_path, load_config, save_config, set_api_key,
    ApprovalSettings, VALID_EFFORT_LEVELS, VALID_EFFORT_STYLES,
};
use spinner::Spinner;
use store::{KIND_AGENT_CHAT, KIND_CHAT};

#[derive(Parser)]
#[command(name = "comms")]
#[command(about = "AI Comms CLI - An OpenAI-compatible frontend for any LLM provider", long_about = None)]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
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

        /// Clear the stored default model (falls back to orcarouter/auto)
        #[arg(long)]
        clear: bool,
    },

    /// View or set the API base URL, to point at any OpenAI-compatible service
    Endpoint {
        /// Base URL to use, e.g. https://openrouter.ai/api/v1 (omit to show the current value)
        url: Option<String>,

        /// Clear the stored endpoint (falls back to the OrcaRouter default)
        #[arg(long)]
        clear: bool,
    },

    /// View or set how the reasoning effort level is sent to the provider
    EffortStyle {
        /// Style to set: flat, nested, or none (omit to show the current value)
        value: Option<String>,

        /// Clear the stored effort style (falls back to "flat")
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

        /// Temperature (0-2)
        #[arg(short, long, default_value_t = 0.7)]
        temperature: f32,
    },

    /// Interactive chat session
    Chat {
        /// Model to use (overrides the persistent default for this call)
        #[arg(short, long)]
        model: Option<String>,

        /// Resume a saved session by id (or unique id prefix); pass with no
        /// value to pick from a list of your saved chat sessions
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
    },

    /// Interactive agentic chat session (tools + persistent conversation context)
    AgentChat {
        /// Model to use (overrides the persistent default for this call)
        #[arg(short, long)]
        model: Option<String>,

        /// Show detailed agent iterations
        #[arg(short, long)]
        verbose: bool,

        /// Maximum number of iterations per turn (overrides the persistent default for this call)
        #[arg(long)]
        max_iterations: Option<usize>,

        /// Resume a saved session by id (or unique id prefix); pass with no
        /// value to pick from a list of your saved agent-chat sessions
        #[arg(long, num_args = 0..=1, default_missing_value = PICK_SESSION_SENTINEL)]
        resume: Option<String>,
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

fn parse_bool(s: &str) -> Result<bool, String> {
    match s.to_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => Err(format!(
            "Invalid boolean value: '{}'. Use true/false, on/off, yes/no, or 1/0",
            s
        )),
    }
}

const DEFAULT_MODEL: &str = "orcarouter/auto";

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
    kind: &str,
) -> Result<store::SessionSummary> {
    if id_or_prefix != PICK_SESSION_SENTINEL {
        return store::find_session(conn, id_or_prefix)?
            .ok_or_else(|| anyhow::anyhow!("No session found matching '{}'", id_or_prefix));
    }

    let sessions: Vec<store::SessionSummary> = store::list_sessions(conn)?
        .into_iter()
        .filter(|s| s.kind == kind)
        .collect();

    if sessions.is_empty() {
        anyhow::bail!(
            "No saved {} sessions to resume",
            if kind == KIND_CHAT { "chat" } else { "agent-chat" }
        );
    }

    println!("{}\n", "Select a session to resume:".blue());
    for (i, s) in sessions.iter().enumerate() {
        println!(
            "  {}. {}  {}",
            i + 1,
            (&s.id[..8]).bright_black(),
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
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn resolve_max_iterations(config: &config::Config, cli_value: Option<usize>) -> usize {
    cli_value.unwrap_or(config.max_iterations)
}

fn response_label(model: &str, effort_level: &Option<String>) -> String {
    match effort_level {
        Some(effort) => format!("{} ({})", model, effort),
        None => model.to_string(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Login => cmd_login().await?,
        Commands::Logout => cmd_logout().await?,
        Commands::Status => cmd_status().await?,
        Commands::Models => cmd_models().await?,
        Commands::Model { name, clear } => cmd_model(name, clear).await?,
        Commands::Endpoint { url, clear } => cmd_endpoint(url, clear).await?,
        Commands::EffortStyle { value, clear } => cmd_effort_style(value, clear).await?,
        Commands::Headers { action } => cmd_headers(action).await?,
        Commands::Approval { action } => cmd_approval(action).await?,
        Commands::MaxIterations { value } => cmd_max_iterations(value).await?,
        Commands::EffortLevel { value, clear } => cmd_effort_level(value, clear).await?,
        Commands::Ask {
            prompt,
            model,
            temperature,
        } => cmd_ask(&prompt, model, temperature).await?,
        Commands::Chat { model, resume } => cmd_chat(model, resume).await?,
        Commands::Agent {
            task,
            model,
            verbose,
            max_iterations,
        } => cmd_agent(&task, model, verbose, max_iterations).await?,
        Commands::AgentChat {
            model,
            verbose,
            max_iterations,
            resume,
        } => cmd_agent_chat(model, verbose, max_iterations, resume).await?,
        Commands::Sessions { action } => cmd_sessions(action).await?,
    }

    Ok(())
}

async fn cmd_login() -> Result<()> {
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
    println!("{}", "AI Comms CLI Configuration:".blue());
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
        config.default_model.as_deref().unwrap_or(DEFAULT_MODEL)
    );
    println!("  Max iterations: {}", config.max_iterations);
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
    if !config.extra_headers.is_empty() {
        println!("  Extra headers: {}", config.extra_headers.len());
    }
    println!("  Config file: {}", get_config_path()?.display());
    println!("\n{}", "Approval Settings:".blue());
    print_approval_status(&config.approval);
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
            DEFAULT_MODEL
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
                config.default_model.as_deref().unwrap_or(DEFAULT_MODEL)
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

async fn cmd_max_iterations(value: Option<usize>) -> Result<()> {
    let mut config = load_config()?;

    match value {
        Some(0) => {
            eprintln!("{} max-iterations must be greater than 0", "✗".red());
            std::process::exit(1);
        }
        Some(value) => {
            config.max_iterations = value;
            save_config(&config)?;
            println!("{} Default max iterations set to {}", "✓".green(), value);
        }
        None => {
            println!("Current default max iterations: {}", config.max_iterations);
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
            let normalized = value.to_lowercase();
            if !VALID_EFFORT_LEVELS.contains(&normalized.as_str()) {
                eprintln!(
                    "{} Invalid effort level '{}'. Valid values: {}",
                    "✗".red(),
                    value,
                    VALID_EFFORT_LEVELS.join(", ")
                );
                std::process::exit(1);
            }
            config.effort_level = Some(normalized.clone());
            save_config(&config)?;
            println!("{} Effort level set to {}", "✓".green(), normalized);
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

async fn cmd_ask(prompt: &str, model: Option<String>, temperature: f32) -> Result<()> {
    let config = load_config()?;
    let model = resolve_model(&config, model);
    let effort_level = config.effort_level.clone();
    let client = Client::new(config)?;

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(prompt.to_string()),
        tool_calls: None,
        tool_call_id: None,
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
    if let Some(content) = &response.choices[0].message.content {
        println!("{}", content);
    }

    Ok(())
}

/// Prints a saved transcript's user/assistant turns (tool and system messages
/// are omitted since they're internal bookkeeping, not conversation content).
fn print_transcript(messages: &[ChatMessage], model_label: &str) {
    for m in messages {
        match m.role.as_str() {
            "user" => {
                if let Some(content) = &m.content {
                    println!("{} {}", "You:".blue(), content);
                }
            }
            "assistant" => {
                if let Some(content) = &m.content {
                    println!("{} {}\n", format!("{}:", model_label).cyan(), content);
                }
            }
            _ => {}
        }
    }
}

async fn cmd_chat(model: Option<String>, resume: Option<String>) -> Result<()> {
    let config = load_config()?;
    let conn = store::open_db()?;

    let (session_id, model, mut messages) = match resume {
        Some(id_or_prefix) => {
            let summary = resolve_resume_target(&conn, &id_or_prefix, KIND_CHAT)?;
            if summary.kind != KIND_CHAT {
                anyhow::bail!(
                    "Session {} is a {} session, resume it with `comms agent-chat --resume {}` instead",
                    summary.id,
                    summary.kind,
                    summary.id
                );
            }
            let history = store::load_messages(&conn, &summary.id)?;
            let model = model.unwrap_or_else(|| summary.model.clone());
            println!(
                "{} Resuming session {} ({})\n",
                "✓".green(),
                summary.id,
                summary.title
            );
            print_transcript(&history, &response_label(&model, &config.effort_level));
            (summary.id, model, history)
        }
        None => {
            let model = resolve_model(&config, model);
            let session_id = store::create_session(&conn, &model, KIND_CHAT)?;
            (session_id, model, Vec::new())
        }
    };

    let effort_level = config.effort_level.clone();
    let client = Client::new(config)?;

    println!("{}\n", "Starting chat session (type 'exit' to quit)".blue());

    let mut rl = DefaultEditor::new()?;
    let mut title_set = messages.iter().any(|m| m.role == "user");

    loop {
        let readline = rl.readline(&format!("{} ", "You:".blue()));

        match readline {
            Ok(line) => {
                if line.to_lowercase() == "exit" {
                    println!("{} Chat session ended", "✓".green());
                    break;
                }

                let seq = messages.len();
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(line.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });
                if let Err(e) = store::append_message(&conn, &session_id, seq, &messages[seq]) {
                    eprintln!("{} Failed to save message: {}", "✗".red(), e);
                }
                if !title_set {
                    let title = store::derive_title(&messages);
                    if let Err(e) = store::set_session_title(&conn, &session_id, &title) {
                        eprintln!("{} Failed to save session title: {}", "✗".red(), e);
                    }
                    title_set = true;
                }

                let spinner = Spinner::start("Thinking...");
                let result = client
                    .chat(
                        model.clone(),
                        messages.clone(),
                        0.7,
                        None,
                        effort_level.clone(),
                    )
                    .await;
                spinner.stop().await;

                match result {
                    Ok(response) => {
                        if let Some(content) = &response.choices[0].message.content {
                            println!(
                                "{} {}\n",
                                format!("{}:", response_label(&model, &effort_level)).cyan(),
                                content
                            );
                            let seq = messages.len();
                            let assistant_message = ChatMessage {
                                role: "assistant".to_string(),
                                content: Some(content.clone()),
                                tool_calls: None,
                                tool_call_id: None,
                            };
                            messages.push(assistant_message.clone());
                            if let Err(e) =
                                store::append_message(&conn, &session_id, seq, &assistant_message)
                            {
                                eprintln!("{} Failed to save message: {}", "✗".red(), e);
                            }
                        }
                    }
                    Err(e) => {
                        println!("{} {}\n", "✗".red(), e);
                    }
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("{} Chat session ended", "✓".green());
                break;
            }
            Err(e) => {
                eprintln!("{} Error: {}", "✗".red(), e);
                break;
            }
        }
    }

    println!(
        "{} Session saved. Resume with: comms chat --resume {}",
        "✓".green(),
        &session_id[..8]
    );

    Ok(())
}

async fn cmd_agent(
    task: &str,
    model: Option<String>,
    verbose: bool,
    max_iterations: Option<usize>,
) -> Result<()> {
    let config = load_config()?;
    let model = resolve_model(&config, model);
    let max_iterations = resolve_max_iterations(&config, max_iterations);
    let approval = config.approval.clone();
    let effort_level = config.effort_level.clone();
    let client = Client::new(config)?;

    println!("{}\n", "Starting agent task...".blue());

    agent::run_agent(
        &client,
        task,
        &model,
        max_iterations,
        verbose,
        &approval,
        effort_level,
    )
    .await?;

    Ok(())
}

async fn cmd_agent_chat(
    model: Option<String>,
    verbose: bool,
    max_iterations: Option<usize>,
    resume: Option<String>,
) -> Result<()> {
    let config = load_config()?;
    let conn = store::open_db()?;

    let (session_id, model, mut messages, mut saved_len) = match resume {
        Some(id_or_prefix) => {
            let summary = resolve_resume_target(&conn, &id_or_prefix, KIND_AGENT_CHAT)?;
            if summary.kind != KIND_AGENT_CHAT {
                anyhow::bail!(
                    "Session {} is a {} session, resume it with `comms chat --resume {}` instead",
                    summary.id,
                    summary.kind,
                    summary.id
                );
            }
            let history = store::load_messages(&conn, &summary.id)?;
            let model = model.unwrap_or_else(|| summary.model.clone());
            println!(
                "{} Resuming session {} ({})\n",
                "✓".green(),
                summary.id,
                summary.title
            );
            print_transcript(&history, &response_label(&model, &config.effort_level));
            let len = history.len();
            (summary.id, model, history, len)
        }
        None => {
            let model = resolve_model(&config, model);
            let session_id = store::create_session(&conn, &model, KIND_AGENT_CHAT)?;
            let messages = vec![ChatMessage {
                role: "system".to_string(),
                content: Some(agent::AGENT_CHAT_SYSTEM_PROMPT.to_string()),
                tool_calls: None,
                tool_call_id: None,
            }];
            store::append_message(&conn, &session_id, 0, &messages[0])?;
            let len = messages.len();
            (session_id, model, messages, len)
        }
    };

    let max_iterations = resolve_max_iterations(&config, max_iterations);
    let approval = config.approval.clone();
    let effort_level = config.effort_level.clone();
    let client = Client::new(config)?;

    println!(
        "{}\n",
        "Starting agent chat session (type 'exit' to quit)".blue()
    );

    let mut rl = DefaultEditor::new()?;
    let mut title_set = messages.iter().any(|m| m.role == "user");

    loop {
        let readline = rl.readline(&format!("{} ", "You:".blue()));

        match readline {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }

                if line.to_lowercase() == "exit" {
                    println!("{} Agent chat session ended", "✓".green());
                    break;
                }

                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(line.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });

                if let Err(e) = agent::run_agent_turn(
                    &client,
                    &mut messages,
                    &model,
                    max_iterations,
                    verbose,
                    &approval,
                    effort_level.clone(),
                )
                .await
                {
                    println!("{} {}\n", "✗".red(), e);
                }

                // Persist any messages appended by this turn (the user message
                // plus whatever the agent loop added: assistant/tool turns).
                for (seq, message) in messages.iter().enumerate().skip(saved_len) {
                    if let Err(e) = store::append_message(&conn, &session_id, seq, message) {
                        eprintln!("{} Failed to save message: {}", "✗".red(), e);
                    }
                }
                saved_len = messages.len();

                if !title_set {
                    let title = store::derive_title(&messages);
                    if let Err(e) = store::set_session_title(&conn, &session_id, &title) {
                        eprintln!("{} Failed to save session title: {}", "✗".red(), e);
                    }
                    title_set = true;
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("{} Agent chat session ended", "✓".green());
                break;
            }
            Err(e) => {
                eprintln!("{} Error: {}", "✗".red(), e);
                break;
            }
        }
    }

    println!(
        "{} Session saved. Resume with: comms agent-chat --resume {}",
        "✓".green(),
        &session_id[..8]
    );

    Ok(())
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
            print_transcript(&messages, &summary.model);
        }
        SessionCommands::Delete { id } => {
            let summary = store::find_session(&conn, &id)?
                .ok_or_else(|| anyhow::anyhow!("No session found matching '{}'", id))?;
            store::delete_session(&conn, &summary.id)?;
            println!("{} Deleted session {} ({})", "✓".green(), summary.id, summary.title);
        }
    }

    Ok(())
}
