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

use crate::config::ApprovalSettings;
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
/// Recognized commands are intercepted, and so is a line that names one of
/// them but doesn't invoke it validly — see [`Submission::UnknownCommand`].
/// That's confidently a failed command, not text meant for the model, so it
/// is never sent either. Anything else is an ordinary message, including
/// text that merely starts with a slash but doesn't name a known command at
/// all, since paths like `/etc/hosts` are common enough in a coding tool
/// that swallowing them would be worse than never catching a genuine typo.
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
    /// Confines the agent's file writes to the working directory and home,
    /// or lets them go anywhere. The read tools are unaffected either way.
    SetSandbox(bool),
    /// Prints/shows whether writes are currently confined, without changing
    /// it.
    ShowSandbox,
    /// Prints/shows every setting this session is running with, without
    /// changing any of them. Named for `comms status`, which does the same
    /// job one scope out: global configuration there, this session here.
    ShowStatus,
    /// A line that named a known command (`/effort`, `/max-iterations`,
    /// `/temperature`/`/temp`, `/approval`, `/sandbox`) but wasn't a valid
    /// invocation of
    /// it — a missing/invalid argument, an unrecognized approval category,
    /// too many or too few words, and so on. Unlike a `/<word>` that isn't a
    /// recognized command at all (kept as an ordinary [`Submission::Message`],
    /// since paths like `/etc/hosts` are common in a coding tool), this is
    /// confidently a failed command: never sent to the model, and reported
    /// by the front end as an error instead. Carries a usage hint for
    /// whichever command was misused.
    UnknownCommand(String),
}

/// Everything `/status` reports, gathered from whichever front end is
/// asking. Both hold the same state — the TUI in its `App`, the CLI in its
/// `ChatSession` — so the shape lives here and the rendering is shared,
/// rather than each growing its own list that drifts from the other.
pub struct SessionSettings<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub model: &'a str,
    pub agentic: bool,
    pub effort_level: Option<&'a str>,
    pub temperature: Option<f32>,
    pub max_iterations: Option<usize>,
    pub verbose: bool,
    pub sandbox: bool,
    pub approval: &'a ApprovalSettings,
}

/// `/status` as label/value rows, ready for either front end to draw.
///
/// A setting that isn't set says what that *means* rather than showing an
/// empty cell — a nullified temperature sends no field at all, which is a
/// different thing from one that happens to equal the default.
pub fn session_settings_rows(settings: &SessionSettings) -> Vec<(String, String)> {
    let on_off = |value: bool| if value { "on" } else { "off" }.to_string();
    let gate = |enabled: bool| if enabled { "Ask" } else { "Auto" };

    vec![
        ("ID".to_string(), settings.id.to_string()),
        ("Title".to_string(), settings.title.to_string()),
        (
            "Mode".to_string(),
            if settings.agentic {
                "agent — tools enabled".to_string()
            } else {
                "ask — no tools".to_string()
            },
        ),
        ("Model".to_string(), settings.model.to_string()),
        (
            "Effort".to_string(),
            settings
                .effort_level
                .map(str::to_string)
                .unwrap_or_else(|| "none sent".to_string()),
        ),
        (
            "Temperature".to_string(),
            settings
                .temperature
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none sent".to_string()),
        ),
        (
            "Max iterations".to_string(),
            settings
                .max_iterations
                .map(|value| value.to_string())
                .unwrap_or_else(|| "not set".to_string()),
        ),
        ("Sandbox".to_string(), on_off(settings.sandbox)),
        ("Verbose".to_string(), on_off(settings.verbose)),
        (
            "Approval".to_string(),
            format!(
                "read {} · write {} · terminal {}",
                gate(settings.approval.read_disk),
                gate(settings.approval.write_disk),
                gate(settings.approval.terminal),
            ),
        ),
    ]
}

/// How the sandbox setting reads back to the user. `changed` picks "set to"
/// over "is", the same distinction every other setting's notice makes.
pub fn sandbox_notice(sandbox: bool, changed: bool) -> String {
    let verb = if changed { "set to" } else { "is" };
    let state = if sandbox {
        "on — writes confined to the working directory and home"
    } else {
        "off — writes allowed anywhere"
    };
    format!("Sandbox {verb} {state}")
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

    if let Some(rest) = trimmed.strip_prefix("/sandbox") {
        if rest.trim().is_empty() {
            return Submission::ShowSandbox;
        }
    }

    if let Some(rest) = trimmed.strip_prefix("/status") {
        if rest.trim().is_empty() {
            return Submission::ShowStatus;
        }
    }

    if let Some(value) = argument(trimmed, "/sandbox") {
        if let Ok(enabled) = parse_bool(value) {
            return Submission::SetSandbox(enabled);
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

    // A line naming a known command's word is confidently an attempted
    // command even when nothing above could parse it — unlike a `/<word>`
    // that isn't one of these at all (a path like `/etc/hosts`, say), which
    // stays an ordinary message. See `command_usage`.
    if let Some(word) = command_word(trimmed) {
        if let Some(usage) = command_usage(word) {
            return Submission::UnknownCommand(format!(
                "Unrecognized /{word} usage. Usage: {usage}"
            ));
        }
        // ...and so is one that merely comes close to naming it. A typo is
        // the case this whole check exists for: `/mode anthropic/...` used
        // to reach the model as text, which reads as the model ignoring a
        // command rather than as a mistake the user can see and fix.
        if let Some(nearest) = nearest_command(word) {
            return Submission::UnknownCommand(match command_usage(nearest) {
                Some(usage) => {
                    format!("Unrecognized command /{word}. Did you mean /{nearest}? Usage: {usage}")
                }
                None => format!("Unrecognized command /{word}. Did you mean /{nearest}?"),
            });
        }
    }

    Submission::Message(text.to_string())
}

/// The leading `/word` of `trimmed`, when it starts with a slash at all —
/// `"model"` for both `"/model"` and `"/model anthropic/opus"`. Stops only
/// at whitespace, not at a second slash, so a path like `/etc/hosts` yields
/// `"etc/hosts"` rather than `"etc"` — which is what keeps it from ever
/// matching a name in [`command_usage`].
fn command_word(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix('/')?;
    Some(rest.split_whitespace().next().unwrap_or(rest))
}

/// Every command word [`classify`] knows, for spotting a near miss. The four
/// that always parse are here too: `/mdoel` should still be caught as a
/// typo for `/model` even though `/model` itself can't be invoked wrongly.
const KNOWN_COMMANDS: [&str; 11] = [
    "model",
    "agent",
    "ask",
    "effort",
    "max-iterations",
    "temperature",
    "temp",
    "approval",
    "sandbox",
    "status",
    "verbose",
];

/// How far a word may stray from a command name and still be read as a
/// misspelling of it, given its length. Two edits covers the ordinary slips
/// — a dropped letter, a doubled one, a transposition (`/mdoel`,
/// `/aprpoval`) — but on a short word two edits is most of the word, which
/// is how `/usr` ends up two from `ask`. Short words get one edit only.
fn max_distance_for(word_length: usize) -> usize {
    if word_length >= 5 {
        2
    } else {
        1
    }
}

/// The command `word` looks like a misspelling of, if any.
///
/// Deliberately conservative, because a false positive costs more than a
/// miss — it swallows a message someone meant to send. Three things are
/// refused outright:
///
/// - anything holding a `/`: that's a path like `etc/hosts`, never a command;
/// - anything under three characters, too close to everything to tell apart;
/// - a word that *extends* the command it matched. `/verbosely` is `verbose`
///   plus a real suffix — a different word, not a misspelling — and words
///   like it have always been ordinary messages. This is checked against
///   the match itself rather than against every command, or `/temperatur`
///   would be thrown out for beginning with the unrelated `/temp`.
///
/// Within what's left it takes the single best match rather than the first
/// in range. The one knowingly-accepted overlap is a bare `/tmp`, one edit
/// from `/temp` — a path segment, but not one anybody types alone as a
/// whole message, and the error names exactly what it thought you meant.
fn nearest_command(word: &str) -> Option<&'static str> {
    let length = word.chars().count();
    if word.contains('/') || length < 3 {
        return None;
    }
    let max_distance = max_distance_for(length);
    let (_, nearest) = KNOWN_COMMANDS
        .iter()
        .map(|name| (edit_distance(word, name), *name))
        // 0 would mean an exact match, which every caller above has already
        // handled — suggesting a word to itself would be nonsense.
        .filter(|(distance, _)| (1..=max_distance).contains(distance))
        .min_by_key(|(distance, _)| *distance)?;

    (!(word.starts_with(nearest) && length > nearest.chars().count())).then_some(nearest)
}

/// Levenshtein distance, iterating over `char`s so a multi-byte character
/// counts as one edit rather than as its bytes. Only ever run over one
/// short word against nine short names, so the straightforward two-row
/// implementation is well within its keep.
fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0; b_chars.len() + 1];

    for (i, a_char) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, b_char) in b_chars.iter().enumerate() {
            let substitution = previous[j] + usize::from(a_char != *b_char);
            let deletion = previous[j + 1] + 1;
            let insertion = current[j] + 1;
            current[j + 1] = substitution.min(deletion).min(insertion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b_chars.len()]
}

/// The usage hint for a known command word, if invoking it wrong is even
/// possible. `/model`, `/agent`, `/ask`, and `/verbose` accept every
/// invocation they can be given (bare, or with any trailing text at all),
/// so they have nothing invalid to report and aren't listed here — by the
/// time [`classify`] reaches this check, any of those words would already
/// have returned its own [`Submission`] above.
///
/// The hint is spelled with the word it was asked about, not a canonical
/// one, so it always names the command the reader just typed or was just
/// pointed at: `/temp` is answered about `/temp`, never about
/// `/temperature`.
fn command_usage(word: &str) -> Option<String> {
    Some(match word {
        "effort" => format!("/{word} <level> | clear | default"),
        "max-iterations" => {
            format!("/{word} <n> | clear | default (n must be a positive integer)")
        }
        "temperature" | "temp" => {
            format!("/{word} <n> | clear | default (n must be 0 or greater)")
        }
        "approval" => format!("/{word} <read|write|terminal|all> <on|off>"),
        "sandbox" => format!("/{word} <on|off>"),
        // Takes no argument at all, so anything after it is a mistake.
        "status" => format!("/{word}"),
        _ => return None,
    })
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
/// all. The caller decides what `None` means from there — a bare `/model`
/// still shows the current model, while a bare `/effort`/`/max-iterations`/
/// `/temperature`/`/approval` falls through to [`command_usage`] and is
/// reported as a failed command rather than reaching the model as text.
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

        // Bare, with nothing to act on, is a failed command rather than
        // being sent to the model as text.
        assert_eq!(
            classify("/effort"),
            Submission::UnknownCommand(
                "Unrecognized /effort usage. Usage: /effort <level> | clear | default".to_string()
            )
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
        // like a bare invocation — they're reported as a failed command
        // rather than silently reaching the model as text.
        let max_iterations_usage = "Unrecognized /max-iterations usage. Usage: \
            /max-iterations <n> | clear | default (n must be a positive integer)";
        assert_eq!(
            classify("/max-iterations 0"),
            Submission::UnknownCommand(max_iterations_usage.to_string())
        );
        assert_eq!(
            classify("/max-iterations banana"),
            Submission::UnknownCommand(max_iterations_usage.to_string())
        );
        // Bare, with nothing to act on, is reported the same way.
        assert_eq!(
            classify("/max-iterations"),
            Submission::UnknownCommand(max_iterations_usage.to_string())
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
        // like a bare invocation — they're reported as a failed command
        // rather than silently reaching the model as text.
        let temperature_usage = "Unrecognized /temperature usage. Usage: \
            /temperature <n> | clear | default (n must be 0 or greater)";
        assert_eq!(
            classify("/temperature -1"),
            Submission::UnknownCommand(temperature_usage.to_string())
        );
        assert_eq!(
            classify("/temperature banana"),
            Submission::UnknownCommand(temperature_usage.to_string())
        );
        // Bare, with nothing to act on, is reported the same way.
        assert_eq!(
            classify("/temperature"),
            Submission::UnknownCommand(temperature_usage.to_string())
        );
    }

    #[test]
    fn classify_recognizes_the_temp_shorthand() {
        assert_eq!(classify("/temp 1.5"), Submission::SetTemperature(Some(1.5)));
        assert_eq!(classify("/temp clear"), Submission::SetTemperature(None));
        // Same rules as the full name: a malformed or bare invocation is a
        // failed command, not text sent to the model.
        // Answered about `/temp`, the word actually typed — not about
        // `/temperature`, which the reader didn't write.
        let temp_usage = "Unrecognized /temp usage. Usage: \
            /temp <n> | clear | default (n must be 0 or greater)";
        assert_eq!(
            classify("/temp banana"),
            Submission::UnknownCommand(temp_usage.to_string())
        );
        assert_eq!(
            classify("/temp"),
            Submission::UnknownCommand(temp_usage.to_string())
        );
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
        // (or too few) words are all a failed command, not text sent to
        // the model.
        let approval_usage = "Unrecognized /approval usage. Usage: \
            /approval <read|write|terminal|all> <on|off>";
        assert_eq!(
            classify("/approval bogus off"),
            Submission::UnknownCommand(approval_usage.to_string())
        );
        assert_eq!(
            classify("/approval read maybe"),
            Submission::UnknownCommand(approval_usage.to_string())
        );
        assert_eq!(
            classify("/approval read"),
            Submission::UnknownCommand(approval_usage.to_string())
        );
        assert_eq!(
            classify("/approval read off now"),
            Submission::UnknownCommand(approval_usage.to_string())
        );

        // Bare — with or without trailing whitespace — shows the current
        // settings instead of falling through, matching `/model`.
        assert_eq!(classify("/approval"), Submission::ShowApproval);
        assert_eq!(classify("  /approval   "), Submission::ShowApproval);
    }

    #[test]
    fn classify_recognizes_the_status_command() {
        assert_eq!(classify("/status"), Submission::ShowStatus);
        assert_eq!(classify("  /status   "), Submission::ShowStatus);
        // It takes no argument, so anything after it is a mistake rather
        // than a message.
        assert_eq!(
            classify("/status verbose"),
            Submission::UnknownCommand("Unrecognized /status usage. Usage: /status".to_string())
        );
        assert_eq!(nearest_command("statu"), Some("status"));
    }

    #[test]
    fn session_settings_rows_say_what_unset_means() {
        // A nullified setting isn't blank — it does something specific, and
        // the readout has to distinguish "sends nothing" from "happens to
        // match the default".
        let approval = ApprovalSettings::default();
        let rows = session_settings_rows(&SessionSettings {
            id: "abc123",
            title: "Untitled",
            model: "openrouter/auto",
            agentic: false,
            effort_level: None,
            temperature: None,
            max_iterations: None,
            verbose: false,
            sandbox: true,
            approval: &approval,
        });
        let value = |label: &str| {
            rows.iter()
                .find(|(l, _)| l == label)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("no {label} row"))
        };

        assert_eq!(value("ID"), "abc123");
        assert_eq!(value("Mode"), "ask — no tools");
        assert_eq!(value("Effort"), "none sent");
        assert_eq!(value("Temperature"), "none sent");
        assert_eq!(value("Max iterations"), "not set");
        assert_eq!(value("Sandbox"), "on");
        assert_eq!(value("Approval"), "read Ask · write Ask · terminal Ask");
    }

    #[test]
    fn session_settings_rows_report_a_configured_session() {
        let approval = ApprovalSettings {
            read_disk: false,
            write_disk: true,
            terminal: true,
        };
        let rows = session_settings_rows(&SessionSettings {
            id: "abc123",
            title: "Fix the parser",
            model: "anthropic/claude-sonnet-5",
            agentic: true,
            effort_level: Some("high"),
            temperature: Some(0.7),
            max_iterations: Some(20),
            verbose: true,
            sandbox: false,
            approval: &approval,
        });
        let value = |label: &str| {
            rows.iter()
                .find(|(l, _)| l == label)
                .map(|(_, v)| v.clone())
                .unwrap()
        };

        assert_eq!(value("Mode"), "agent — tools enabled");
        assert_eq!(value("Effort"), "high");
        assert_eq!(value("Sandbox"), "off");
        assert_eq!(value("Verbose"), "on");
        // An auto-approved category reads differently from a gated one.
        assert_eq!(value("Approval"), "read Auto · write Ask · terminal Ask");
    }

    #[test]
    fn classify_recognizes_the_sandbox_command() {
        assert_eq!(classify("/sandbox off"), Submission::SetSandbox(false));
        assert_eq!(classify("/sandbox on"), Submission::SetSandbox(true));
        // Same boolean words every other on/off setting takes.
        assert_eq!(classify("/sandbox false"), Submission::SetSandbox(false));
        assert_eq!(classify("/sandbox 1"), Submission::SetSandbox(true));
        // Bare shows the current setting, matching `/approval`.
        assert_eq!(classify("/sandbox"), Submission::ShowSandbox);
        assert_eq!(classify("  /sandbox   "), Submission::ShowSandbox);
        // A value that isn't a boolean is a failed command, not a message.
        assert_eq!(
            classify("/sandbox maybe"),
            Submission::UnknownCommand(
                "Unrecognized /sandbox usage. Usage: /sandbox <on|off>".to_string()
            )
        );
        // And it joins the near-miss set like every other command word.
        assert_eq!(nearest_command("sandbix"), Some("sandbox"));
    }

    #[test]
    fn sandbox_notice_says_what_changed_and_what_it_means() {
        assert_eq!(
            sandbox_notice(true, true),
            "Sandbox set to on — writes confined to the working directory and home"
        );
        assert_eq!(
            sandbox_notice(false, false),
            "Sandbox is off — writes allowed anywhere"
        );
    }

    #[test]
    fn classify_catches_a_misspelled_command() {
        // The case this exists for, taken from a real session: `/mode ...`
        // reached the model as text, which reads as the model ignoring a
        // command rather than as a typo the user can see and fix.
        assert_eq!(
            classify("/mode anthropic/claude-sonnet-5"),
            Submission::UnknownCommand(
                "Unrecognized command /mode. Did you mean /model?".to_string()
            )
        );
        // A transposition is two edits, still within reach, and a command
        // that *can* be misused carries its usage along with the guess.
        assert_eq!(
            classify("/aprpoval read off"),
            Submission::UnknownCommand(
                "Unrecognized command /aprpoval. Did you mean /approval? \
                 Usage: /approval <read|write|terminal|all> <on|off>"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_suggestion_spells_its_usage_the_way_it_was_suggested() {
        // `/tmp` is nearest to the `/temp` shorthand, so the hint has to
        // talk about `/temp` — naming `/temperature` here would answer
        // about a word the reader was never pointed at.
        assert_eq!(
            classify("/tmp"),
            Submission::UnknownCommand(
                "Unrecognized command /tmp. Did you mean /temp? \
                 Usage: /temp <n> | clear | default (n must be 0 or greater)"
                    .to_string()
            )
        );
    }

    #[test]
    fn classify_leaves_paths_and_unrelated_slash_words_alone() {
        // The regression the whole hybrid guards against: a leading slash
        // is not on its own enough to swallow a message.
        for text in [
            "/etc/hosts",
            "/etc/hosts, what's in it?",
            "/usr/bin/env python",
            "/home/dylan/code",
            "/x",
        ] {
            assert_eq!(
                classify(text),
                Submission::Message(text.to_string()),
                "{text} should stay a message"
            );
        }
    }

    #[test]
    fn nearest_command_refuses_to_guess_at_a_path() {
        // A `/` anywhere in the word means a path, however close its first
        // segment lands to a command name.
        assert_eq!(nearest_command("mode/dark"), None);
        // Too short to tell apart from anything.
        assert_eq!(nearest_command("md"), None);
        // Unrelated words stay unrelated. `usr` is two edits from `ask`,
        // which on a three-letter word is most of the word — hence the
        // length-relative bound.
        assert_eq!(nearest_command("etc"), None);
        assert_eq!(nearest_command("usr"), None);
        // A word that extends its own match is a different word, not a
        // misspelling — these have always been ordinary messages.
        assert_eq!(nearest_command("verbosely"), None);
        assert_eq!(nearest_command("models-are-great"), None);
        assert_eq!(nearest_command("tempo"), None);
        // But extending some *other*, unrelated command is no reason to
        // throw a real typo away: `temperatur` begins with `temp`.
        assert_eq!(nearest_command("temperatur"), Some("temperature"));
        // The knowingly-accepted overlap, called out so a change is loud.
        assert_eq!(nearest_command("tmp"), Some("temp"));
        // And the best match wins, not merely the first in range.
        assert_eq!(nearest_command("temperatur"), Some("temperature"));
    }

    #[test]
    fn edit_distance_counts_characters_not_bytes() {
        assert_eq!(edit_distance("model", "model"), 0);
        assert_eq!(edit_distance("mode", "model"), 1);
        assert_eq!(edit_distance("mdoel", "model"), 2);
        // A multi-byte character is one edit, not one per byte.
        assert_eq!(edit_distance("café", "cafe"), 1);
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
