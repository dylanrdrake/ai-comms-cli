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
use std::future::Future;

/// Formats a model name with its effort level for display, e.g.
/// "orcarouter/auto (high)", or just the model name when no effort is set.
pub fn response_label(model: &str, effort_level: &Option<String>) -> String {
    match effort_level {
        Some(effort) => format!("{} ({})", model, effort),
        None => model.to_string(),
    }
}

/// The one argument that best identifies what a tool call is doing — the
/// path for a file tool, the command for `run_terminal_command` — so a
/// terse, non-verbose notice can name it without dumping the full argument
/// JSON. `None` if `arguments` isn't a JSON object or has none of these.
pub fn primary_argument(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let object = value.as_object()?;
    ["filepath", "command", "dirpath"]
        .iter()
        .find_map(|key| object.get(*key).and_then(|v| v.as_str()))
        .map(str::to_string)
}

/// Flattens a possibly long/multi-line value onto one line, truncated to
/// `max` characters, for a compact preview — shared by both front ends so
/// neither drifts from the other's idea of "too long to show in full".
pub fn summarize(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    if flat.chars().count() > max {
        let kept: String = flat.chars().take(max).collect();
        format!("{kept}…")
    } else {
        flat.to_string()
    }
}

/// Splits a tool call's JSON arguments/result into `(field, value)` pairs,
/// each value flattened and truncated for a single display line — the
/// per-field detail both the approval prompt and verbose tool-call notices
/// show. Empty if `text` isn't a JSON object.
pub fn json_fields(text: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    object
        .iter()
        .map(|(key, value)| {
            let shown = value
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| value.to_string());
            (key.clone(), summarize(&shown, 100))
        })
        .collect()
}

/// Interprets a typed answer to an approval prompt. Anything other than an
/// explicit yes denies the action — a blank answer included, matching a
/// conventional `[y/N]:` prompt's default. Shared so the CLI's stdin prompt
/// and the TUI's input-box prompt agree on what counts as "yes".
pub fn parse_yes_no(input: &str) -> bool {
    let response = input.trim().to_lowercase();
    response == "y" || response == "yes"
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
    /// A fragment of the reply, as it streams in. Only emitted when
    /// streaming is on. The deltas of a turn concatenate to exactly the
    /// `AssistantMessage` that follows them, so a front end renders one or
    /// the other — never both.
    AssistantDelta { text: String },
    /// The model produced visible text for the user. Always emitted at the
    /// end of a turn, streaming or not, with the complete text.
    AssistantMessage {
        model: String,
        effort_level: Option<String>,
        text: String,
    },
    /// Something went wrong that the user should see but that doesn't end
    /// the session — a failed request, a message that couldn't be saved.
    Error { message: String },
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
/// Both methods return futures so an implementation can await real work — a
/// GUI answering `approve` from a channel once someone clicks a button, say —
/// rather than blocking the executor.
///
/// They're written as explicit `-> impl Future + Send` rather than `async fn`
/// because an `async fn` in a trait gives its future no `Send` bound, which
/// makes the whole agent loop un-spawnable from any generic context. The TUI
/// runs the loop on a background task, so `Send` is required.
pub trait AgentUi {
    /// Report progress. Implementations should not block for long here.
    fn event(&mut self, event: AgentEvent) -> impl Future<Output = ()> + Send;

    /// Ask whether a tool may run. Returning `Ok(false)` denies it and lets
    /// the loop continue; returning `Err` aborts the turn.
    fn approve(&mut self, request: ApprovalRequest) -> impl Future<Output = Result<bool>> + Send;
}

/// An [`AgentUi`] that shows nothing and denies every approval request.
/// Useful for tests and for any caller that wants the loop to run without a
/// user attached.
pub struct SilentUi;

impl AgentUi for SilentUi {
    fn event(&mut self, _event: AgentEvent) -> impl Future<Output = ()> + Send {
        async {}
    }

    fn approve(&mut self, _request: ApprovalRequest) -> impl Future<Output = Result<bool>> + Send {
        async { Ok(false) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_yes_no_accepts_only_explicit_yes() {
        assert!(parse_yes_no("y"));
        assert!(parse_yes_no("yes"));
        assert!(parse_yes_no("  YES  \n"));
        assert!(parse_yes_no("Y\n"));
    }

    #[test]
    fn parse_yes_no_denies_everything_else() {
        assert!(!parse_yes_no("n"));
        assert!(!parse_yes_no("no"));
        assert!(!parse_yes_no(""));
        assert!(!parse_yes_no("\n"));
        assert!(!parse_yes_no("maybe"));
        // Fails closed: a stray answer is a denial, never an approval.
        assert!(!parse_yes_no("yep"));
    }

    #[test]
    fn primary_argument_finds_a_file_path() {
        assert_eq!(
            primary_argument(r#"{"filepath":"src/main.rs","content":"x"}"#),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn primary_argument_finds_a_terminal_command() {
        assert_eq!(
            primary_argument(r#"{"command":"cargo test","timeout_secs":30}"#),
            Some("cargo test".to_string())
        );
    }

    #[test]
    fn primary_argument_finds_a_directory() {
        assert_eq!(
            primary_argument(r#"{"dirpath":"src"}"#),
            Some("src".to_string())
        );
    }

    #[test]
    fn primary_argument_is_none_without_a_recognized_key() {
        assert_eq!(
            primary_argument(r#"{"search":"foo","replace":"bar"}"#),
            None
        );
        assert_eq!(primary_argument("not json"), None);
        assert_eq!(primary_argument("{}"), None);
    }
}
