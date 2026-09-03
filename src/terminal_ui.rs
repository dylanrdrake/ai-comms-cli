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

/// Writes what the session is doing while a turn runs, for anything watching
/// the list of sessions.
///
/// Holds its own database handle rather than borrowing the session: the turn
/// already has that borrowed mutably for the whole call, and an approval
/// prompt happens in the middle of it.
pub struct ActivityWriter {
    conn: rusqlite::Connection,
    session_id: String,
    /// Held, not used: it ticks on its own and gives up the claim when this
    /// writer is dropped. Without it every activity written below would go
    /// stale after its window and stop being believed.
    _heartbeat: Option<crate::session::Heartbeat>,
}

impl ActivityWriter {
    pub fn new(session_id: String) -> Result<Self> {
        Ok(ActivityWriter {
            conn: crate::store::open_db()?,
            _heartbeat: crate::session::Heartbeat::start(session_id.clone())?,
            session_id,
        })
    }

    fn set(&self, activity: Option<crate::store::Activity>, detail: Option<&str>) {
        let _ = crate::store::set_session_activity(&self.conn, &self.session_id, activity, detail);
    }
}

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
    /// Set for a `session`, which is watchable from the picker; `None` for a
    /// one-shot `agent`, which nobody is monitoring.
    activity: Option<ActivityWriter>,
    /// The call's arguments, held from `ToolCallStarted` to whichever event
    /// settles it, so the notice that settles it can still name the file
    /// or command being acted on, and so `ToolCallCompleted` can tell a
    /// denied call (already reported by `ToolCallDenied`) from one that
    /// actually ran. Tool calls run one at a time, so there's never more
    /// than one in flight to track.
    pending_arguments: Option<String>,
    /// Whether the current tool-call header line (`🔨 name  detail`) is
    /// still open — printed without a trailing newline, waiting for
    /// whichever event resolves it to close it with a trailing status
    /// marker on the same line, the CLI's equivalent of the TUI
    /// transcript's trailing ✓/✗/?. A verbose dump between the header and
    /// its resolution closes it early instead, since the marker can no
    /// longer land on that same (now scrolled-past) line.
    tool_header_open: bool,
    /// Whether the current call showed an approval prompt (closing the
    /// header with `?`). Its own typed `y`/`N` answer already says how it
    /// resolved, so — unlike a call that ran with no prompt at all —
    /// `ToolCallCompleted`/`ToolCallDenied` draw no further marker for it.
    approval_shown: bool,
    /// Whether this run has a terminal behind it. Set by `--headless`, which
    /// exists so a run can be fired off and left: no spinner (an animation
    /// redrawn with `\r` is noise in a log) and no approval prompt (there is
    /// no stdin to read an answer from, and blocking on one would hang the
    /// run forever with nobody watching).
    headless: bool,
}

impl TerminalAgentUi {
    /// Starts reporting what this session is doing while turns run, so a
    /// CLI session shows up in the picker the way a TUI one does.
    pub fn watch(&mut self, activity: ActivityWriter) {
        self.activity = Some(activity);
    }

    pub fn new(verbose: bool, show_model_label: bool) -> Self {
        TerminalAgentUi {
            verbose,
            show_model_label,
            spinner: None,
            pending_arguments: None,
            tool_header_open: false,
            approval_shown: false,
            activity: None,
            headless: false,
        }
    }

    /// Drops everything that needs a live terminal, for `--headless`.
    ///
    /// Deliberately one-way: a run either has someone watching it or it
    /// doesn't, and there is no point in the session where that changes.
    pub fn go_headless(&mut self) {
        self.headless = true;
    }

    /// Flips the `-v`-equivalent detail level live, for `/verbose` in a
    /// `session` loop. Takes effect from the next event on.
    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }
}

impl TerminalAgentUi {
    /// Draws one agent event.
    ///
    /// Inherent rather than only reachable through [`AgentUi`], because the
    /// two ways this front end is driven arrive by different routes: the
    /// one-shot `agent` command calls the loop inline and gets events
    /// through the trait, while a `session` running on a
    /// [`crate::conversation::Conversation`] sees them re-emitted as
    /// `Event::Agent(..)` and never touches the trait at all. Both render
    /// identically because both land here.
    pub async fn render_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Steered { text } => {
                // Echoed back the way the prompt would have shown it, so the
                // transcript reads in the order the model saw it rather than
                // the message appearing to have come from nowhere.
                println!("\n{} {}", "❯".green().bold(), text);
            }
            AgentEvent::IterationStarted { iteration } => {
                if self.verbose {
                    println!("{}", format!("\n[Iteration {}]", iteration).bright_black());
                }
            }
            AgentEvent::RequestStarted => {
                if !self.headless {
                    self.spinner = Some(Spinner::start("Thinking..."));
                }
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
                // One blank line after every transcript unit, matching
                // the TUI, which spaces its items the same way
                // regardless of what kind each one is.
                println!();
            }
            AgentEvent::Thinking { text } => {
                if self.verbose {
                    // The spinner is still animating on the current line
                    // here: the reply has resolved, but `RequestFinished`
                    // hasn't been emitted yet. Printing over it lands
                    // mid-line and then gets half-overwritten by the next
                    // redraw, so clear it first and let the thinking
                    // start its own line. The request it was tracking is
                    // already done, so `RequestFinished` simply finds
                    // nothing left to stop.
                    if let Some(spinner) = self.spinner.take() {
                        spinner.stop().await;
                    }
                    // Same marker-plus-hanging-indent shape the
                    // assistant's own reply uses, one step dimmer.
                    println!(
                        "{} {}",
                        "💭".bright_black(),
                        wrap::wrap_indented(&text, "   ").bright_black().italic()
                    );
                    println!();
                }
            }
            AgentEvent::ToolCallStarted { name, arguments } => {
                // Printed without a trailing newline — closed by
                // whichever of approval/denial/completion resolves it
                // next, with a trailing status marker, so the whole
                // call reads as one line: the CLI's equivalent of the
                // TUI transcript's gutter-marker-plus-trailing-status
                // row instead of repeating the tool's name on its own
                // line every time.
                print_tool_header(&name, &arguments);
                self.tool_header_open = true;
                self.approval_shown = false;
                if self.verbose {
                    println!();
                    self.tool_header_open = false;
                    print_fields(&tool_call_fields(&name, &arguments));
                }
                self.pending_arguments = Some(arguments);
            }
            AgentEvent::ToolCallDenied { name: _ } => {
                // Consumes the pending call so the ToolCallCompleted
                // that always follows a denial knows not to report the
                // same call again as if it had succeeded. A denial only
                // ever happens after an approval prompt — the typed `N`
                // that produced it already says how this resolved, so
                // there's no separate marker to draw underneath it.
                let _ = self.pending_arguments.take();
                println!();
            }
            AgentEvent::ToolCallCompleted { name: _, result } => {
                // Only the non-denied path reaches here with a pending
                // entry; a denial already reported itself and cleared it.
                if self.pending_arguments.take().is_none() {
                    return;
                }
                // A call that went through an approval prompt already
                // has its answer on screen (the typed `y`); only a call
                // that ran with no prompt at all still needs its own
                // closing marker.
                if !self.approval_shown {
                    self.close_tool_header("✓".green());
                }
                if self.verbose {
                    print_fields(&json_fields(&result));
                }
                println!();
            }
            AgentEvent::Error { message } => {
                println!("{} {}", "✗".red(), message);
                println!();
            }
            AgentEvent::TurnFinished => {
                if self.verbose {
                    println!("{}", "✓ Agent finished".green());
                    println!();
                }
            }
        }
    }
}

impl AgentUi for TerminalAgentUi {
    fn event(&mut self, event: AgentEvent) -> impl Future<Output = ()> + Send {
        self.render_agent_event(event)
    }

    fn approve(&mut self, request: ApprovalRequest) -> impl Future<Output = Result<bool>> + Send {
        async move { self.prompt_approval(request) }
    }
}

impl TerminalAgentUi {
    /// The blocking stdin prompt behind [`AgentUi::approve`], kept separate
    /// so the async wrapper stays trivial.
    fn prompt_approval(&mut self, request: ApprovalRequest) -> Result<bool> {
        // Denied without asking, and without ever touching stdin. `clank
        // agent --headless` refuses to start while any gate is on, so this
        // is the second line of defence rather than the expected path — but
        // it is the one that matters, because the alternative is a run that
        // blocks forever on an answer nobody is there to give. Never records
        // `AwaitingApproval` either: nothing is awaited.
        if self.headless {
            self.close_tool_header("✗".red());
            self.approval_shown = true;
            println!(
                "  {} {} needs approval, and --headless cannot ask",
                "denied:".red(),
                request.tool_name.cyan()
            );
            return Ok(false);
        }

        // Announced before the prompt blocks on stdin: this is exactly the
        // state worth seeing from another terminal, and it's the one a
        // blocking loop would otherwise never report.
        if let Some(activity) = &self.activity {
            activity.set(
                Some(crate::store::Activity::AwaitingApproval),
                Some(&crate::ui::approval_summary(&request)),
            );
        }
        let answered = self.ask_approval(request);
        if let Some(activity) = &self.activity {
            activity.set(Some(crate::store::Activity::Working), None);
        }
        answered
    }

    fn ask_approval(&mut self, request: ApprovalRequest) -> Result<bool> {
        // Closes the tool-call header with a trailing `?`, matching the
        // TUI transcript's `AwaitingApproval` marker, before the prompt
        // itself — which has no TUI transcript equivalent (that lives in
        // the TUI's separate approval modal instead) — continues below.
        self.close_tool_header("?".yellow());
        self.approval_shown = true;

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

    /// Closes the currently open tool-call header line with a trailing
    /// status marker — landing right after the name/detail on the same
    /// line, matching the TUI transcript's trailing ✓/? — or, if the line
    /// was already closed by a verbose dump, prints the marker on its own
    /// short indented line instead, rather than repeating the tool's name.
    /// Only ever called for `?` (always) and `✓` on a call that ran with no
    /// approval prompt — one that went through a prompt has its answer on
    /// screen already and draws no further marker.
    fn close_tool_header(&mut self, marker: ColoredString) {
        if self.tool_header_open {
            println!(" {marker}");
            self.tool_header_open = false;
        } else {
            println!("  {marker}");
        }
    }
}

/// Prints `🔨 name  detail` with no trailing newline — `detail` is the file
/// path or command the call is acting on when its arguments have one, the
/// same terse identification the TUI always shows for a tool call,
/// regardless of `-v`. Left open for [`TerminalAgentUi::close_tool_header`]
/// to finish with a trailing status marker.
fn print_tool_header(name: &str, arguments: &str) {
    match primary_argument(arguments) {
        Some(detail) => print!(
            "{} {}  {}",
            "🔨".magenta(),
            name.bold(),
            detail.bright_black()
        ),
        None => print!("{} {}", "🔨".magenta(), name.bold()),
    }
    let _ = io::stdout().flush();
}

/// The verbose-only per-field breakdown under a tool notice, matching the
/// TUI's indentation for the same data.
fn print_fields(fields: &[(String, String)]) {
    for (key, shown) in fields {
        println!("     {}  {}", key.bright_black(), shown);
    }
}
