//! The boundary between what the app *does* and how that is presented.
//!
//! The agent loop in [`crate::agent`] is pure orchestration: it decides what
//! to call and in what order, and reports progress by emitting
//! [`AgentEvent`]s to an [`AgentUi`] rather than printing. Anything that
//! needs a decision from the user goes through [`AgentUi::approve`].
//!
//! That keeps a second front end (a GUI, a web server, a test harness) from
//! having to fork the loop just to render it differently: it implements this
//! trait instead. [`crate::terminal_ui`] is the CLI's implementation.

use anyhow::Result;

/// Formats a model name with its effort level for display, e.g.
/// "orcarouter/auto (high)", or just the model name when no effort is set.
pub fn response_label(model: &str, effort_level: &Option<String>) -> String {
    match effort_level {
        Some(effort) => format!("{} ({})", model, effort),
        None => model.to_string(),
    }
}

/// Something the agent loop wants to report as it runs. A front end decides
/// what (if any) of this to surface — the CLI, for instance, shows most of
/// it only in verbose mode.
///
/// Every event carries enough to render it standalone even where the CLI
/// happens not to use all of it: a front end that lists tool calls as they
/// resolve needs the `name` on a denial or a result to match it back to the
/// call it belongs to, which a purely sequential transcript doesn't.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum AgentEvent {
    /// A new pass through the tool-calling loop has begun. 1-based.
    IterationStarted { iteration: usize },
    /// A request to the model is in flight; nothing will happen until it
    /// resolves. Paired with exactly one `RequestFinished`.
    RequestStarted,
    /// The in-flight request resolved, successfully or not.
    RequestFinished,
    /// The model produced visible text for the user.
    AssistantMessage {
        model: String,
        effort_level: Option<String>,
        text: String,
    },
    /// The model asked to run a tool. Emitted before any approval prompt.
    ToolCallStarted { name: String, arguments: String },
    /// The user declined to let a tool run.
    ToolCallDenied { name: String },
    /// A tool ran (or failed); `result` is the JSON handed back to the model.
    ToolCallCompleted { name: String, result: String },
    /// The model answered without requesting tools, so the turn is over.
    TurnFinished,
}

/// A tool call waiting on a yes/no decision before it runs.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool_name: String,
    /// Which [`crate::agent::ToolCategory`]-style bucket the tool falls in
    /// ("read", "write", "terminal", or "unknown"), for front ends that want
    /// to describe the action rather than just name the tool.
    pub category: &'static str,
    /// The tool's arguments, as the raw JSON string the model produced.
    pub arguments: String,
}

/// How the agent loop talks to whoever is driving it.
///
/// Both methods are `async` so an implementation can await real work — a GUI
/// answering `approve` from a channel once someone clicks a button, say —
/// rather than blocking the executor. The `async_fn_in_trait` lint is about
/// callers being unable to add `Send` bounds to the returned futures; this is
/// an application trait with known implementors, not a published API, so
/// there's nothing to constrain.
#[allow(async_fn_in_trait)]
pub trait AgentUi {
    /// Report progress. Implementations should not block for long here.
    async fn event(&mut self, event: AgentEvent);

    /// Ask whether a tool may run. Returning `Ok(false)` denies it and lets
    /// the loop continue; returning `Err` aborts the turn.
    async fn approve(&mut self, request: ApprovalRequest) -> Result<bool>;
}

/// An [`AgentUi`] that shows nothing and denies every approval request.
/// Useful for tests and for any caller that wants the loop to run without a
/// user attached.
pub struct SilentUi;

impl AgentUi for SilentUi {
    async fn event(&mut self, _event: AgentEvent) {}

    async fn approve(&mut self, _request: ApprovalRequest) -> Result<bool> {
        Ok(false)
    }
}
