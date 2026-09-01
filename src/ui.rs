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
/// "openrouter/auto (high)", or just the model name when no effort is set.
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

/// [`json_fields`] for a tool call's arguments, plus a `working_dir` entry
/// for a `run_terminal_command` call that didn't specify one — that means
/// it runs in the current directory, which otherwise wouldn't show up in
/// the notice at all.
pub fn tool_call_fields(name: &str, arguments: &str) -> Vec<(String, String)> {
    let mut fields = json_fields(arguments);
    if name == "run_terminal_command" && !fields.iter().any(|(key, _)| key == "working_dir") {
        if let Ok(cwd) = std::env::current_dir() {
            fields.push(("working_dir".to_string(), cwd.display().to_string()));
        }
    }
    fields
}

/// Interprets a typed answer to an approval prompt. Anything other than an
/// explicit yes denies the action — a blank answer included, matching a
/// conventional `[y/N]:` prompt's default. Shared so the CLI's stdin prompt
/// and the TUI's input-box prompt agree on what counts as "yes".
pub fn parse_yes_no(input: &str) -> bool {
    let response = input.trim().to_lowercase();
    response == "y" || response == "yes"
}

/// What a submitted line turned out to be. Shared between the TUI's input
/// box and the CLI's `session` loop, so `/model`, `/agent`, etc. behave
/// identically in both.
///
/// Only recognized commands are intercepted — anything else is a message,
/// including text that merely starts with a slash, since paths like
/// `/etc/hosts` are common enough in a coding tool that swallowing them
/// would be worse than letting a mistyped command reach the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Submission {
    Message(String),
    SetModel(String),
    ShowModel,
    SetAgentic(bool),
    /// `None` nullifies the override (`/effort clear`) — no effort field is
    /// sent, regardless of the configured default, until set again.
    SetEffort(Option<String>),
    /// `/effort default` — reads the *currently* configured default and
    /// saves that concrete value to the session now, distinct from
    /// [`Submission::SetEffort`]`(None)`, which nullifies instead.
    ResetEffort,
    ToggleVerbose,
    /// `None` nullifies the override (`/max-iterations clear`) — turns fall
    /// back to whatever the configured default is at the time each one
    /// runs, regardless of what it was when this session started.
    SetMaxIterations(Option<usize>),
    /// `/max-iterations default` — reads the *currently* configured default
    /// and saves that concrete value to the session now, distinct from
    /// [`Submission::SetMaxIterations`]`(None)`, which nullifies instead.
    ResetMaxIterations,
    /// `None` nullifies the override (`/temperature clear`), same deal as
    /// [`Submission::SetMaxIterations`].
    SetTemperature(Option<f32>),
    /// `/temperature default` — same deal as [`Submission::ResetMaxIterations`].
    ResetTemperature,
    /// `category` is one of "read", "write", "terminal", "all" — already
    /// validated by [`classify`], so a consumer can match on it directly.
    SetApproval {
        category: String,
        enabled: bool,
    },
    /// Prints/shows this session's current tool-approval gates without
    /// changing them.
    ShowApproval,
}

pub fn classify(text: &str) -> Submission {
    let trimmed = text.trim();

    match trimmed.strip_prefix("/model") {
        // "/models-are-great" is a message, not a malformed command.
        Some(rest) if rest.is_empty() => return Submission::ShowModel,
        Some(rest) if rest.starts_with(char::is_whitespace) => {
            let name = rest.trim();
            return if name.is_empty() {
                Submission::ShowModel
            } else {
                Submission::SetModel(name.to_string())
            };
        }
        _ => {}
    }

    if bare_command(trimmed, "/agent") {
        return Submission::SetAgentic(true);
    }
    if bare_command(trimmed, "/ask") {
        return Submission::SetAgentic(false);
    }
    if bare_command(trimmed, "/verbose") {
        return Submission::ToggleVerbose;
    }

    if let Some(value) = argument(trimmed, "/effort") {
        // "clear" nullifies — no effort field is sent at all, regardless of
        // the configured default, until set again. "default" is a distinct
        // action: it reads whatever the default currently is and saves that
        // concrete value to the session now. Anything else is passed
        // through as typed rather than checked against a fixed
        // low/medium/high allowlist — models vary in what they actually
        // accept, and this is a live per-session override, easy to correct
        // if wrong, not worth gatekeeping the way the persistent global
        // default is.
        if value.eq_ignore_ascii_case("clear") {
            return Submission::SetEffort(None);
        }
        if value.eq_ignore_ascii_case("default") {
            return Submission::ResetEffort;
        }
        return Submission::SetEffort(Some(value.to_string()));
    }

    if let Some(value) = argument(trimmed, "/max-iterations") {
        if value.eq_ignore_ascii_case("clear") {
            return Submission::SetMaxIterations(None);
        }
        if value.eq_ignore_ascii_case("default") {
            return Submission::ResetMaxIterations;
        }
        // A value that isn't recognized above and isn't a positive number
        // falls through below, same as any other malformed command — no
        // distinct error variant, matching how the rest of `classify`
        // degrades.
        if let Ok(n) = value.parse::<usize>() {
            if n > 0 {
                return Submission::SetMaxIterations(Some(n));
            }
        }
    }

    // "/temp" is accepted as a shorthand for "/temperature".
    if let Some(value) = argument(trimmed, "/temperature").or_else(|| argument(trimmed, "/temp")) {
        if value.eq_ignore_ascii_case("clear") {
            return Submission::SetTemperature(None);
        }
        if value.eq_ignore_ascii_case("default") {
            return Submission::ResetTemperature;
        }
        // A value that isn't recognized above and isn't a non-negative
        // number falls through below, same as any other malformed command —
        // no distinct error variant, matching how the rest of `classify`
        // degrades.
        if let Ok(n) = value.parse::<f32>() {
            if n >= 0.0 && n.is_finite() {
                return Submission::SetTemperature(Some(n));
            }
        }
    }

    if let Some(rest) = trimmed.strip_prefix("/approval") {
        if rest.trim().is_empty() {
            return Submission::ShowApproval;
        }
    }

    if let Some(rest) = argument(trimmed, "/approval") {
        let mut words = rest.split_whitespace();
        if let (Some(category), Some(value), None) = (words.next(), words.next(), words.next()) {
            if matches!(category, "read" | "write" | "terminal" | "all") {
                if let Ok(enabled) = parse_bool(value) {
                    return Submission::SetApproval {
                        category: category.to_string(),
                        enabled,
                    };
                }
            }
        }
    }

    Submission::Message(text.to_string())
}

/// "clear" (case-insensitive) resets an override; anything else is the new
/// value to set.
/// Accepts the same words the CLI's own `comms approval`/`comms stream`
/// flags do, so `/approval` in a session reads the same way.
pub fn parse_bool(s: &str) -> Result<bool, String> {
    match s.to_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => Err(format!(
            "Invalid boolean value: '{}'. Use true/false, on/off, yes/no, or 1/0",
            s
        )),
    }
}

/// The trimmed text after `/name `, when `trimmed` is `/name` followed by
/// whitespace and something non-empty. `None` for a bare `/name` (nothing
/// sensible to do without a value) or for text that isn't this command at
/// all, so — like `/model` — it falls through to an ordinary message
/// rather than silently doing nothing.
fn argument<'a>(trimmed: &'a str, name: &str) -> Option<&'a str> {
    let rest = trimmed.strip_prefix(name)?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let value = rest.trim();
    (!value.is_empty()).then_some(value)
}

/// Whether `trimmed` is exactly `/name`, or `/name` followed by whitespace
/// (any trailing text is ignored — neither command takes an argument).
/// Requiring that separator, like `/model` does, is what keeps
/// `/agentic-issue` a message rather than a malformed command.
fn bare_command(trimmed: &str, name: &str) -> bool {
    match trimmed.strip_prefix(name) {
        Some(rest) => rest.is_empty() || rest.starts_with(char::is_whitespace),
        None => false,
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
    /// A fragment of the reply, as it streams in. Only emitted when
    /// streaming is on. The deltas of a turn concatenate to exactly the
    /// `AssistantMessage` that follows them, so a front end renders one or
    /// the other — never both.
    AssistantDelta { text: String },
    /// The model's own thinking for this turn, when it returned any.
    /// Emitted before the reply (and before any tool call) it led to, so a
    /// front end can show the reasoning in the order it happened. Front
    /// ends gate this behind `/verbose`: it's the same class of detail as a
    /// tool call's arguments.
    Thinking { text: String },
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
    fn classify_recognizes_the_model_command() {
        assert_eq!(
            classify("/model anthropic/claude-opus-4.5"),
            Submission::SetModel("anthropic/claude-opus-4.5".to_string())
        );
        assert_eq!(classify("/model"), Submission::ShowModel);
        assert_eq!(classify("  /model   "), Submission::ShowModel);
    }

    #[test]
    fn classify_leaves_ordinary_text_and_paths_alone() {
        assert_eq!(
            classify("what does /etc/hosts do?"),
            Submission::Message("what does /etc/hosts do?".to_string())
        );
        // A leading slash that isn't a command must still reach the model,
        // since paths are common input in a coding tool.
        assert_eq!(
            classify("/usr/bin/env"),
            Submission::Message("/usr/bin/env".to_string())
        );
        // Not the command, just a word starting with it.
        assert_eq!(
            classify("/modelling is fun"),
            Submission::Message("/modelling is fun".to_string())
        );
    }

    #[test]
    fn classify_recognizes_agent_and_ask() {
        assert_eq!(classify("/agent"), Submission::SetAgentic(true));
        assert_eq!(classify("  /agent  "), Submission::SetAgentic(true));
        // Trailing text is ignored rather than rejected — neither command
        // takes an argument.
        assert_eq!(classify("/agent please"), Submission::SetAgentic(true));
        assert_eq!(classify("/ask"), Submission::SetAgentic(false));
        assert_eq!(classify("/ask nicely"), Submission::SetAgentic(false));

        // Not the command, just a word starting with it.
        assert_eq!(
            classify("/agentic-issue"),
            Submission::Message("/agentic-issue".to_string())
        );
        assert_eq!(
            classify("/asking for a friend"),
            Submission::Message("/asking for a friend".to_string())
        );
    }

    #[test]
    fn classify_recognizes_verbose_toggle() {
        assert_eq!(classify("/verbose"), Submission::ToggleVerbose);
        assert_eq!(classify("  /verbose  "), Submission::ToggleVerbose);
        assert_eq!(
            classify("/verbosely"),
            Submission::Message("/verbosely".to_string())
        );
    }

    #[test]
    fn classify_recognizes_effort() {
        assert_eq!(
            classify("/effort high"),
            Submission::SetEffort(Some("high".to_string()))
        );
        // "clear" nullifies — no effort field is sent until set again.
        assert_eq!(classify("/effort clear"), Submission::SetEffort(None));
        // "default" is a distinct action: it reads whatever the configured
        // default currently is and saves that concrete value now.
        assert_eq!(classify("/effort default"), Submission::ResetEffort);
        // Case-insensitive, like a keyword rather than a literal value.
        assert_eq!(classify("/effort CLEAR"), Submission::SetEffort(None));
        assert_eq!(classify("/effort DEFAULT"), Submission::ResetEffort);

        // Anything else passes through as typed, case included — not
        // checked against a fixed low/medium/high list, since models vary
        // in what reasoning-effort values they actually accept.
        assert_eq!(
            classify("/effort HIGH"),
            Submission::SetEffort(Some("HIGH".to_string()))
        );
        assert_eq!(
            classify("/effort minimal"),
            Submission::SetEffort(Some("minimal".to_string()))
        );

        // Bare, with nothing to act on, falls through as an ordinary
        // message rather than doing nothing silently.
        assert_eq!(
            classify("/effort"),
            Submission::Message("/effort".to_string())
        );
        // Not the command, just a word starting with it.
        assert_eq!(
            classify("/effortless"),
            Submission::Message("/effortless".to_string())
        );
    }

    #[test]
    fn classify_recognizes_max_iterations() {
        assert_eq!(
            classify("/max-iterations 30"),
            Submission::SetMaxIterations(Some(30))
        );
        // "clear" nullifies — turns fall back to the configured default.
        assert_eq!(
            classify("/max-iterations clear"),
            Submission::SetMaxIterations(None)
        );
        // "default" is a distinct action: it reads whatever the configured
        // default currently is and saves that concrete value now.
        assert_eq!(
            classify("/max-iterations default"),
            Submission::ResetMaxIterations
        );
        // Case-insensitive, like a keyword rather than a literal value.
        assert_eq!(
            classify("/max-iterations CLEAR"),
            Submission::SetMaxIterations(None)
        );
        assert_eq!(
            classify("/max-iterations DEFAULT"),
            Submission::ResetMaxIterations
        );

        // Zero and non-numeric values aren't valid iteration counts, so —
        // like a genuinely malformed command — they fall through as an
        // ordinary message rather than silently doing nothing.
        assert_eq!(
            classify("/max-iterations 0"),
            Submission::Message("/max-iterations 0".to_string())
        );
        assert_eq!(
            classify("/max-iterations banana"),
            Submission::Message("/max-iterations banana".to_string())
        );
        // Bare, with nothing to act on, falls through too.
        assert_eq!(
            classify("/max-iterations"),
            Submission::Message("/max-iterations".to_string())
        );
    }

    #[test]
    fn classify_recognizes_temperature() {
        assert_eq!(
            classify("/temperature 1.5"),
            Submission::SetTemperature(Some(1.5))
        );
        // Zero is a valid (deterministic) temperature, unlike max-iterations.
        assert_eq!(
            classify("/temperature 0"),
            Submission::SetTemperature(Some(0.0))
        );
        // "clear" nullifies — turns fall back to the configured default.
        assert_eq!(
            classify("/temperature clear"),
            Submission::SetTemperature(None)
        );
        // "default" is a distinct action: it reads whatever the configured
        // default currently is and saves that concrete value now.
        assert_eq!(
            classify("/temperature default"),
            Submission::ResetTemperature
        );
        // Case-insensitive, like a keyword rather than a literal value.
        assert_eq!(
            classify("/temperature CLEAR"),
            Submission::SetTemperature(None)
        );
        assert_eq!(
            classify("/temperature DEFAULT"),
            Submission::ResetTemperature
        );

        // Negative and non-numeric values aren't valid temperatures, so —
        // like a genuinely malformed command — they fall through as an
        // ordinary message rather than silently doing nothing.
        assert_eq!(
            classify("/temperature -1"),
            Submission::Message("/temperature -1".to_string())
        );
        assert_eq!(
            classify("/temperature banana"),
            Submission::Message("/temperature banana".to_string())
        );
        // Bare, with nothing to act on, falls through too.
        assert_eq!(
            classify("/temperature"),
            Submission::Message("/temperature".to_string())
        );
    }

    #[test]
    fn classify_recognizes_the_temp_shorthand() {
        assert_eq!(classify("/temp 1.5"), Submission::SetTemperature(Some(1.5)));
        assert_eq!(classify("/temp clear"), Submission::SetTemperature(None));
        // Same fall-through rules as the full name.
        assert_eq!(
            classify("/temp banana"),
            Submission::Message("/temp banana".to_string())
        );
        assert_eq!(classify("/temp"), Submission::Message("/temp".to_string()));
    }

    #[test]
    fn classify_recognizes_approval() {
        assert_eq!(
            classify("/approval read off"),
            Submission::SetApproval {
                category: "read".to_string(),
                enabled: false
            }
        );
        assert_eq!(
            classify("/approval write on"),
            Submission::SetApproval {
                category: "write".to_string(),
                enabled: true
            }
        );
        assert_eq!(
            classify("/approval terminal yes"),
            Submission::SetApproval {
                category: "terminal".to_string(),
                enabled: true
            }
        );
        assert_eq!(
            classify("/approval all off"),
            Submission::SetApproval {
                category: "all".to_string(),
                enabled: false
            }
        );

        // Not a recognized category, not a recognized boolean, or too many
        // words all fall through as an ordinary message.
        assert_eq!(
            classify("/approval bogus off"),
            Submission::Message("/approval bogus off".to_string())
        );
        assert_eq!(
            classify("/approval read maybe"),
            Submission::Message("/approval read maybe".to_string())
        );
        assert_eq!(
            classify("/approval read"),
            Submission::Message("/approval read".to_string())
        );
        assert_eq!(
            classify("/approval read off now"),
            Submission::Message("/approval read off now".to_string())
        );

        // Bare — with or without trailing whitespace — shows the current
        // settings instead of falling through, matching `/model`.
        assert_eq!(classify("/approval"), Submission::ShowApproval);
        assert_eq!(classify("  /approval   "), Submission::ShowApproval);
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

    #[test]
    fn tool_call_fields_adds_the_default_working_dir_for_a_terminal_command() {
        let fields = tool_call_fields("run_terminal_command", r#"{"command":"cargo test"}"#);
        let working_dir = fields.iter().find(|(key, _)| key == "working_dir");
        assert!(working_dir.is_some(), "{fields:?}");
        assert_eq!(
            working_dir.unwrap().1,
            std::env::current_dir().unwrap().display().to_string()
        );
    }

    #[test]
    fn tool_call_fields_keeps_an_explicit_working_dir_as_is() {
        let fields = tool_call_fields(
            "run_terminal_command",
            r#"{"command":"ls","working_dir":"/tmp"}"#,
        );
        assert_eq!(
            fields,
            vec![
                ("command".to_string(), "ls".to_string()),
                ("working_dir".to_string(), "/tmp".to_string()),
            ]
        );
    }

    #[test]
    fn tool_call_fields_leaves_other_tools_unchanged() {
        let fields = tool_call_fields("write_file", r#"{"filepath":"a.rs","content":"x"}"#);
        assert!(
            !fields.iter().any(|(key, _)| key == "working_dir"),
            "{fields:?}"
        );
    }
}
