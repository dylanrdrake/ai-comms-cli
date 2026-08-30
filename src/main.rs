mod agent;
mod client;
mod config;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;
use rustyline::DefaultEditor;
use std::io::{self, Write};

use client::{ChatMessage, Client};
use config::{load_config, save_config, get_config_path};

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

        /// Maximum number of iterations
        #[arg(long, default_value_t = 10)]
        max_iterations: usize,
    },
}

const DEFAULT_MODEL: &str = "orcarouter/auto";

fn resolve_model(config: &config::Config, cli_model: Option<String>) -> String {
    cli_model
        .or_else(|| config.default_model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
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
    println!("  Config file: {}", get_config_path()?.display());
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

async fn cmd_models() -> Result<()> {
    let config = load_config()?;
    let client = Client::new(config)?;

    let spinner = "Fetching models...".yellow();
    print!("{} ", spinner);
    io::stdout().flush()?;

    let models = client.list_models().await?;

    println!("\r{} ", "✓".green());
    println!("\n{}\n", format!("Available models ({}):", models.len()).blue());

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
    let client = Client::new(config)?;

    let spinner = "Thinking...".yellow();
    print!("{} ", spinner);
    io::stdout().flush()?;

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(prompt.to_string()),
        tool_calls: None,
    }];

    let response = client.chat(model, messages, temperature, None).await?;

    println!("\r{} ", "✓".green());
    println!("\n{}:", "Assistant".cyan());
    if let Some(content) = &response.choices[0].message.content {
        println!("{}", content);
    }

    Ok(())
}

async fn cmd_chat(model: Option<String>) -> Result<()> {
    let config = load_config()?;
    let model = resolve_model(&config, model);
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
                });

                let spinner = "...".yellow();
                print!("{} ", spinner);
                io::stdout().flush()?;

                match client
                    .chat(model.clone(), messages.clone(), 0.7, None)
                    .await
                {
                    Ok(response) => {
                        println!("\r   ");
                        if let Some(content) = &response.choices[0].message.content {
                            println!("{} {}\n", "Assistant:".cyan(), content);
                            messages.push(ChatMessage {
                                role: "assistant".to_string(),
                                content: Some(content.clone()),
                                tool_calls: None,
                            });
                        }
                    }
                    Err(e) => {
                        println!("\r{} {}\n", "✗".red(), e);
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
    max_iterations: usize,
) -> Result<()> {
    let config = load_config()?;
    let model = resolve_model(&config, model);
    let client = Client::new(config)?;

    println!("{}\n", "Starting agent task...".blue());

    agent::run_agent(&client, task, &model, max_iterations, verbose).await?;

    Ok(())
}
