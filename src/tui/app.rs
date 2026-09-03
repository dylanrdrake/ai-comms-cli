//! TUI state and the rules for folding conversation events into it.
//!
//! Deliberately free of rendering and I/O: everything here is a plain state
//! transition, so the interesting behavior (how a stream becomes a transcript
//! block, what happens to input while busy) is testable without a terminal.

use crate::config::ApprovalSettings;
use crate::conversation::Event;
pub use crate::ui::{classify, Submission};
use crate::ui::{AgentEvent, ApprovalRequest};
use std::collections::VecDeque;

/// One rendered block of the conversation.
///
/// Richer than the CLI's transcript, which drops system and tool messages —
/// watching tools run is a big part of why a full-screen UI is worth having,
/// so they get their own entries with live status.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    User(String),
    Assistant {
        text: String,
        /// True while deltas are still arriving, so the view can show a
        /// cursor and the final message can replace rather than append.
        streaming: bool,
        /// Which model produced this block, captured when it was created.
        /// Held per block rather than read from the session so that
        /// switching models mid-conversation doesn't retroactively re-label
        /// everything that came before.
        ///
        /// `None` for replies saved before per-message model tracking
        /// existed. Those are shown as unattributed rather than borrowing
        /// the session's current model, which would assert a model they may
        /// well not have been produced by.
        label: Option<String>,
    },
    /// The model's own thinking for a turn. Only drawn when `/verbose` is
    /// on — the same class of detail as a tool call's arguments.
    Thinking(String),
    ToolCall {
        name: String,
        arguments: String,
        status: ToolStatus,
    },
    /// A command the *user* ran with `$`, kept distinct from `ToolCall` so
    /// it reads as yours rather than as something the agent did. Stays in
    /// the transcript whether or not it was sent to the model: discarding
    /// decides what the model sees, not what you see.
    Shell {
        command: String,
        output: String,
        exit_code: i32,
        sent: bool,
    },
    Error(String),
    Notice(String),
    /// Every setting this session is running with, from `/session`. Held as
    /// rendered rows rather than as the settings themselves: the values are
    /// a snapshot of the moment it was asked for, and shouldn't quietly
    /// change under the reader when a later `/effort` scrolls past.
    SessionStatus(Vec<(String, String)>),
    /// This session's approval gates, pretty-printed the same way `clank
    /// approval` shows them in the CLI rather than packed into one
    /// `Notice` line. Shown both after `/approval <category> <on|off>`
    /// changes something and after a bare `/approval` query.
    ApprovalStatus {
        approval: ApprovalSettings,
        /// Whether this reflects a just-made change, so the header reads
        /// "set to" instead of "is" — the same distinction every other
        /// setting's `Notice` makes.
        changed: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    AwaitingApproval,
    Running,
    Denied,
    Done { result: String },
}

/// How a command's output reads to the model: the command, then what it
/// printed. Labelled as the user's own run so the model doesn't mistake it
/// for something it did — it is a user message, and it says why.
fn shell_message(command: &str, output: &str, exit_code: i32) -> String {
    let output = if output.trim().is_empty() {
        "(no output)"
    } else {
        output.trim_end()
    };
    format!("I ran `{command}` (exit {exit_code}):\n\n{output}")
}

/// Where a `$` command has got to.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellState {
    Running {
        command: String,
    },
    Finished {
        command: String,
        output: String,
        exit_code: i32,
    },
}

pub struct App {
    pub transcript: Vec<TranscriptItem>,
    pub input: String,
    /// Byte index of the cursor within `input`. Kept on a char boundary.
    pub cursor: usize,
    pub busy: bool,
    /// Messages typed while a turn was running, in the order they will be
    /// taken. Held as the text rather than a count so the box above the
    /// prompt can show what is waiting; the count is just its length.
    pub pending: VecDeque<String>,
    pub pending_approval: Option<ApprovalRequest>,
    /// The `$` command in flight, or the one whose output is waiting on a
    /// decision. Only the finished state offers keys — a spinner has nothing
    /// to decide.
    pub pending_shell: Option<ShellState>,
    /// Lines scrolled up from the bottom. 0 means pinned to the newest
    /// content, which is where it stays unless the user scrolls back.
    pub scroll_back: u16,
    /// The model subsequent turns will use. Changes with `/model`.
    pub model: String,
    /// Changes with `/effort`; `None` means "use the configured default".
    pub effort_level: Option<String>,
    /// Not shown in the UI, but sessions are worth keeping uniquely
    /// identifiable regardless.
    #[allow(dead_code)]
    /// The session's full id. Full, not the short form shown to the user:
    /// the picker hashes the whole id for its mark, and the gutter has to
    /// hash exactly the same string or the two marks disagree. `short_id`
    /// derives the display form, so there is only ever one id to get wrong.
    pub session_id: String,
    /// The session's current title, shown in the header. "Untitled" until
    /// the first user message names it.
    pub title: String,
    /// Whether this conversation runs the tool loop, shown in the status bar
    /// so the mode is never ambiguous once you're inside a session.
    pub agentic: bool,
    /// Mirrors the plain CLI's `-v`: gates whether tool call arguments and
    /// results are shown, not just that a tool ran. Toggled with `/verbose`.
    pub verbose: bool,
    /// Whether this session bands the user's own messages.
    pub highlight: bool,
    /// Whether the agent's file writes are confined to the working
    /// directory. Changed with `/sandbox`.
    pub sandbox: bool,
    /// Whether this session streams replies token-by-token. Changed with
    /// `/stream`.
    pub stream: bool,
    /// The directory this session was started in, shown by `/status`.
    pub working_dir: Option<String>,
    /// This session's `/max-iterations` override, changed with
    /// `/max-iterations`/`/max-iterations default`. `None` means nullified —
    /// turns fall back to the configured default. Only takes effect in
    /// agent mode.
    pub max_iterations: Option<usize>,
    /// This session's `/temperature` override, changed with
    /// `/temperature`/`/temperature default`. `None` means nullified, same
    /// deal as `max_iterations`.
    pub temperature: Option<f32>,
    /// Changes with `/approval`. Not shown in the status bar, but folded
    /// here so a `Notice` can report what it actually ended up as.
    pub approval: ApprovalSettings,
    /// Previously submitted lines, oldest first, that Up/Down recall into
    /// the input box — the TUI's equivalent of the plain CLI's readline
    /// history. Seeded from a resumed session's past turns.
    pub input_history: Vec<String>,
    /// Position within `input_history` while browsing it; `None` means the
    /// box holds a fresh draft rather than a recalled entry.
    history_cursor: Option<usize>,
    /// What was being typed before Up was first pressed, restored once Down
    /// walks back past the newest history entry.
    draft: String,
}

impl App {
    pub fn new(model: String, effort_level: Option<String>, session_id: String) -> Self {
        App {
            transcript: Vec::new(),
            input: String::new(),
            cursor: 0,
            busy: false,
            pending: VecDeque::new(),
            pending_approval: None,
            pending_shell: None,
            scroll_back: 0,
            model,
            effort_level,
            session_id,
            title: "Untitled".to_string(),
            agentic: false,
            verbose: false,
            highlight: true,
            sandbox: true,
            stream: true,
            working_dir: None,
            max_iterations: None,
            temperature: None,
            approval: ApprovalSettings::default(),
            input_history: Vec::new(),
            history_cursor: None,
            draft: String::new(),
        }
    }

    /// How the current model is displayed, e.g. "orcarouter/auto (high)".
    pub fn label(&self) -> String {
        crate::ui::response_label(&self.model, &self.effort_level)
    }

    pub fn is_pinned_to_bottom(&self) -> bool {
        self.scroll_back == 0
    }

    /// Folds one worker event into the view.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::UserMessage(text) => {
                // A message that was waiting has started its own turn. Does
                // nothing for one sent while idle, which never waited.
                self.take_pending(&text);
                self.transcript.push(TranscriptItem::User(text));
            }
            Event::Busy(busy) => {
                self.busy = busy;
                if !busy {
                    // A turn can end mid-stream (cancelled, or a failure
                    // after partial text); make sure nothing is left marked
                    // as still streaming.
                    self.finish_streaming();
                }
            }
            Event::Queued { text } => self.pending.push_back(text),
            Event::ShellStarted { command } => {
                // A result still waiting when the next command starts is
                // dropped from the conversation — but recorded, because
                // losing output you were deciding about with no trace of it
                // is the worse failure. Discarding is the safe reading:
                // sending on your behalf puts something in the context you
                // never agreed to.
                self.settle_shell(false);
                self.pending_shell = Some(ShellState::Running { command });
            }
            Event::ShellFinished {
                command,
                output,
                exit_code,
            } => {
                self.pending_shell = Some(ShellState::Finished {
                    command,
                    output,
                    exit_code,
                });
            }
            Event::Cancelled => {
                self.finish_streaming();
                self.pending.clear();
                self.pending_approval = None;
                // Any tool frozen mid-flight is no longer going to resolve.
                for item in self.transcript.iter_mut().rev() {
                    if let TranscriptItem::ToolCall { status, .. } = item {
                        if matches!(status, ToolStatus::AwaitingApproval | ToolStatus::Running) {
                            *status = ToolStatus::Denied;
                        }
                        break;
                    }
                }
                self.transcript
                    .push(TranscriptItem::Notice("Cancelled".to_string()));
            }
            Event::ApprovalRequested(request) => {
                // The call was optimistically marked Running when it started;
                // correct that now that we know it's gated on the user.
                self.set_last_tool_status(ToolStatus::AwaitingApproval);
                self.pending_approval = Some(request);
            }
            Event::ModelChanged {
                model,
                effort_level,
            } => {
                let changed = model != self.model;
                self.model = model;
                self.effort_level = effort_level;
                let label = self.label();
                self.transcript.push(TranscriptItem::Notice(if changed {
                    format!("Model set to {label}")
                } else {
                    format!("Model is {label}")
                }));
            }
            Event::AgenticChanged { agentic } => {
                let changed = agentic != self.agentic;
                self.agentic = agentic;
                let label = if agentic {
                    "agent mode (tools enabled)"
                } else {
                    "ask mode (no tools)"
                };
                self.transcript.push(TranscriptItem::Notice(if changed {
                    format!("Switched to {label}")
                } else {
                    format!("Already in {label}")
                }));
            }
            Event::EffortChanged { effort_level } => {
                let changed = effort_level != self.effort_level;
                self.effort_level = effort_level;
                let label = self.effort_level.as_deref().unwrap_or("default");
                self.transcript.push(TranscriptItem::Notice(if changed {
                    format!("Effort set to {label}")
                } else {
                    format!("Effort is {label}")
                }));
            }
            Event::StreamChanged { stream } => {
                let changed = stream != self.stream;
                self.stream = stream;
                if changed {
                    self.transcript
                        .push(TranscriptItem::Notice(crate::ui::stream_notice(
                            stream, true,
                        )));
                }
            }
            Event::SandboxChanged { sandbox } => {
                let changed = sandbox != self.sandbox;
                self.sandbox = sandbox;
                if changed {
                    self.transcript
                        .push(TranscriptItem::Notice(crate::ui::sandbox_notice(
                            sandbox, true,
                        )));
                }
            }
            Event::HighlightChanged { highlight } => {
                self.highlight = highlight;
                self.transcript
                    .push(TranscriptItem::Notice(crate::ui::highlight_notice(
                        highlight, true,
                    )));
            }
            Event::VerboseChanged { verbose } => {
                self.verbose = verbose;
                self.transcript.push(TranscriptItem::Notice(
                    if verbose {
                        "Verbose mode on"
                    } else {
                        "Verbose mode off"
                    }
                    .to_string(),
                ));
            }
            Event::MaxIterationsChanged { max_iterations } => {
                let changed = max_iterations != self.max_iterations;
                self.max_iterations = max_iterations;
                let label = self
                    .max_iterations
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "default".to_string());
                self.transcript.push(TranscriptItem::Notice(if changed {
                    format!("Max iterations set to {label}")
                } else {
                    format!("Max iterations is {label}")
                }));
            }
            Event::TemperatureChanged { temperature } => {
                let changed = temperature != self.temperature;
                self.temperature = temperature;
                let label = self
                    .temperature
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "default".to_string());
                self.transcript.push(TranscriptItem::Notice(if changed {
                    format!("Temperature set to {label}")
                } else {
                    format!("Temperature is {label}")
                }));
            }
            Event::ApprovalSettingsChanged { approval } => {
                let changed = approval != self.approval;
                self.approval = approval.clone();
                self.transcript
                    .push(TranscriptItem::ApprovalStatus { approval, changed });
            }
            // Purely cosmetic — the header re-renders with whatever this
            // is next frame, with no need to call it out in the transcript.
            Event::TitleChanged { title } => self.title = title,
            Event::Agent(event) => self.apply_agent(event),
        }
    }

    fn apply_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::AssistantDelta { text } => self.push_delta(&text),
            AgentEvent::AssistantMessage {
                model,
                effort_level,
                text,
            } => {
                // Streaming already built this block delta by delta; replace
                // its text so the two can never disagree, and fall back to
                // creating it when streaming is off.
                match self.last_streaming_assistant() {
                    Some(existing) => *existing = text,
                    None => self.transcript.push(TranscriptItem::Assistant {
                        text,
                        streaming: false,
                        // The event knows which model actually produced this,
                        // which beats assuming it was the current one.
                        label: Some(crate::ui::response_label(&model, &effort_level)),
                    }),
                }
                self.finish_streaming();
            }
            AgentEvent::Thinking { text } => self.push_thinking(text),
            AgentEvent::ToolCallStarted { name, arguments } => {
                self.finish_streaming();
                self.transcript.push(TranscriptItem::ToolCall {
                    name,
                    arguments,
                    // Assume it runs; an ApprovalRequested right behind this
                    // downgrades it to AwaitingApproval when it's gated.
                    status: ToolStatus::Running,
                });
            }
            AgentEvent::ToolCallDenied { .. } => {
                self.set_last_tool_status(ToolStatus::Denied);
                self.pending_approval = None;
            }
            AgentEvent::ToolCallCompleted { result, .. } => {
                self.set_last_tool_status(ToolStatus::Done { result });
                self.pending_approval = None;
            }
            AgentEvent::Error { message } => {
                self.finish_streaming();
                self.transcript.push(TranscriptItem::Error(message));
            }
            AgentEvent::Steered { text } => {
                self.finish_streaming();
                self.take_pending(&text);
                self.transcript.push(TranscriptItem::User(text));
            }
            // Busy state is driven by Event::Busy, which brackets the whole
            // turn rather than each request within it.
            AgentEvent::RequestStarted
            | AgentEvent::RequestFinished
            | AgentEvent::IterationStarted { .. }
            | AgentEvent::TurnFinished => {}
        }
    }

    /// Settles a finished `$` command: it leaves the box and joins the
    /// transcript either way, marked with whether the model was given it.
    ///
    /// Returns the text to send, when it is being sent. Sending appends it
    /// to the conversation without starting a turn — the model reads it when
    /// you next say something, so the output and the question you have about
    /// it arrive together instead of costing two round trips.
    pub fn settle_shell(&mut self, sent: bool) -> Option<String> {
        // Checked before taking: the keys are live the whole time a command
        // is on screen, and taking first would throw away one that is still
        // running.
        if !matches!(self.pending_shell, Some(ShellState::Finished { .. })) {
            return None;
        }
        let Some(ShellState::Finished {
            command,
            output,
            exit_code,
        }) = self.pending_shell.take()
        else {
            return None;
        };

        let message = sent.then(|| shell_message(&command, &output, exit_code));
        self.transcript.push(TranscriptItem::Shell {
            command,
            output,
            exit_code,
            sent,
        });
        message
    }

    /// The first eight characters of the id — enough to name the session at
    /// `clank resume`, and what `/status` shows.
    pub fn short_id(&self) -> &str {
        &self.session_id[..8.min(self.session_id.len())]
    }

    /// Drops the first message waiting with this text, if any.
    ///
    /// Matched by text rather than by an id threaded through `Command`,
    /// `Event` and `Steering`. Two identical messages waiting at once would
    /// come out in the wrong order, which is invisible for exactly the
    /// reason it can happen: they are identical.
    fn take_pending(&mut self, text: &str) {
        if let Some(at) = self.pending.iter().position(|waiting| waiting == text) {
            self.pending.remove(at);
        }
    }

    /// Thinking resolves with the request, which — when streaming — is
    /// after the reply it led to has already been painted delta by delta.
    /// Slot it in ahead of that block so the transcript still reads in the
    /// order the model worked: what it thought, then what it said.
    fn push_thinking(&mut self, text: String) {
        let item = TranscriptItem::Thinking(text);
        match self.transcript.last() {
            Some(TranscriptItem::Assistant {
                streaming: true, ..
            }) => {
                let before_last = self.transcript.len() - 1;
                self.transcript.insert(before_last, item);
            }
            _ => self.transcript.push(item),
        }
    }

    fn push_delta(&mut self, text: &str) {
        match self.last_streaming_assistant() {
            Some(existing) => existing.push_str(text),
            None => {
                let label = Some(self.label());
                self.transcript.push(TranscriptItem::Assistant {
                    text: text.to_string(),
                    streaming: true,
                    label,
                })
            }
        }
    }

    /// The text of the trailing assistant block, if it's still streaming.
    fn last_streaming_assistant(&mut self) -> Option<&mut String> {
        match self.transcript.last_mut() {
            Some(TranscriptItem::Assistant {
                text, streaming, ..
            }) if *streaming => Some(text),
            _ => None,
        }
    }

    fn finish_streaming(&mut self) {
        if let Some(TranscriptItem::Assistant { streaming, .. }) = self.transcript.last_mut() {
            *streaming = false;
        }
    }

    fn set_last_tool_status(&mut self, status: ToolStatus) {
        for item in self.transcript.iter_mut().rev() {
            if let TranscriptItem::ToolCall {
                status: existing, ..
            } = item
            {
                if !matches!(existing, ToolStatus::Done { .. } | ToolStatus::Denied) {
                    *existing = status;
                    return;
                }
            }
        }
    }

    /// Reflects the user's answer to an approval prompt immediately, rather
    /// than waiting for the round trip through the worker, so the tool stops
    /// showing as "awaiting" the moment they decide.
    pub fn approval_answered(&mut self, allowed: bool) {
        self.pending_approval = None;
        if allowed {
            self.set_last_tool_status(ToolStatus::Running);
        }
        // A denial is left to the worker's ToolCallDenied, which is what
        // actually settles the call.
    }

    // --- input editing ---------------------------------------------------

    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Inserts a terminal paste at the cursor as literal text — including
    /// any embedded newlines — rather than one character at a time. Plain
    /// per-character delivery is what let a pasted newline be read as a
    /// real Enter and submit each line as its own message; bracketed paste
    /// (enabled around the event loop) is what routes it here instead.
    pub fn paste(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        self.input.insert_str(self.cursor, &normalized);
        self.cursor += normalized.len();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Step back a whole character, not a byte, so multi-byte input
        // (emoji, accents) deletes cleanly.
        let prev = self.input[..self.cursor]
            .chars()
            .next_back()
            .map(|c| c.len_utf8())
            .unwrap_or(0);
        self.cursor -= prev;
        self.input.remove(self.cursor);
    }

    pub fn move_left(&mut self) {
        if let Some(c) = self.input[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
    }

    pub fn move_right(&mut self) {
        if let Some(c) = self.input[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    /// Takes the current input, clearing the box. Returns `None` when it's
    /// blank so a stray Enter doesn't send an empty turn.
    pub fn take_input(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input.clear();
        self.cursor = 0;
        self.history_cursor = None;
        self.draft.clear();
        // Skip a duplicate of the immediately preceding entry, matching
        // ordinary shell history, so repeating a line doesn't pad it.
        if self.input_history.last().map(String::as_str) != Some(text.as_str()) {
            self.input_history.push(text.clone());
        }
        Some(text)
    }

    /// Clears the input box and returns whatever was typed there, for an
    /// approval prompt to interpret. Unlike [`Self::take_input`], a blank
    /// answer is meaningful (it denies, matching a conventional `[y/N]:`
    /// prompt) rather than swallowed as a stray Enter, and it isn't added to
    /// prompt history — a "y" or "n" isn't a message worth recalling later.
    /// Recalls an older entry into the input box. The first press stashes
    /// whatever was being typed so Down can return to it later.
    pub fn history_up(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            None => {
                self.draft = self.input.clone();
                self.input_history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_cursor = Some(next);
        self.input = self.input_history[next].clone();
        self.cursor = self.input.len();
    }

    /// Steps toward more recent history, restoring the stashed draft once
    /// it walks past the newest entry.
    pub fn history_down(&mut self) {
        let Some(i) = self.history_cursor else {
            return;
        };
        if i + 1 < self.input_history.len() {
            self.history_cursor = Some(i + 1);
            self.input = self.input_history[i + 1].clone();
        } else {
            self.history_cursor = None;
            self.input = self.draft.clone();
        }
        self.cursor = self.input.len();
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_app_holds_the_full_id_so_its_mark_matches_the_picker() {
        // The bug this pins: the app used to hold `short_id`, so the reply
        // gutter hashed eight characters while the picker hashed all
        // thirty-six. Same function, different seeds, unrelated marks — and
        // no test comparing the two functions could have caught it, because
        // the fault was in what the call sites fed them.
        let full = "4f2a91b2-3c1d-4e8a-9f02-7b6c5d4e3a21";
        let a = App::new("m".to_string(), None, full.to_string());

        assert_eq!(a.session_id, full, "the whole id, not the display form");
        assert_eq!(a.short_id(), "4f2a91b2");
    }

    #[test]
    fn a_short_id_survives_an_id_shorter_than_eight() {
        let a = App::new("m".to_string(), None, "abc".to_string());
        assert_eq!(a.short_id(), "abc");
    }

    #[test]
    fn a_second_command_records_the_one_it_replaces() {
        let mut a = app();
        a.apply(Event::ShellFinished {
            command: "ls".to_string(),
            output: "src target".to_string(),
            exit_code: 0,
        });

        // Fired before answering the first. The output must not simply
        // vanish — it was on screen and undecided a moment ago.
        a.apply(Event::ShellStarted {
            command: "pwd".to_string(),
        });

        assert!(matches!(
            a.transcript.last(),
            Some(TranscriptItem::Shell { command, sent: false, .. }) if command == "ls"
        ));
        assert_eq!(
            a.pending_shell,
            Some(ShellState::Running {
                command: "pwd".to_string()
            })
        );
    }

    #[test]
    fn starting_a_command_while_one_runs_loses_nothing() {
        // Nothing is waiting on a decision, so there is nothing to record.
        let mut a = app();
        a.apply(Event::ShellStarted {
            command: "sleep 5".to_string(),
        });
        a.apply(Event::ShellStarted {
            command: "ls".to_string(),
        });
        assert!(a.transcript.is_empty());
    }

    #[test]
    fn a_command_shows_while_it_runs_then_offers_its_output() {
        let mut a = app();
        a.apply(Event::ShellStarted {
            command: "cargo test".to_string(),
        });
        assert_eq!(
            a.pending_shell,
            Some(ShellState::Running {
                command: "cargo test".to_string()
            })
        );

        a.apply(Event::ShellFinished {
            command: "cargo test".to_string(),
            output: "299 passed".to_string(),
            exit_code: 0,
        });
        assert!(matches!(a.pending_shell, Some(ShellState::Finished { .. })));
    }

    #[test]
    fn sending_a_command_hands_back_a_message_naming_it() {
        let mut a = app();
        a.apply(Event::ShellFinished {
            command: "cargo test".to_string(),
            output: "299 passed".to_string(),
            exit_code: 0,
        });

        let message = a.settle_shell(true).expect("sent");
        assert!(message.contains("cargo test"), "{message}");
        assert!(message.contains("299 passed"), "{message}");
        assert!(a.pending_shell.is_none(), "the box is done with");
        // It stays visible either way: discarding decides what the model
        // sees, not what the user sees.
        assert!(matches!(
            a.transcript.last(),
            Some(TranscriptItem::Shell { sent: true, .. })
        ));
    }

    #[test]
    fn discarding_keeps_it_on_screen_and_out_of_the_conversation() {
        let mut a = app();
        a.apply(Event::ShellFinished {
            command: "ls".to_string(),
            output: "src".to_string(),
            exit_code: 0,
        });

        assert_eq!(a.settle_shell(false), None, "nothing to send");
        assert!(a.pending_shell.is_none());
        assert!(matches!(
            a.transcript.last(),
            Some(TranscriptItem::Shell { sent: false, .. })
        ));
    }

    #[test]
    fn a_running_command_has_nothing_to_settle_yet() {
        let mut a = app();
        a.apply(Event::ShellStarted {
            command: "sleep 5".to_string(),
        });
        // The keys are live the whole time; they must not eat a command
        // that hasn't produced anything.
        assert_eq!(a.settle_shell(true), None);
        assert!(a.pending_shell.is_some(), "still running");
        assert_eq!(a.settle_shell(false), None);
        assert!(a.pending_shell.is_some());
    }

    #[test]
    fn a_failing_command_says_so_in_what_the_model_reads() {
        let mut a = app();
        a.apply(Event::ShellFinished {
            command: "cargo build".to_string(),
            output: "error[E0308]: mismatched types".to_string(),
            exit_code: 101,
        });
        let message = a.settle_shell(true).expect("sent");
        assert!(message.contains("101"), "{message}");
        assert!(message.contains("E0308"), "{message}");
    }

    fn queued(text: &str) -> Event {
        Event::Queued {
            text: text.to_string(),
        }
    }

    #[test]
    fn waiting_messages_stack_in_the_order_they_will_be_taken() {
        let mut a = app();
        a.apply(queued("check Windows too"));
        a.apply(queued("and the seam"));
        assert_eq!(a.pending, ["check Windows too", "and the seam"]);
    }

    #[test]
    fn a_steered_message_leaves_the_queue_and_lands_in_the_transcript() {
        let mut a = app();
        a.apply(queued("check Windows too"));
        a.apply(queued("and the seam"));

        a.apply(Event::Agent(AgentEvent::Steered {
            text: "check Windows too".to_string(),
        }));

        // It reads as the user's message, at the point the loop took it —
        // not at the end of the turn, which is when it would otherwise have
        // been sent.
        assert!(
            matches!(a.transcript.last(), Some(TranscriptItem::User(text)) if text == "check Windows too"),
            "{:?}",
            a.transcript.last()
        );
        assert_eq!(a.pending, ["and the seam"], "the rest keep waiting");
    }

    #[test]
    fn a_waiting_message_leaves_when_it_starts_its_own_turn() {
        let mut a = app();
        a.apply(queued("run it again"));
        a.apply(Event::UserMessage("run it again".to_string()));
        assert!(a.pending.is_empty());
    }

    #[test]
    fn a_message_sent_while_idle_never_waited() {
        let mut a = app();
        a.apply(queued("still waiting"));
        // Nothing was queued for this one, so it must not consume an entry
        // that belongs to a different message.
        a.apply(Event::UserMessage("typed just now".to_string()));
        assert_eq!(a.pending, ["still waiting"]);
    }

    use super::*;

    fn app() -> App {
        App::new("test-model".to_string(), None, "abcd1234".to_string())
    }

    fn delta(app: &mut App, text: &str) {
        app.apply(Event::Agent(AgentEvent::AssistantDelta {
            text: text.to_string(),
        }));
    }

    #[test]
    fn deltas_accumulate_into_one_streaming_block() {
        let mut a = app();
        delta(&mut a, "Hello");
        delta(&mut a, ", world");
        assert_eq!(
            a.transcript,
            vec![TranscriptItem::Assistant {
                text: "Hello, world".to_string(),
                streaming: true,
                label: Some("test-model".to_string())
            }]
        );
    }

    #[test]
    fn final_message_replaces_streamed_text_rather_than_duplicating() {
        let mut a = app();
        delta(&mut a, "Hel");
        delta(&mut a, "lo");
        a.apply(Event::Agent(AgentEvent::AssistantMessage {
            model: "m".into(),
            effort_level: None,
            text: "Hello".into(),
        }));
        // One block, not two, and no longer marked streaming.
        assert_eq!(
            a.transcript,
            vec![TranscriptItem::Assistant {
                text: "Hello".to_string(),
                streaming: false,
                label: Some("test-model".to_string())
            }]
        );
    }

    #[test]
    fn works_with_streaming_off_when_only_a_final_message_arrives() {
        let mut a = app();
        a.apply(Event::Agent(AgentEvent::AssistantMessage {
            model: "m".into(),
            effort_level: None,
            text: "Whole reply".into(),
        }));
        assert_eq!(
            a.transcript,
            vec![TranscriptItem::Assistant {
                text: "Whole reply".to_string(),
                streaming: false,
                label: Some("m".to_string())
            }]
        );
    }

    #[test]
    fn a_tool_call_closes_the_streaming_block_before_it() {
        let mut a = app();
        delta(&mut a, "I'll read that.");
        a.apply(Event::Agent(AgentEvent::ToolCallStarted {
            name: "read_file".into(),
            arguments: "{}".into(),
        }));
        assert_eq!(
            a.transcript[0],
            TranscriptItem::Assistant {
                text: "I'll read that.".to_string(),
                streaming: false,
                label: Some("test-model".to_string())
            }
        );
        assert!(matches!(
            a.transcript[1],
            TranscriptItem::ToolCall {
                status: ToolStatus::Running,
                ..
            }
        ));
    }

    #[test]
    fn a_gated_tool_is_downgraded_to_awaiting_then_runs_once_allowed() {
        let mut a = app();
        a.apply(Event::Agent(AgentEvent::ToolCallStarted {
            name: "write_file".into(),
            arguments: "{}".into(),
        }));
        a.apply(Event::ApprovalRequested(ApprovalRequest {
            tool_name: "write_file".into(),
            category: "write",
            arguments: "{}".into(),
        }));
        assert!(matches!(
            &a.transcript[0],
            TranscriptItem::ToolCall {
                status: ToolStatus::AwaitingApproval,
                ..
            }
        ));

        a.approval_answered(true);
        assert!(a.pending_approval.is_none());
        assert!(matches!(
            &a.transcript[0],
            TranscriptItem::ToolCall {
                status: ToolStatus::Running,
                ..
            }
        ));
    }

    #[test]
    fn tool_status_advances_and_clears_the_approval_prompt() {
        let mut a = app();
        a.apply(Event::Agent(AgentEvent::ToolCallStarted {
            name: "write_file".into(),
            arguments: "{}".into(),
        }));
        a.apply(Event::ApprovalRequested(ApprovalRequest {
            tool_name: "write_file".into(),
            category: "write",
            arguments: "{}".into(),
        }));
        assert!(a.pending_approval.is_some());

        a.apply(Event::Agent(AgentEvent::ToolCallCompleted {
            name: "write_file".into(),
            result: r#"{"success":true}"#.into(),
        }));
        assert!(a.pending_approval.is_none());
        assert!(matches!(
            &a.transcript[0],
            TranscriptItem::ToolCall {
                status: ToolStatus::Done { .. },
                ..
            }
        ));
    }

    #[test]
    fn denial_marks_the_tool_denied() {
        let mut a = app();
        a.apply(Event::Agent(AgentEvent::ToolCallStarted {
            name: "write_file".into(),
            arguments: "{}".into(),
        }));
        a.apply(Event::Agent(AgentEvent::ToolCallDenied {
            name: "write_file".into(),
        }));
        assert!(matches!(
            &a.transcript[0],
            TranscriptItem::ToolCall {
                status: ToolStatus::Denied,
                ..
            }
        ));
    }

    #[test]
    fn cancel_settles_streaming_and_pending_tools() {
        let mut a = app();
        delta(&mut a, "partial answer");
        a.apply(Event::Agent(AgentEvent::ToolCallStarted {
            name: "run_terminal_command".into(),
            arguments: "{}".into(),
        }));
        a.apply(Event::Cancelled);

        assert!(matches!(
            &a.transcript[0],
            TranscriptItem::Assistant {
                streaming: false,
                ..
            }
        ));
        assert!(matches!(
            &a.transcript[1],
            TranscriptItem::ToolCall {
                status: ToolStatus::Denied,
                ..
            }
        ));
        assert_eq!(a.transcript[2], TranscriptItem::Notice("Cancelled".into()));
        assert!(a.pending_approval.is_none());
        assert!(a.pending.is_empty(), "cancelling drops what was waiting");
    }

    #[test]
    fn ending_a_turn_never_leaves_text_marked_streaming() {
        let mut a = app();
        delta(&mut a, "half a sen");
        a.apply(Event::Busy(false));
        assert!(matches!(
            &a.transcript[0],
            TranscriptItem::Assistant {
                streaming: false,
                ..
            }
        ));
    }

    #[test]
    fn errors_appear_as_their_own_block() {
        let mut a = app();
        a.apply(Event::Agent(AgentEvent::Error {
            message: "API error: 500".into(),
        }));
        assert_eq!(
            a.transcript,
            vec![TranscriptItem::Error("API error: 500".to_string())]
        );
    }

    // --- input editing ---------------------------------------------------

    #[test]
    fn model_changed_updates_the_label_and_notes_it() {
        let mut a = app();
        a.apply(Event::ModelChanged {
            model: "anthropic/claude-opus-4.5".to_string(),
            effort_level: Some("high".to_string()),
        });
        assert_eq!(a.model, "anthropic/claude-opus-4.5");
        assert_eq!(a.label(), "anthropic/claude-opus-4.5 (high)");
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice(
                "Model set to anthropic/claude-opus-4.5 (high)".to_string()
            ))
        );
    }

    #[test]
    fn asking_for_the_current_model_reports_rather_than_claiming_a_change() {
        let mut a = app();
        a.apply(Event::ModelChanged {
            model: "test-model".to_string(),
            effort_level: None,
        });
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice("Model is test-model".to_string()))
        );
    }

    #[test]
    fn agentic_changed_updates_the_flag_and_notes_it() {
        let mut a = app();
        assert!(!a.agentic);

        a.apply(Event::AgenticChanged { agentic: true });
        assert!(a.agentic);
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice(
                "Switched to agent mode (tools enabled)".to_string()
            ))
        );

        // Repeating the same mode reports rather than claiming a change.
        a.apply(Event::AgenticChanged { agentic: true });
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice(
                "Already in agent mode (tools enabled)".to_string()
            ))
        );

        a.apply(Event::AgenticChanged { agentic: false });
        assert!(!a.agentic);
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice(
                "Switched to ask mode (no tools)".to_string()
            ))
        );
    }

    #[test]
    fn effort_changed_updates_the_field_and_notes_it() {
        let mut a = app();
        assert_eq!(a.effort_level, None);

        a.apply(Event::EffortChanged {
            effort_level: Some("high".to_string()),
        });
        assert_eq!(a.effort_level, Some("high".to_string()));
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice("Effort set to high".to_string()))
        );

        a.apply(Event::EffortChanged {
            effort_level: Some("high".to_string()),
        });
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice("Effort is high".to_string()))
        );

        a.apply(Event::EffortChanged { effort_level: None });
        assert_eq!(a.effort_level, None);
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice("Effort set to default".to_string()))
        );
    }

    #[test]
    fn temperature_changed_updates_the_field_and_notes_it() {
        let mut a = app();
        assert_eq!(a.temperature, None);

        a.apply(Event::TemperatureChanged {
            temperature: Some(1.5),
        });
        assert_eq!(a.temperature, Some(1.5));
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice(
                "Temperature set to 1.5".to_string()
            ))
        );

        a.apply(Event::TemperatureChanged {
            temperature: Some(1.5),
        });
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice("Temperature is 1.5".to_string()))
        );

        // `/temperature clear` nullifies — this app-layer label just falls
        // back to "default", same as effort's.
        a.apply(Event::TemperatureChanged { temperature: None });
        assert_eq!(a.temperature, None);
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice(
                "Temperature set to default".to_string()
            ))
        );
    }

    #[test]
    fn thinking_slots_in_ahead_of_the_reply_it_led_to() {
        // Streaming paints the reply first and the thinking only resolves
        // with the request, so the item has to go in above the block that
        // is already on screen — otherwise the transcript reads backwards.
        let mut a = app();
        a.apply(Event::Agent(AgentEvent::AssistantDelta {
            text: "the answer".to_string(),
        }));
        a.apply(Event::Agent(AgentEvent::Thinking {
            text: "the thought".to_string(),
        }));

        assert_eq!(
            a.transcript[a.transcript.len() - 2],
            TranscriptItem::Thinking("the thought".to_string())
        );
        assert!(matches!(
            a.transcript.last(),
            Some(TranscriptItem::Assistant { text, .. }) if text == "the answer"
        ));
    }

    #[test]
    fn thinking_appends_when_nothing_is_streaming() {
        // The non-streaming path: no reply on screen yet, so it simply goes
        // on the end and the reply lands after it.
        let mut a = app();
        a.apply(Event::Agent(AgentEvent::Thinking {
            text: "the thought".to_string(),
        }));
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Thinking("the thought".to_string()))
        );
    }

    #[test]
    fn verbose_changed_updates_the_flag_and_notes_it() {
        let mut a = app();
        assert!(!a.verbose);

        a.apply(Event::VerboseChanged { verbose: true });
        assert!(a.verbose);
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice("Verbose mode on".to_string()))
        );

        a.apply(Event::VerboseChanged { verbose: false });
        assert!(!a.verbose);
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::Notice("Verbose mode off".to_string()))
        );
    }

    #[test]
    fn approval_changed_updates_the_field_and_notes_it() {
        let mut a = app();
        assert_eq!(a.approval, ApprovalSettings::default());

        let updated = ApprovalSettings {
            read_disk: false,
            write_disk: true,
            terminal: true,
        };
        a.apply(Event::ApprovalSettingsChanged {
            approval: updated.clone(),
        });
        assert_eq!(a.approval, updated);
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::ApprovalStatus {
                approval: updated.clone(),
                changed: true,
            })
        );

        // Repeating the same settings reports rather than claiming a change.
        a.apply(Event::ApprovalSettingsChanged {
            approval: updated.clone(),
        });
        assert_eq!(
            a.transcript.last(),
            Some(&TranscriptItem::ApprovalStatus {
                approval: updated,
                changed: false,
            })
        );
    }

    #[test]
    fn title_changed_updates_silently() {
        let mut a = app();
        assert_eq!(a.title, "Untitled");
        let before = a.transcript.len();

        a.apply(Event::TitleChanged {
            title: "Write me a snake game".to_string(),
        });
        assert_eq!(a.title, "Write me a snake game");
        // Purely cosmetic — nothing is added to the transcript for it.
        assert_eq!(a.transcript.len(), before);
    }

    #[test]
    fn switching_models_does_not_relabel_earlier_replies() {
        let mut a = app();
        delta(&mut a, "answered by the first model");
        a.apply(Event::Busy(false));
        a.apply(Event::ModelChanged {
            model: "second-model".to_string(),
            effort_level: None,
        });
        delta(&mut a, "answered by the second");

        let labels: Vec<&str> = a
            .transcript
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::Assistant { label, .. } => label.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["test-model", "second-model"]);
    }

    #[test]
    fn blank_input_is_not_sendable() {
        let mut a = app();
        assert!(a.take_input().is_none());
        a.input = "   ".to_string();
        assert!(a.take_input().is_none());
    }

    #[test]
    fn take_input_trims_and_clears() {
        let mut a = app();
        for c in "  hi  ".chars() {
            a.insert_char(c);
        }
        assert_eq!(a.take_input(), Some("hi".to_string()));
        assert!(a.input.is_empty());
        assert_eq!(a.cursor, 0);
    }

    #[test]
    fn history_up_and_down_walk_submitted_lines_and_restore_the_draft() {
        let mut a = app();
        for text in ["first", "second", "third"] {
            for c in text.chars() {
                a.insert_char(c);
            }
            a.take_input();
        }

        // Start a fresh, unsent draft before recalling anything.
        for c in "unsent".chars() {
            a.insert_char(c);
        }

        a.history_up();
        assert_eq!(a.input, "third");
        a.history_up();
        assert_eq!(a.input, "second");
        a.history_up();
        assert_eq!(a.input, "first");
        // Already at the oldest entry; another Up is a no-op.
        a.history_up();
        assert_eq!(a.input, "first");

        a.history_down();
        assert_eq!(a.input, "second");
        a.history_down();
        assert_eq!(a.input, "third");
        // Past the newest entry, the stashed draft comes back.
        a.history_down();
        assert_eq!(a.input, "unsent");
        // Down with nothing being browsed does nothing.
        a.history_down();
        assert_eq!(a.input, "unsent");
    }

    #[test]
    fn take_input_skips_consecutive_duplicates_in_history() {
        let mut a = app();
        for _ in 0..2 {
            for c in "repeat".chars() {
                a.insert_char(c);
            }
            a.take_input();
        }
        assert_eq!(a.input_history, vec!["repeat".to_string()]);
    }

    #[test]
    fn paste_inserts_multiline_text_without_submitting() {
        let mut a = app();
        a.insert_char('x');
        a.paste("line one\nline two\r\nline three\r");
        assert_eq!(a.input, "xline one\nline two\nline three\n");
        assert_eq!(a.cursor, a.input.len());
        // Nothing was submitted — take_input still returns it all as one
        // pending message, which is the whole point of routing a paste here
        // instead of letting embedded newlines fall through as Enter.
        assert_eq!(
            a.take_input(),
            Some("xline one\nline two\nline three".to_string())
        );
    }

    #[test]
    fn typing_survives_an_approval_arriving() {
        // The approval used to borrow the input box as its answer buffer,
        // so a draft in progress was consumed by answering. It has its own
        // box now and the input is left alone.
        let mut a = app();
        for c in "half a thou".chars() {
            a.insert_char(c);
        }

        a.apply(Event::ApprovalRequested(ApprovalRequest {
            tool_name: "write_file".into(),
            category: "write",
            arguments: "{}".into(),
        }));
        a.paste("ght");
        assert_eq!(a.input, "half a thought");

        a.approval_answered(true);
        assert_eq!(
            a.input, "half a thought",
            "answering must not eat the draft"
        );
        assert_eq!(a.cursor, a.input.len());
    }

    #[test]
    fn editing_handles_multibyte_characters() {
        let mut a = app();
        for c in "café".chars() {
            a.insert_char(c);
        }
        assert_eq!(a.cursor, a.input.len());
        a.backspace();
        assert_eq!(a.input, "caf");
        a.move_left();
        a.insert_char('é');
        assert_eq!(a.input, "caéf");
    }

    #[test]
    fn cursor_movement_stops_at_the_ends() {
        let mut a = app();
        a.move_left();
        assert_eq!(a.cursor, 0);
        a.insert_char('x');
        a.move_right();
        assert_eq!(a.cursor, 1);
    }
}
