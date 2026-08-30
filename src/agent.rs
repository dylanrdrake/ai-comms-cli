use crate::client::{Client, ChatMessage};
use crate::config::ApprovalSettings;
use crate::tools::{execute_tool, get_tool_definitions};
use anyhow::Result;
use colored::*;
use serde_json::json;
use std::io::{self, Write};

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
) -> Result<Option<String>> {
    let mut messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(task.to_string()),
        tool_calls: None,
    }];

    let tool_definitions = get_tool_definitions();
    let mut iteration = 0;
    let mut final_response = None;

    while iteration < max_iterations {
        iteration += 1;

        if verbose {
            println!("{}", format!("\n[Iteration {}]", iteration).bright_black());
        }

        // Call the LLM with tool definitions
        let response = client
            .chat(
                model.to_string(),
                messages.clone(),
                0.7,
                Some(tool_definitions.clone()),
            )
            .await?;

        let choice = &response.choices[0];

        // If the LLM generated text, show it
        if let Some(content) = &choice.message.content {
            println!("{} {}", "Assistant:".cyan(), content);
            final_response = Some(content.clone());
        }

        // If no tool calls, we're done
        if choice.message.tool_calls.is_none() || choice.message.tool_calls.as_ref().unwrap().is_empty() {
            if verbose {
                println!("{}", "✓ Agent finished".green());
            }
            return Ok(final_response);
        }

        // Add the assistant's response to messages
        messages.push(choice.message.clone());

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

                // Add tool result back to messages
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(result.to_string()),
                    tool_calls: None,
                });
            }
        }
    }

    Err(anyhow::anyhow!("Agent exceeded max iterations"))
}
