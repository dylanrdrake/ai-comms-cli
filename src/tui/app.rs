//! TUI state and the rules for folding conversation events into it.
//!
//! Deliberately free of rendering and I/O: everything here is a plain state
//! transition, so the interesting behavior (how a stream becomes a transcript
//! block, what happens to input while busy) is testable without a terminal.

use crate::conversation::Event;
use crate::ui::{AgentEvent, ApprovalRequest};

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
    ToolCall {
        name: String,
        arguments: String,
        status: ToolStatus,
    },
    Error(String),
    Notice(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    AwaitingApproval,
    Running,
    Denied,
    Done { result: String },
}

/// What the input box does when Enter is pressed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    /// Normal typing; Enter sends or queues.
    Input,
    /// A tool is waiting on y/n; keystrokes answer it instead of typing.
    Approval,
}

/// What a submitted line turned out to be.
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
}

pub fn classify(text: &str) -> Submission {
    let trimmed = text.trim();
    match trimmed.strip_prefix("/model") {
        // "/models-are-great" is a message, not a malformed command.
        Some(rest) if rest.is_empty() => Submission::ShowModel,
        Some(rest) if rest.starts_with(char::is_whitespace) => {
            let name = rest.trim();
            if name.is_empty() {
                Submission::ShowModel
            } else {
                Submission::SetModel(name.to_string())
            }
        }
        _ => Submission::Message(text.to_string()),
    }
}

pub struct App {
    pub transcript: Vec<TranscriptItem>,
    pub input: String,
    /// Byte index of the cursor within `input`. Kept on a char boundary.
    pub cursor: usize,
    pub focus: Focus,
    pub busy: bool,
    pub queued: usize,
    pub pending_approval: Option<ApprovalRequest>,
    /// Lines scrolled up from the bottom. 0 means pinned to the newest
    /// content, which is where it stays unless the user scrolls back.
    pub scroll_back: u16,
    /// The model subsequent turns will use. Changes with `/model`.
    pub model: String,
    pub effort_level: Option<String>,
    pub session_id: String,
    /// Whether this conversation runs the tool loop, shown in the status bar
    /// so the mode is never ambiguous once you're inside a session.
    pub agentic: bool,
    /// Mirrors the plain CLI's `-v`: gates whether tool call arguments and
    /// results are shown, not just that a tool ran.
    pub verbose: bool,
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
            focus: Focus::Input,
            busy: false,
            queued: 0,
            pending_approval: None,
            scroll_back: 0,
            model,
            effort_level,
            session_id,
            agentic: false,
            verbose: false,
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
            Event::UserMessage(text) => self.transcript.push(TranscriptItem::User(text)),
            Event::Busy(busy) => {
                self.busy = busy;
                if !busy {
                    // A turn can end mid-stream (cancelled, or a failure
                    // after partial text); make sure nothing is left marked
                    // as still streaming.
                    self.finish_streaming();
                }
            }
            Event::Queued { pending } => self.queued = pending,
            Event::Cancelled => {
                self.finish_streaming();
                self.queued = 0;
                self.pending_approval = None;
                self.focus = Focus::Input;
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
                self.focus = Focus::Approval;
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
                self.focus = Focus::Input;
            }
            AgentEvent::ToolCallCompleted { result, .. } => {
                self.set_last_tool_status(ToolStatus::Done { result });
                self.pending_approval = None;
                self.focus = Focus::Input;
            }
            AgentEvent::Error { message } => {
                self.finish_streaming();
                self.transcript.push(TranscriptItem::Error(message));
            }
            // Busy state is driven by Event::Busy, which brackets the whole
            // turn rather than each request within it.
            AgentEvent::RequestStarted
            | AgentEvent::RequestFinished
            | AgentEvent::IterationStarted { .. }
            | AgentEvent::TurnFinished => {}
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
        self.focus = Focus::Input;
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
    /// Ignored while an approval prompt has focus, like ordinary typing.
    pub fn paste(&mut self, text: &str) {
        if self.focus != Focus::Input {
            return;
        }
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
    pub fn take_approval_answer(&mut self) -> String {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.history_cursor = None;
        self.draft.clear();
        text
    }

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
        assert_eq!(a.focus, Focus::Input);
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
        assert_eq!(a.focus, Focus::Approval);

        a.apply(Event::Agent(AgentEvent::ToolCallCompleted {
            name: "write_file".into(),
            result: r#"{"success":true}"#.into(),
        }));
        assert_eq!(a.focus, Focus::Input);
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
        assert_eq!(a.focus, Focus::Input);
        assert_eq!(a.queued, 0);
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

    // --- /model command -------------------------------------------------

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
    fn take_approval_answer_clears_input_without_touching_history() {
        let mut a = app();
        for c in "yes".chars() {
            a.insert_char(c);
        }
        assert_eq!(a.take_approval_answer(), "yes");
        assert!(a.input.is_empty());
        assert_eq!(a.cursor, 0);
        assert!(a.input_history.is_empty());
    }

    #[test]
    fn take_approval_answer_returns_a_blank_answer_rather_than_none() {
        let mut a = app();
        assert_eq!(a.take_approval_answer(), "");
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
    fn paste_is_ignored_while_an_approval_prompt_has_focus() {
        let mut a = app();
        a.focus = Focus::Approval;
        a.paste("sneaky\nmessage");
        assert_eq!(a.input, "");
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
