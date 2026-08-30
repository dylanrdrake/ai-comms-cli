use crate::client::{Client, ChatMessage};
use crate::config::ApprovalSettings;
use crate::spinner::Spinner;
use crate::tools::{execute_tool, get_tool_definitions};
use anyhow::Result;
use colored::*;
use serde_json::json;
use std::io::{self, Write};

/// Seeds a continuous agent-chat session so the model treats the growing
/// transcript as history to build on, not a backlog of tasks to redo.
pub const AGENT_CHAT_SYSTEM_PROMPT: &str = "You are a coding agent operating in a continuous \
interactive chat session. The conversation history may contain earlier user requests that you \
already completed, along with your replies and any tool calls/results for them. Treat each new \
user message as the only request currently being asked of you - use earlier turns purely as \
background context. Do not restate, re-summarize, or redo work from earlier turns unless the \
user explicitly asks you to.";

fn get_tool_category(tool_name: &str) -> &'static str {
    match tool_name {
        "read_file" | "list_files" => "read",
        "write_file" | "replace_in_file" => "write",
        "run_terminal_command" => "terminal",
        _ => "unknown",
    }
}

fn requires_approval(tool_name: &str, approval: &ApprovalSettings) -> bool {
    match get_tool_category(tool_name) {
        "read" => approval.read_disk,
        "write" => approval.write_disk,
        "terminal" => approval.terminal,
        _ => true,
    }
}

fn prompt_user_approval(tool_name: &str, arguments: &str) -> Result<bool> {
    let category = get_tool_category(tool_name);
    let category_label = match category {
        "read" => "Read from disk",
        "write" => "Write to disk",
        "terminal" => "Terminal command",
        _ => "Unknown action",
    };

    println!("\n{} {} requested:", "⚠".yellow(), category_label);
    println!("  Tool: {}", tool_name.cyan());
    
    // Parse and display arguments nicely
    if let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) {
        if let Some(obj) = args.as_object() {
            for (key, value) in obj {
                let display_value = if key == "content" {
                    // Truncate long content
                    let s = value.as_str().map(|s| s.to_string()).unwrap_or_else(|| value.to_string());
                    if s.len() > 100 {
                        format!("{}... ({} chars)", &s[..100], s.len())
                    } else {
                        s
                    }
                } else {
                    value.as_str().map(|s| s.to_string()).unwrap_or_else(|| value.to_string())
                };
                println!("  {}: {}", key, display_value.bright_black());
            }
        }
    }

    print!("\n{} ", "Allow? [y/N]:".blue());
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let response = input.trim().to_lowercase();

    Ok(response == "y" || response == "yes")
}

pub async fn run_agent(
    client: &Client,
    task: &str,
    model: &str,
    max_iterations: usize,
    verbose: bool,
    approval: &ApprovalSettings,
    effort_level: Option<String>,
) -> Result<Option<String>> {
    let mut messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(task.to_string()),
        tool_calls: None,
        tool_call_id: None,
    }];

    run_agent_turn(
        client,
        &mut messages,
        model,
        max_iterations,
        verbose,
        approval,
        effort_level,
    )
    .await
}

/// Runs the tool-calling agent loop against an existing message history,
/// appending the assistant/tool messages produced along the way so the
/// history can be reused for a follow-up turn (e.g. a continuous chat).
pub async fn run_agent_turn(
    client: &Client,
    messages: &mut Vec<ChatMessage>,
    model: &str,
    max_iterations: usize,
    verbose: bool,
    approval: &ApprovalSettings,
    effort_level: Option<String>,
) -> Result<Option<String>> {
    let tool_definitions = get_tool_definitions();
    let mut iteration = 0;
    let mut final_response = None;

    while iteration < max_iterations {
        iteration += 1;

        if verbose {
            println!("{}", format!("\n[Iteration {}]", iteration).bright_black());
        }

        // Call the LLM with tool definitions
        let spinner = Spinner::start("Thinking...");
        let response = client
            .chat(
                model.to_string(),
                messages.clone(),
                0.7,
                Some(tool_definitions.clone()),
                effort_level.clone(),
            )
            .await;
        spinner.stop().await;
        let response = response?;

        let choice = &response.choices[0];

        // If the LLM generated text, show it
        if let Some(content) = &choice.message.content {
            let label = match &effort_level {
                Some(effort) => format!("{} ({}):", model, effort),
                None => format!("{}:", model),
            };
            println!("{} {}", label.cyan(), content);
            final_response = Some(content.clone());
        }

        let no_tool_calls = choice.message.tool_calls.is_none()
            || choice.message.tool_calls.as_ref().unwrap().is_empty();

        // Record the assistant's turn in history before deciding whether to
        // keep looping, so a plain text answer (no tool calls) is still
        // remembered on the next turn instead of vanishing from context.
        messages.push(choice.message.clone());

        if no_tool_calls {
            if verbose {
                println!("{}", "✓ Agent finished".green());
            }
            return Ok(final_response);
        }

        // Process each tool call
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tool_call in tool_calls {
                let tool_name = &tool_call.function.name;

                if verbose {
                    println!("{} {}", "→ Calling tool:".yellow(), tool_name);
                    println!("{} {}", "  Input:".bright_black(), tool_call.function.arguments);
                }

                // Check if approval is needed
                let approved = if requires_approval(tool_name, approval) {
                    prompt_user_approval(tool_name, &tool_call.function.arguments)?
                } else {
                    true
                };

                let result = if approved {
                    // Execute the tool
                    let tool_result = execute_tool(tool_name, &tool_call.function.arguments).await;

                    match tool_result {
                        Ok(result) => result,
                        Err(e) => json!({ "error": e.to_string() }),
                    }
                } else {
                    println!("{} Tool execution denied by user", "✗".red());
                    json!({ "error": "User denied permission for this action" })
                };

                if verbose {
                    println!("{} {}", "  Result:".bright_black(), result);
                }

                // Add tool result back to messages, threaded to the call that produced it
                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(result.to_string()),
                    tool_calls: None,
                    tool_call_id: Some(tool_call.id.clone()),
                });
            }
        }
    }

    Err(anyhow::anyhow!("Agent exceeded max iterations"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categorizes_known_tools() {
        assert_eq!(get_tool_category("read_file"), "read");
        assert_eq!(get_tool_category("list_files"), "read");
        assert_eq!(get_tool_category("write_file"), "write");
        assert_eq!(get_tool_category("replace_in_file"), "write");
        assert_eq!(get_tool_category("run_terminal_command"), "terminal");
        assert_eq!(get_tool_category("something_else"), "unknown");
    }

    fn settings(read: bool, write: bool, terminal: bool) -> ApprovalSettings {
        ApprovalSettings {
            read_disk: read,
            write_disk: write,
            terminal,
        }
    }

    #[test]
    fn requires_approval_respects_per_category_flags() {
        let all_on = settings(true, true, true);
        assert!(requires_approval("read_file", &all_on));
        assert!(requires_approval("list_files", &all_on));
        assert!(requires_approval("write_file", &all_on));
        assert!(requires_approval("replace_in_file", &all_on));
        assert!(requires_approval("run_terminal_command", &all_on));

        let all_off = settings(false, false, false);
        assert!(!requires_approval("read_file", &all_off));
        assert!(!requires_approval("list_files", &all_off));
        assert!(!requires_approval("write_file", &all_off));
        assert!(!requires_approval("replace_in_file", &all_off));
        assert!(!requires_approval("run_terminal_command", &all_off));
    }

    #[test]
    fn requires_approval_is_independent_per_category() {
        // Only write approval enabled: read and terminal should be auto-approved,
        // write should still prompt.
        let write_only = settings(false, true, false);
        assert!(!requires_approval("read_file", &write_only));
        assert!(requires_approval("write_file", &write_only));
        assert!(!requires_approval("run_terminal_command", &write_only));
    }

    #[test]
    fn unknown_tools_always_require_approval() {
        // Fail-safe: an unrecognized tool name must default to requiring approval,
        // even when every known category is set to auto-approve.
        let all_off = settings(false, false, false);
        assert!(requires_approval("some_future_tool", &all_off));
    }
}
