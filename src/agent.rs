use crate::client::{Client, ChatMessage};
use crate::tools::{execute_tool, get_tool_definitions};
use anyhow::Result;
use colored::*;
use serde_json::json;

pub async fn run_agent(
    client: &Client,
    task: &str,
    model: &str,
    max_iterations: usize,
    verbose: bool,
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

                // Execute the tool
                let tool_result = execute_tool(tool_name, &tool_call.function.arguments).await;

                let result = match tool_result {
                    Ok(result) => result,
                    Err(e) => json!({ "error": e.to_string() }),
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
