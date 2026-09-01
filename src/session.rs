//! Conversation state and its persistence, independent of any front end.
//!
//! A [`ChatSession`] owns the message history for one `chat`/`agent-chat`
//! session plus the database handle backing it, and knows how to create,
//! resume, and durably record turns. It does no I/O with the user and holds
//! no opinion about how a conversation is displayed or driven, so the CLI
//! loops and any future GUI can share the same bookkeeping instead of each
//! reimplementing "append, persist, name the session".

use crate::client::ChatMessage;
use crate::config::ApprovalSettings;
use crate::store::{self, SessionSummary, StoredMessage, KIND_AGENT_CHAT, KIND_CHAT};
use anyhow::Result;
use rusqlite::Connection;

pub struct ChatSession {
    conn: Connection,
    id: String,
    /// The session's current title. "Untitled" until [`Self::persist_pending`]
    /// derives a real one from the first user message.
    title: String,
    model: String,
    /// `KIND_CHAT` or `KIND_AGENT_CHAT` — mutable via [`Self::set_agentic`],
    /// unlike `store::create_session`'s `kind` param, which only sets the
    /// starting mode.
    kind: String,
    effort_level: Option<String>,
    /// Whether this session's TUI view currently shows verbose tool detail.
    /// Purely a display setting — the agent loop never reads it.
    verbose: bool,
    /// This session's tool-calling iteration cap per turn, while in agent
    /// mode. Starts as a snapshot of the configured default (merged with any
    /// `--max-iterations` given at creation), mutable from inside it with
    /// `/max-iterations <n>`. `/max-iterations clear` nullifies it back to
    /// `None` — turns then fall back to whatever the configured default is
    /// *at the time each one runs*, not frozen to what it was at creation or
    /// at the last explicit set. `/max-iterations default` is the concrete
    /// counterpart: it reads the currently configured default once and
    /// saves that as a new explicit `Some` value, same as typing the number
    /// itself.
    max_iterations: Option<usize>,
    /// This session's sampling temperature — same deal as `max_iterations`.
    temperature: Option<f32>,
    /// This session's current tool-approval gates, always concrete (unlike
    /// `effort_level`/`max_iterations`/`temperature` there's no "unset,
    /// defer to config" state once a turn actually needs to check them).
    approval: ApprovalSettings,
    messages: Vec<ChatMessage>,
    /// Whether the session has been given a title derived from a user
    /// message yet. Sessions start as "Untitled".
    title_set: bool,
    /// How many of `messages` have been written to the database. Everything
    /// from here on is pending; see [`ChatSession::persist_pending`].
    saved_len: usize,
}

impl ChatSession {
    /// Starts a new session, registering it in the database. `effort_level`,
    /// `max_iterations`, and `temperature` are each a snapshot of the
    /// configured default at creation time (already merged with any
    /// `--flag` the caller was given) — like `approval`, they're written
    /// immediately rather than re-resolved on every resume. Any of the three
    /// can already be `None` here, if nothing is configured anywhere; that's
    /// still a real snapshot, not "unset by omission".
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        conn: Connection,
        model: String,
        kind: &str,
        effort_level: Option<String>,
        max_iterations: Option<usize>,
        temperature: Option<f32>,
        approval: ApprovalSettings,
    ) -> Result<Self> {
        let id = store::create_session(
            &conn,
            &model,
            kind,
            effort_level.as_deref(),
            max_iterations.map(|n| n as i64),
            temperature.map(|n| n as f64),
            &approval,
        )?;
        Ok(ChatSession {
            conn,
            id,
            title: "Untitled".to_string(),
            model,
            kind: kind.to_string(),
            effort_level,
            verbose: false,
            max_iterations,
            temperature,
            approval,
            messages: Vec::new(),
            title_set: false,
            saved_len: 0,
        })
    }

    /// Reopens a saved session. `model` overrides the model it was created
    /// with (a `--model` flag on resume); pass `summary.model` to keep it.
    /// Every other setting comes straight off `summary`, the session's own
    /// persisted value; there's no config fallback to re-resolve here — a
    /// `None` `max_iterations`/`temperature` stays `None` (nullified, or a
    /// row written before this was tracked; either way a caller resolves it
    /// against the configured default per turn, not here).
    ///
    /// Returns the stored history alongside the session so a caller can
    /// render the prior transcript — including the per-message model/effort
    /// each turn was produced with, which the in-memory history drops.
    pub fn resume(
        conn: Connection,
        summary: &SessionSummary,
        model: String,
    ) -> Result<(Self, Vec<StoredMessage>)> {
        let history = store::load_messages(&conn, &summary.id)?;
        let messages: Vec<ChatMessage> = history.iter().map(|sm| sm.message.clone()).collect();
        let title_set = messages.iter().any(|m| m.role == "user");
        let saved_len = messages.len();

        // Resuming with a different model (a `--model` flag) is a real
        // switch, not a one-off override: record it so the session reports
        // what it's actually using and doesn't silently revert next time.
        if model != summary.model {
            store::set_session_model(&conn, &summary.id, &model)?;
        }

        Ok((
            ChatSession {
                conn,
                id: summary.id.clone(),
                title: summary.title.clone(),
                model,
                kind: summary.kind.clone(),
                effort_level: summary.effort_level.clone(),
                verbose: summary.verbose,
                max_iterations: summary.max_iterations.map(|n| n as usize),
                temperature: summary.temperature.map(|n| n as f32),
                approval: summary.approval.clone(),
                messages,
                title_set,
                saved_len,
            },
            history,
        ))
    }

    /// Switches the model for subsequent turns and records it. Messages
    /// already sent keep the model they were produced with, since each row
    /// carries its own.
    pub fn set_model(&mut self, model: String) -> Result<()> {
        if self.model == model {
            return Ok(());
        }
        store::set_session_model(&self.conn, &self.id, &model)?;
        self.model = model;
        Ok(())
    }

    /// Switches between plain and agent (tool-calling) mode and records it,
    /// so the switch sticks on resume and in `sessions list`. Doesn't touch
    /// message history either way: the agent system prompt isn't stored —
    /// some providers (Anthropic among them) require a `system`-role
    /// message to sit at the very start of the conversation, and `/agent`
    /// can flip this on at any point mid-conversation, so there's no
    /// position here that's guaranteed to stay valid as the conversation
    /// grows around it. `agent::request_turn` prepends it fresh on every
    /// turn that actually needs it instead.
    pub fn set_agentic(&mut self, agentic: bool) -> Result<()> {
        let kind = if agentic { KIND_AGENT_CHAT } else { KIND_CHAT };
        if self.kind == kind {
            return Ok(());
        }
        store::set_session_kind(&self.conn, &self.id, kind)?;
        self.kind = kind.to_string();
        Ok(())
    }

    /// Whether this session is currently in agent (tool-calling) mode.
    pub fn is_agentic(&self) -> bool {
        self.kind == KIND_AGENT_CHAT
    }

    /// Switches the reasoning effort for subsequent turns and records it.
    /// `None` clears the override, falling back to whatever the configured
    /// default is next time the session is opened.
    pub fn set_effort_level(&mut self, effort_level: Option<String>) -> Result<()> {
        if self.effort_level == effort_level {
            return Ok(());
        }
        store::set_session_effort_level(&self.conn, &self.id, effort_level.as_deref())?;
        self.effort_level = effort_level;
        Ok(())
    }

    /// Toggles whether this session's TUI view shows verbose tool detail,
    /// and records it so it's remembered on resume.
    pub fn set_verbose(&mut self, verbose: bool) -> Result<()> {
        if self.verbose == verbose {
            return Ok(());
        }
        store::set_session_verbose(&self.conn, &self.id, verbose)?;
        self.verbose = verbose;
        Ok(())
    }

    /// Whether this session's TUI view currently shows verbose tool detail.
    pub fn verbose(&self) -> bool {
        self.verbose
    }

    /// Switches the tool-calling iteration cap per turn (agent mode only)
    /// and records it. `None` nullifies it (`/max-iterations clear`) — a
    /// turn then falls back to whatever the configured default is when it
    /// actually runs, not to anything frozen here.
    pub fn set_max_iterations(&mut self, max_iterations: Option<usize>) -> Result<()> {
        if self.max_iterations == max_iterations {
            return Ok(());
        }
        store::set_session_max_iterations(&self.conn, &self.id, max_iterations.map(|n| n as i64))?;
        self.max_iterations = max_iterations;
        Ok(())
    }

    /// This session's `/max-iterations` override, if one is set.
    pub fn max_iterations(&self) -> Option<usize> {
        self.max_iterations
    }

    /// Switches the sampling temperature for subsequent turns and records
    /// it. `None` nullifies it, same deal as [`Self::set_max_iterations`].
    pub fn set_temperature(&mut self, temperature: Option<f32>) -> Result<()> {
        if self.temperature == temperature {
            return Ok(());
        }
        store::set_session_temperature(&self.conn, &self.id, temperature.map(|n| n as f64))?;
        self.temperature = temperature;
        Ok(())
    }

    /// This session's `/temperature` override, if one is set.
    pub fn temperature(&self) -> Option<f32> {
        self.temperature
    }

    /// Switches this session's tool-approval gates and records them.
    pub fn set_approval(&mut self, approval: ApprovalSettings) -> Result<()> {
        if self.approval == approval {
            return Ok(());
        }
        store::set_session_approval(&self.conn, &self.id, &approval)?;
        self.approval = approval;
        Ok(())
    }

    /// This session's current tool-approval gates.
    pub fn approval(&self) -> &ApprovalSettings {
        &self.approval
    }

    /// The session's current title — "Untitled" until the first user
    /// message names it.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Sets the title explicitly — naming a new session up front, or
    /// renaming an existing one. Marks it as no longer eligible for the
    /// usual derive-from-first-message step, so a later `persist_pending`
    /// won't silently overwrite a name that was actually chosen.
    pub fn set_title(&mut self, title: String) -> Result<()> {
        store::set_session_title(&self.conn, &self.id, &title)?;
        self.title = title;
        self.title_set = true;
        Ok(())
    }

    /// The full session id. The CLI mostly shows [`ChatSession::short_id`],
    /// but the whole id is what a caller needs to address a session later.
    #[allow(dead_code)]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The first 8 characters of the id — what `--resume` and `sessions
    /// list` show, since any unique prefix resolves.
    pub fn short_id(&self) -> &str {
        &self.id[..8]
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// The effort level new turns are recorded with. Callers that already
    /// hold the config value don't need this; one handed a session alone
    /// does.
    pub fn effort_level(&self) -> Option<&str> {
        self.effort_level.as_deref()
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// The history as the agent loop wants it: a `&mut Vec` it can append
    /// assistant and tool turns to. Anything added this way is pending until
    /// [`ChatSession::persist_pending`] runs.
    pub fn messages_mut(&mut self) -> &mut Vec<ChatMessage> {
        &mut self.messages
    }

    /// Appends a message in memory only. Use when several turns will be
    /// written together (an agent turn produces assistant + tool messages).
    pub fn push(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }

    pub fn push_user(&mut self, text: String) {
        self.push(ChatMessage {
            role: "user".to_string(),
            content: Some(text),
            tool_calls: None,
            tool_call_id: None,
            ..Default::default()
        });
    }

    /// The counterpart to [`ChatSession::push_user`]. The agent loop appends
    /// assistant turns itself through `messages_mut`, so this is for callers
    /// driving a conversation directly.
    #[allow(dead_code)]
    pub fn push_assistant(&mut self, text: String) {
        self.push(ChatMessage {
            role: "assistant".to_string(),
            content: Some(text),
            tool_calls: None,
            tool_call_id: None,
            ..Default::default()
        });
    }

    /// Writes every message added since the last call, tagging each with the
    /// model and effort level in force now, and names the session from its
    /// first user message if it doesn't have a title yet.
    ///
    /// Attempts every pending message even if one fails, so a single bad
    /// write doesn't silently drop the rest of a turn; the first error is
    /// returned once the rest have been tried.
    pub fn persist_pending(&mut self) -> Result<()> {
        let mut first_error = None;

        for (seq, message) in self.messages.iter().enumerate().skip(self.saved_len) {
            if let Err(e) = store::append_message(
                &self.conn,
                &self.id,
                seq,
                message,
                &self.model,
                self.effort_level.as_deref(),
            ) {
                first_error.get_or_insert(e);
            }
        }
        self.saved_len = self.messages.len();

        if !self.title_set && self.messages.iter().any(|m| m.role == "user") {
            let title = store::derive_title(&self.messages);
            match store::set_session_title(&self.conn, &self.id, &title) {
                Ok(()) => {
                    self.title = title;
                    self.title_set = true;
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }

        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Appends one message and writes it (plus anything else pending)
    /// immediately.
    pub fn push_and_persist(&mut self, message: ChatMessage) -> Result<()> {
        self.push(message);
        self.persist_pending()
    }

    /// Deletes the session if nothing was ever said in it, reporting whether
    /// it did.
    ///
    /// A session row is created up front so messages have somewhere to go,
    /// which means opening a conversation and backing out without typing
    /// leaves an empty "Untitled" behind. Harmless when sessions are only
    /// listed on demand, but clutter once a launch screen shows recent ones.
    /// A resumed session is never empty, so this only ever discards a
    /// genuinely unused one.
    pub fn discard_if_unused(&self) -> Result<bool> {
        if self.messages.iter().any(|m| m.role == "user") {
            return Ok(false);
        }
        store::delete_session(&self.conn, &self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::KIND_CHAT;

    /// An in-memory database with the same schema `store::open_db` builds,
    /// so sessions can be exercised without touching the real one.
    fn memory_session() -> ChatSession {
        crate::crypto::seed_test_key();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE sessions (
                id              TEXT PRIMARY KEY,
                title           TEXT NOT NULL,
                model           TEXT NOT NULL,
                kind            TEXT NOT NULL,
                effort_level    TEXT,
                verbose         INTEGER NOT NULL DEFAULT 0,
                max_iterations  INTEGER,
                temperature     REAL,
                approval_read      INTEGER NOT NULL DEFAULT 1,
                approval_write     INTEGER NOT NULL DEFAULT 1,
                approval_terminal  INTEGER NOT NULL DEFAULT 1,
                created_at      INTEGER NOT NULL,
                updated_at      INTEGER NOT NULL
            );
            CREATE TABLE messages (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq               INTEGER NOT NULL,
                role              TEXT NOT NULL,
                content           TEXT,
                tool_calls        TEXT,
                tool_call_id      TEXT,
                model             TEXT,
                effort_level      TEXT,
                reasoning_details TEXT,
                reasoning         TEXT
            );
            ",
        )
        .unwrap();
        ChatSession::create(
            conn,
            "test-model".to_string(),
            KIND_CHAT,
            Some("high".to_string()),
            Some(20),
            Some(0.7),
            ApprovalSettings::default(),
        )
        .unwrap()
    }

    #[test]
    fn persists_pending_messages_once() {
        let mut session = memory_session();
        session.push_user("hello".to_string());
        session.push_assistant("hi there".to_string());
        session.persist_pending().unwrap();

        let stored = store::load_messages(&session.conn, session.id()).unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].message.role, "user");
        assert_eq!(stored[1].message.content, Some("hi there".to_string()));

        // A second call must not duplicate anything already written.
        session.persist_pending().unwrap();
        let stored = store::load_messages(&session.conn, session.id()).unwrap();
        assert_eq!(stored.len(), 2);
    }

    #[test]
    fn tags_each_message_with_model_and_effort() {
        let mut session = memory_session();
        session.push_user("hello".to_string());
        session.persist_pending().unwrap();

        let stored = store::load_messages(&session.conn, session.id()).unwrap();
        assert_eq!(stored[0].model.as_deref(), Some("test-model"));
        assert_eq!(stored[0].effort_level.as_deref(), Some("high"));
    }

    #[test]
    fn titles_session_from_first_user_message() {
        let mut session = memory_session();
        session.push(ChatMessage {
            role: "system".to_string(),
            content: Some("system prompt".to_string()),
            tool_calls: None,
            tool_call_id: None,
            ..Default::default()
        });
        session.persist_pending().unwrap();
        // A system-only session has nothing to name itself after yet.
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.title, "Untitled");

        session.push_user("Write me a snake game".to_string());
        session.persist_pending().unwrap();
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.title, "Write me a snake game");
    }

    #[test]
    fn title_is_not_rewritten_by_later_turns() {
        let mut session = memory_session();
        session.push_user("first question".to_string());
        session.persist_pending().unwrap();
        session.push_user("second question".to_string());
        session.persist_pending().unwrap();

        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.title, "first question");
    }

    #[test]
    fn set_model_persists_so_a_later_resume_picks_it_up() {
        let mut session = memory_session();
        session.push_user("hello".to_string());
        session.persist_pending().unwrap();

        session.set_model("second-model".to_string()).unwrap();
        assert_eq!(session.model(), "second-model");

        // The sessions row must reflect it, not just the in-memory session.
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.model, "second-model");
    }

    #[test]
    fn set_agentic_persists_the_kind_without_touching_history() {
        let mut session = memory_session();
        assert!(!session.is_agentic());
        session.push_user("hello".to_string());
        session.persist_pending().unwrap();
        let messages_before = session.messages().len();

        session.set_agentic(true).unwrap();
        assert!(session.is_agentic());
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.kind, crate::store::KIND_AGENT_CHAT);
        // No system prompt gets stored — a provider-valid position for one
        // can't be guaranteed here, since `/agent` can flip this on at any
        // point mid-conversation; `agent::request_turn` prepends it fresh
        // on each turn that actually needs it instead.
        assert_eq!(session.messages().len(), messages_before);
    }

    #[test]
    fn set_agentic_off_leaves_prior_history_alone() {
        let mut session = memory_session();
        session.set_agentic(true).unwrap();
        session.push_user("hello".to_string());
        session.persist_pending().unwrap();
        let messages_before = session.messages().len();

        session.set_agentic(false).unwrap();
        assert!(!session.is_agentic());
        assert_eq!(session.messages().len(), messages_before);

        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.kind, crate::store::KIND_CHAT);
    }

    #[test]
    fn set_effort_level_persists_and_clears() {
        let mut session = memory_session();
        assert_eq!(session.effort_level(), Some("high"));

        session.set_effort_level(Some("low".to_string())).unwrap();
        assert_eq!(session.effort_level(), Some("low"));
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.effort_level, Some("low".to_string()));

        session.set_effort_level(None).unwrap();
        assert_eq!(session.effort_level(), None);
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.effort_level, None);
    }

    #[test]
    fn set_verbose_persists_so_a_later_resume_picks_it_up() {
        let mut session = memory_session();
        assert!(!session.verbose());

        session.set_verbose(true).unwrap();
        assert!(session.verbose());
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert!(summary.verbose);
    }

    #[test]
    fn set_max_iterations_persists_and_clears() {
        let mut session = memory_session();
        // The 20 `memory_session` created it with — a snapshot, not "unset".
        assert_eq!(session.max_iterations(), Some(20));

        session.set_max_iterations(Some(30)).unwrap();
        assert_eq!(session.max_iterations(), Some(30));
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.max_iterations, Some(30));

        // Nullifying is a session-layer concept again — a caller resolves
        // `/max-iterations default` to a concrete value itself, same as any
        // other explicit number, but `clear` passes `None` straight through.
        session.set_max_iterations(None).unwrap();
        assert_eq!(session.max_iterations(), None);
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.max_iterations, None);
    }

    #[test]
    fn set_temperature_persists_and_clears() {
        let mut session = memory_session();
        assert_eq!(session.temperature(), Some(0.7));

        session.set_temperature(Some(1.5)).unwrap();
        assert_eq!(session.temperature(), Some(1.5));
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.temperature, Some(1.5));

        session.set_temperature(None).unwrap();
        assert_eq!(session.temperature(), None);
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.temperature, None);
    }

    #[test]
    fn set_approval_persists_and_updates_the_summary() {
        let mut session = memory_session();
        assert_eq!(*session.approval(), ApprovalSettings::default());

        let custom = ApprovalSettings {
            read_disk: true,
            write_disk: false,
            terminal: false,
        };
        session.set_approval(custom.clone()).unwrap();
        assert_eq!(*session.approval(), custom);
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.approval, custom);
    }

    #[test]
    fn title_tracks_live_once_derived_from_the_first_message() {
        let mut session = memory_session();
        assert_eq!(session.title(), "Untitled");

        session.push_user("Write me a snake game".to_string());
        session.persist_pending().unwrap();
        assert_eq!(session.title(), "Write me a snake game");
    }

    #[test]
    fn set_title_persists_and_blocks_later_auto_derivation() {
        let mut session = memory_session();
        session.set_title("My chosen title".to_string()).unwrap();
        assert_eq!(session.title(), "My chosen title");
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.title, "My chosen title");

        // A later user message must not clobber the chosen title.
        session.push_user("hello".to_string());
        session.persist_pending().unwrap();
        assert_eq!(session.title(), "My chosen title");
    }

    #[test]
    fn resume_picks_up_the_persisted_title_verbose_max_iterations_and_temperature() {
        let mut session = memory_session();
        session.push_user("hello".to_string());
        session.persist_pending().unwrap();
        session.set_verbose(true).unwrap();
        session.set_max_iterations(Some(30)).unwrap();
        session.set_temperature(Some(1.5)).unwrap();

        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        let ChatSession { conn, .. } = session;

        let (resumed, _) = ChatSession::resume(conn, &summary, summary.model.clone()).unwrap();
        assert_eq!(resumed.title(), "hello");
        assert!(resumed.verbose());
        assert_eq!(resumed.max_iterations(), Some(30));
        assert_eq!(resumed.temperature(), Some(1.5));
    }

    #[test]
    fn resuming_with_a_different_model_records_the_switch() {
        let mut session = memory_session();
        session.push_user("hello".to_string());
        session.persist_pending().unwrap();
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        let ChatSession { conn, .. } = session;

        let (resumed, _) =
            ChatSession::resume(conn, &summary, "switched-model".to_string()).unwrap();
        assert_eq!(resumed.model(), "switched-model");

        // And resuming again with no override keeps the switched model
        // rather than reverting to the original.
        let summary = store::find_session(&resumed.conn, resumed.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.model, "switched-model");
    }

    #[test]
    fn an_unused_session_is_discarded() {
        let session = memory_session();
        assert!(session.discard_if_unused().unwrap());
        assert!(store::find_session(&session.conn, session.id())
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_session_with_only_a_system_prompt_still_counts_as_unused() {
        let mut session = memory_session();
        session
            .push_and_persist(ChatMessage {
                role: "system".to_string(),
                content: Some("system prompt".to_string()),
                tool_calls: None,
                tool_call_id: None,
                ..Default::default()
            })
            .unwrap();
        assert!(session.discard_if_unused().unwrap());
    }

    #[test]
    fn a_used_session_is_kept() {
        let mut session = memory_session();
        session.push_user("hello".to_string());
        session.persist_pending().unwrap();
        assert!(!session.discard_if_unused().unwrap());
        assert!(store::find_session(&session.conn, session.id())
            .unwrap()
            .is_some());
    }

    #[test]
    fn resume_restores_history_and_keeps_appending_in_order() {
        let mut session = memory_session();
        session.push_user("hello".to_string());
        session.push_assistant("hi there".to_string());
        session.persist_pending().unwrap();

        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        let ChatSession { conn, .. } = session;

        let (mut resumed, history) =
            ChatSession::resume(conn, &summary, summary.model.clone()).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(resumed.messages().len(), 2);

        // Resuming must not re-write the history it just loaded.
        resumed.push_user("follow up".to_string());
        resumed.persist_pending().unwrap();
        let stored = store::load_messages(&resumed.conn, resumed.id()).unwrap();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[2].message.content, Some("follow up".to_string()));
    }
}
