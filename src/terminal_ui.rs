//! The CLI's [`AgentUi`]: renders agent progress to stdout and asks for tool
//! approval on stdin. This is the only place the agent loop's output is
//! formatted, so a different front end can present the same events however
//! it likes without touching the loop itself.

use crate::spinner::Spinner;
use crate::ui::{response_label, AgentEvent, AgentUi, ApprovalRequest};
use crate::wrap;
use anyhow::Result;
use colored::*;
use std::future::Future;
use std::io::{self, Write};

pub struct TerminalAgentUi {
    /// Mirrors the `-v` flag: gates the step-by-step iteration/tool logs.
    /// Assistant text and denial notices are always shown.
    verbose: bool,
    /// Live only between `RequestStarted` and `RequestFinished`.
    spinner: Option<Spinner>,
}

impl TerminalAgentUi {
    pub fn new(verbose: bool) -> Self {
        TerminalAgentUi {
            verbose,
            spinner: None,
        }
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
                    let label = format!("{}:", response_label(&model, &effort_level));
                    println!("{} {}", label.cyan(), wrap::wrap(&text));
                }
                AgentEvent::ToolCallStarted { name, arguments } => {
                    if self.verbose {
                        println!("{} {}", "→ Calling tool:".yellow(), name);
                        println!("{} {}", "  Input:".bright_black(), arguments);
                    }
                }
                AgentEvent::ToolCallDenied { .. } => {
                    println!("{} Tool execution denied by user", "✗".red());
                }
                AgentEvent::ToolCallCompleted { result, .. } => {
                    if self.verbose {
                        println!("{} {}", "  Result:".bright_black(), result);
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

        Ok(parse_approval_response(&input))
    }
}

/// Interprets a typed answer to the `Allow? [y/N]:` prompt. Anything other
/// than an explicit yes denies the action.
fn parse_approval_response(input: &str) -> bool {
    let response = input.trim().to_lowercase();
    response == "y" || response == "yes"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_accepts_only_explicit_yes() {
        assert!(parse_approval_response("y"));
        assert!(parse_approval_response("yes"));
        assert!(parse_approval_response("  YES  \n"));
        assert!(parse_approval_response("Y\n"));
    }

    #[test]
    fn approval_denies_everything_else() {
        assert!(!parse_approval_response("n"));
        assert!(!parse_approval_response("no"));
        assert!(!parse_approval_response(""));
        assert!(!parse_approval_response("\n"));
        assert!(!parse_approval_response("maybe"));
        // Fails closed: a stray answer is a denial, never an approval.
        assert!(!parse_approval_response("yep"));
    }
}
