mod agent;
mod client;
mod config;
mod spinner;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;
use rustyline::DefaultEditor;
use std::io::{self, Write};

use client::{ChatMessage, Client};
use config::{get_config_path, load_config, save_config, ApprovalSettings, VALID_EFFORT_LEVELS};
use spinner::Spinner;

#[derive(Parser)]
#[command(name = "orca")]
#[command(about = "OrcaRouter CLI Agent - A consolidated agent frontend for OrcaRouter", long_about = None)]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Set up OrcaRouter API key
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

    /// Send a prompt to OrcaRouter
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
        Commands::Approval { action } => cmd_approval(action).await?,
        Commands::MaxIterations { value } => cmd_max_iterations(value).await?,
        Commands::EffortLevel { value, clear } => cmd_effort_level(value, clear).await?,
        Commands::Ask {
            prompt,
            model,
            temperature,
        } => cmd_ask(&prompt, model, temperature).await?,
        Commands::Chat { model } => cmd_chat(model).await?,
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
        } => cmd_agent_chat(model, verbose, max_iterations).await?,
    }

    Ok(())
}

async fn cmd_login() -> Result<()> {
    print!("{} ", "Enter your OrcaRouter API key:".blue());
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let api_key = input.trim();

    if api_key.is_empty() {
        eprintln!("{} API key cannot be empty", "✗".red());
        std::process::exit(1);
    }

    let mut config = load_config()?;
    config.api_key = Some(api_key.to_string());
    save_config(&config)?;

    println!("{} API key saved", "✓".green());
    println!(
        "{} {}",
        "Config location:".bright_black(),
        get_config_path()?.display()
    );

    Ok(())
}

async fn cmd_logout() -> Result<()> {
    let mut config = load_config()?;
    config.api_key = None;
    save_config(&config)?;
    println!("{} API key removed", "✓".green());
    Ok(())
}

async fn cmd_status() -> Result<()> {
    let config = load_config()?;
    println!("{}", "OrcaCLI Configuration:".blue());
    println!("  Base URL: {}", config.base_url);
    println!(
        "  API Key: {}",
        if config.api_key.is_some() {
            format!("{} Set", "✓".green())
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
            println!("  orca approval read <on|off>     Set read approval");
            println!("  orca approval write <on|off>    Set write approval");
            println!("  orca approval terminal <on|off> Set terminal approval");
            println!("  orca approval all <on|off>      Set all approvals");
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

async fn cmd_chat(model: Option<String>) -> Result<()> {
    let config = load_config()?;
    let model = resolve_model(&config, model);
    let effort_level = config.effort_level.clone();
    let client = Client::new(config)?;

    println!("{}\n", "Starting chat session (type 'exit' to quit)".blue());

    let mut rl = DefaultEditor::new()?;
    let mut messages: Vec<ChatMessage> = vec![];

    loop {
        let readline = rl.readline(&format!("{} ", "You:".blue()));

        match readline {
            Ok(line) => {
                if line.to_lowercase() == "exit" {
                    println!("{} Chat session ended", "✓".green());
                    break;
                }

                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(line.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });

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
                            messages.push(ChatMessage {
                                role: "assistant".to_string(),
                                content: Some(content.clone()),
                                tool_calls: None,
                                tool_call_id: None,
                            });
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
) -> Result<()> {
    let config = load_config()?;
    let model = resolve_model(&config, model);
    let max_iterations = resolve_max_iterations(&config, max_iterations);
    let approval = config.approval.clone();
    let effort_level = config.effort_level.clone();
    let client = Client::new(config)?;

    println!(
        "{}\n",
        "Starting agent chat session (type 'exit' to quit)".blue()
    );

    let mut rl = DefaultEditor::new()?;
    let mut messages: Vec<ChatMessage> = vec![ChatMessage {
        role: "system".to_string(),
        content: Some(agent::AGENT_CHAT_SYSTEM_PROMPT.to_string()),
        tool_calls: None,
        tool_call_id: None,
    }];

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

    Ok(())
}
