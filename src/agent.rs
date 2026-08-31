use crate::client::{ChatMessage, Client};
use crate::config::ApprovalSettings;
use crate::tools::{execute_tool, get_tool_definitions};
use crate::ui::{AgentEvent, AgentUi, ApprovalRequest};
use anyhow::Result;
use serde_json::json;

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

pub async fn run_agent(
    client: &Client,
    ui: &mut impl AgentUi,
    task: &str,
    model: &str,
    max_iterations: usize,
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
        ui,
        &mut messages,
        model,
        max_iterations,
        approval,
        effort_level,
    )
    .await
}

/// Runs the tool-calling agent loop against an existing message history,
/// appending the assistant/tool messages produced along the way so the
/// history can be reused for a follow-up turn (e.g. a continuous chat).
///
/// Progress is reported to `ui` rather than printed, and any tool needing
/// permission is put to `ui` as an [`ApprovalRequest`], so the same loop
/// drives the CLI, a GUI, or a test harness unchanged.
pub async fn run_agent_turn(
    client: &Client,
    ui: &mut impl AgentUi,
    messages: &mut Vec<ChatMessage>,
    model: &str,
    max_iterations: usize,
    approval: &ApprovalSettings,
    effort_level: Option<String>,
) -> Result<Option<String>> {
    let tool_definitions = get_tool_definitions();
    let mut iteration = 0;
    let mut final_response = None;

    while iteration < max_iterations {
        iteration += 1;

        ui.event(AgentEvent::IterationStarted { iteration }).await;

        // Call the LLM with tool definitions
        ui.event(AgentEvent::RequestStarted).await;
        let response = client
            .chat(
                model.to_string(),
                messages.clone(),
                0.7,
                Some(tool_definitions.clone()),
                effort_level.clone(),
            )
            .await;
        ui.event(AgentEvent::RequestFinished).await;
        let response = response?;

        let choice = &response.choices[0];

        let no_tool_calls = choice.message.tool_calls.is_none()
            || choice.message.tool_calls.as_ref().unwrap().is_empty();

        // If the LLM generated text, show it
        if choice.message.has_visible_content() {
            let content = choice.message.content.as_deref().unwrap();
            ui.event(AgentEvent::AssistantMessage {
                model: model.to_string(),
                effort_level: effort_level.clone(),
                text: content.to_string(),
            })
            .await;
            final_response = Some(content.to_string());
        }

        // Record the assistant's turn in history before deciding whether to
        // keep looping, so a plain text answer (no tool calls) is still
        // remembered on the next turn instead of vanishing from context.
        messages.push(choice.message.clone());

        if no_tool_calls {
            ui.event(AgentEvent::TurnFinished).await;
            return Ok(final_response);
        }

        // Process each tool call
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tool_call in tool_calls {
                let tool_name = &tool_call.function.name;

                ui.event(AgentEvent::ToolCallStarted {
                    name: tool_name.clone(),
                    arguments: tool_call.function.arguments.clone(),
                })
                .await;

                // Check if approval is needed
                let approved = if requires_approval(tool_name, approval) {
                    ui.approve(ApprovalRequest {
                        tool_name: tool_name.clone(),
                        category: get_tool_category(tool_name),
                        arguments: tool_call.function.arguments.clone(),
                    })
                    .await?
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
                    ui.event(AgentEvent::ToolCallDenied {
                        name: tool_name.clone(),
                    })
                    .await;
                    json!({ "error": "User denied permission for this action" })
                };

                ui.event(AgentEvent::ToolCallCompleted {
                    name: tool_name.clone(),
                    result: result.to_string(),
                })
                .await;

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
