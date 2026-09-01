//! The CLI's [`AgentUi`]: renders agent progress to stdout and asks for tool
//! approval on stdin. This is the only place the agent loop's output is
//! formatted, so a different front end can present the same events however
//! it likes without touching the loop itself.

use crate::spinner::Spinner;
use crate::ui::{
    json_fields, parse_yes_no, primary_argument, response_label, tool_call_fields, AgentEvent,
    AgentUi, ApprovalRequest,
};
use crate::wrap;
use anyhow::Result;
use colored::*;
use std::future::Future;
use std::io::{self, Write};

pub struct TerminalAgentUi {
    /// Mirrors the `-v` flag: gates the full argument/result dump. The
    /// marker-and-name notice below it — matching what the TUI always shows
    /// — is not gated, so plain `agent`/`agent-chat` isn't silent about
    /// tool calls the way it used to be.
    verbose: bool,
    /// Whether a reply is prefixed with `model (effort):`. On for one-shot
    /// `agent` calls, where there's no other way to see what answered; off
    /// for `session`, matching the TUI transcript, which dropped the same
    /// label — current model there is `/model`'s job, not every reply's.
    show_model_label: bool,
    /// Live only between `RequestStarted` and `RequestFinished`.
    spinner: Option<Spinner>,
    /// The call's arguments, held from `ToolCallStarted` to whichever event
    /// settles it, so the notice that settles it can still name the file
    /// or command being acted on, and so `ToolCallCompleted` can tell a
    /// denied call (already reported by `ToolCallDenied`) from one that
    /// actually ran. Tool calls run one at a time, so there's never more
    /// than one in flight to track.
    pending_arguments: Option<String>,
}

impl TerminalAgentUi {
    pub fn new(verbose: bool, show_model_label: bool) -> Self {
        TerminalAgentUi {
            verbose,
            show_model_label,
            spinner: None,
            pending_arguments: None,
        }
    }

    /// Flips the `-v`-equivalent detail level live, for `/verbose` in a
    /// `session` loop. Takes effect from the next event on.
    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }
}

impl AgentUi for TerminalAgentUi {
    fn event(&mut self, event: AgentEvent) -> impl Future<Output = ()> + Send {
        async move {
            match event {
                AgentEvent::IterationStarted { iteration } => {
                    if self.verbose {
                        println!("{}", format!("\n[Iteration {}]", iteration).bright_black());
                    }
                }
                AgentEvent::RequestStarted => {
                    self.spinner = Some(Spinner::start("Thinking..."));
                }
                AgentEvent::RequestFinished => {
                    if let Some(spinner) = self.spinner.take() {
                        spinner.stop().await;
                    }
                }
                // Deliberately ignored: a scrolling terminal can't re-wrap
                // text it has already printed, so the CLI buffers and renders
                // the complete `AssistantMessage` below instead.
                AgentEvent::AssistantDelta { .. } => {}
                AgentEvent::AssistantMessage {
                    model,
                    effort_level,
                    text,
                } => {
                    if self.show_model_label {
                        let label = format!("{}:", response_label(&model, &effort_level));
                        println!("{} {}", label.cyan(), wrap::wrap(&text));
                    } else {
                        // Matches the TUI transcript's dot marker, now that
                        // neither shows a model label on every reply — and,
                        // like the TUI's gutter, a wrapped continuation
                        // line lines up under the first rather than
                        // resuming at column 0.
                        println!("{} {}", "●".cyan(), wrap::wrap_indented(&text, "  "));
                    }
                }
                AgentEvent::ToolCallStarted { name, arguments } => {
                    // Matches the TUI transcript's gear marker for a tool
                    // call; completion still gets its own ✓/✗ line below,
                    // the CLI's equivalent of the TUI's trailing status.
                    tool_notice("⚙".magenta(), &name, &arguments);
                    if self.verbose {
                        print_fields(&tool_call_fields(&name, &arguments));
                    }
                    self.pending_arguments = Some(arguments);
                }
                AgentEvent::ToolCallDenied { name } => {
                    // Consumes the pending call so the ToolCallCompleted
                    // that always follows a denial knows not to report the
                    // same call again as if it had succeeded.
                    let arguments = self.pending_arguments.take().unwrap_or_default();
                    tool_notice("✗".red(), &format!("{name} denied"), &arguments);
                }
                AgentEvent::ToolCallCompleted { name, result } => {
                    // Only the non-denied path reaches here with a pending
                    // entry; a denial already reported itself and cleared it.
                    if self.pending_arguments.take().is_none() {
                        return;
                    }
                    println!("{} {}", "✓".green(), name.bold());
                    if self.verbose {
                        print_fields(&json_fields(&result));
                    }
                }
                AgentEvent::Error { message } => {
                    println!("{} {}", "✗".red(), message);
                }
                AgentEvent::TurnFinished => {
                    if self.verbose {
                        println!("{}", "✓ Agent finished".green());
                    }
                }
            }
        }
    }

    fn approve(&mut self, request: ApprovalRequest) -> impl Future<Output = Result<bool>> + Send {
        async move { self.prompt_approval(request) }
    }
}

impl TerminalAgentUi {
    /// The blocking stdin prompt behind [`AgentUi::approve`], kept separate
    /// so the async wrapper stays trivial.
    fn prompt_approval(&mut self, request: ApprovalRequest) -> Result<bool> {
        let category_label = match request.category {
            "read" => "Read from disk",
            "write" => "Write to disk",
            "terminal" => "Terminal command",
            _ => "Unknown action",
        };

        println!("\n{} {} requested:", "⚠".yellow(), category_label);
        println!("  Tool: {}", request.tool_name.cyan());

        // Parse and display arguments nicely
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(&request.arguments) {
            if let Some(obj) = args.as_object() {
                for (key, value) in obj {
                    let display_value = if key == "content" {
                        // Truncate long content
                        let s = value
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| value.to_string());
                        if s.len() > 100 {
                            format!("{}... ({} chars)", &s[..100], s.len())
                        } else {
                            s
                        }
                    } else {
                        value
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| value.to_string())
                    };
                    println!("  {}: {}", key, display_value.bright_black());
                }
            }
        }

        print!("\n{} ", "Allow? [y/N]:".blue());
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        Ok(parse_yes_no(&input))
    }
}

/// Prints `marker name  detail`, where `detail` is the file path or command
/// the call is acting on when its arguments have one — the same terse
/// identification the TUI always shows for a tool call, regardless of `-v`.
fn tool_notice(marker: ColoredString, name: &str, arguments: &str) {
    match primary_argument(arguments) {
        Some(detail) => println!("{marker} {}  {}", name.bold(), detail.bright_black()),
        None => println!("{marker} {}", name.bold()),
    }
}

/// The verbose-only per-field breakdown under a tool notice, matching the
/// TUI's indentation for the same data.
fn print_fields(fields: &[(String, String)]) {
    for (key, shown) in fields {
        println!("     {}  {}", key.bright_black(), shown);
    }
}
