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
use std::path::Path;

/// Keeps a session's `heartbeat` column fresh for as long as this is held,
/// and gives up the claim when it's dropped.
///
/// Exists because an `activity` is a claim about a *live process* — "a
/// request is in flight", "somebody is being asked a question" — written by
/// a process that then has to survive long enough to take it back. A run
/// killed by an OOM, a reboot or a `kill -9` never does, and the row goes on
/// insisting it is working for ever. Nothing was watching a detached run, so
/// nothing corrected it either.
///
/// A ticking timestamp fixes that without needing to identify the process:
/// no PIDs (which get reused), no platform-specific liveness check. If the
/// stamp is fresh, someone is there; if it stopped, they aren't, whatever
/// their activity last claimed. See [`store::heartbeat_is_live`].
///
/// Ticks on its own task rather than from the turn loop, because the state
/// that most needs to be believed — waiting on an approval — is exactly the
/// one where the loop is blocked and doing nothing.
pub struct Heartbeat {
    conn: Connection,
    session_id: String,
    /// Identifies this claim, so renewing and releasing only ever touch a
    /// claim this process actually holds. Random per claim rather than a
    /// PID, which the OS reuses.
    owner: String,
    /// `None` when there is no Tokio runtime to tick on. The claim is still
    /// real — it just expires on its own instead of being renewed.
    ticker: Option<tokio::task::JoinHandle<()>>,
}

impl Heartbeat {
    /// Takes the session for this process, or reports that someone else has
    /// it. `Ok(None)` means a live claim is already held.
    ///
    /// The claim is a single conditional write, so it cannot be split into a
    /// check and a take. Only the renewal below needs a runtime, which is why
    /// the claim itself is attempted unconditionally: a test without a
    /// runtime still gets a real claim, and simply lets it lapse.
    pub fn claim(session_id: String) -> Result<Option<Self>> {
        let owner = uuid::Uuid::new_v4().to_string();
        let conn = store::open_db()?;
        if !store::claim_session(&conn, &session_id, &owner)? {
            return Ok(None);
        }

        // Its own handle: this writes on a timer, from a task, while the
        // caller's connection is busy with whatever the turn is doing.
        let ticker = if tokio::runtime::Handle::try_current().is_ok() {
            let ticking = store::open_db()?;
            let id = session_id.clone();
            let mine = owner.clone();
            Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(store::HEARTBEAT_INTERVAL);
                loop {
                    interval.tick().await;
                    match store::renew_session_claim(&ticking, &id, &mine) {
                        // Starved past the stale window, and the session has
                        // been taken by someone else. Stop renewing rather
                        // than stamping over a claim that is no longer ours.
                        Ok(false) => break,
                        Ok(true) => {}
                        // A failed write proves nothing; the next tick may
                        // well succeed, and the cost of being wrong for one
                        // interval is a row that briefly looks abandoned.
                        Err(_) => {}
                    }
                }
            }))
        } else {
            None
        };

        Ok(Some(Heartbeat {
            conn,
            session_id,
            owner,
            ticker,
        }))
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        if let Some(ticker) = &self.ticker {
            ticker.abort();
        }
        // Best-effort, and only an optimisation: it makes a clean exit
        // register at once instead of after the staleness window. The exits
        // this whole mechanism exists for never reach here at all. Scoped to
        // our own claim, so a late exit cannot release someone else's.
        let _ = store::release_session_claim(&self.conn, &self.session_id, &self.owner);
    }
}

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
    highlight: bool,
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
    /// Whether this session confines the agent's file writes to the working
    /// directory. Always concrete, like `approval` — a tool about to write
    /// needs a yes or no, not "defer to config".
    sandbox: bool,
    /// Whether this session streams replies token-by-token. Snapshotted from
    /// the configured default at creation, like `sandbox`.
    stream: bool,
    /// The directory this session was started in — the sandbox's boundary
    /// and what its relative paths resolve against. `None` for a session
    /// recorded before this was tracked.
    working_dir: Option<String>,
    messages: Vec<ChatMessage>,
    /// Whether the session has been given a title derived from a user
    /// message yet. Sessions start as "Untitled".
    title_set: bool,
    /// How many of `messages` have been written to the database. Everything
    /// from here on is pending; see [`ChatSession::persist_pending`].
    saved_len: usize,
}

/// What moving into a session's recorded directory did.
pub enum EnteredDir {
    /// The process moved into the session's directory.
    Moved(String),
    /// Already there, or the session recorded no directory — sessions
    /// written before that was tracked resume wherever they're run, as they
    /// always did.
    Unchanged,
    /// The recorded directory is gone. The caller decides what to do: the
    /// session's sandbox boundary can't be honoured, so continuing means
    /// running against whatever directory happens to be current.
    Missing(String),
}

/// Moves the process into `session`'s recorded working directory.
///
/// The directory is the sandbox's boundary and what the session's relative
/// paths resolve against, so a session resumed somewhere else is bounded by
/// wherever the shell happened to be — which is not what anyone means by
/// resuming it. Moving the process is what keeps the boundary a property of
/// the session rather than of the terminal.
pub fn enter_working_dir(session: &ChatSession) -> Result<EnteredDir> {
    let Some(recorded) = session.working_dir() else {
        return Ok(EnteredDir::Unchanged);
    };
    let recorded = recorded.to_string();
    if !Path::new(&recorded).is_dir() {
        return Ok(EnteredDir::Missing(recorded));
    }
    if std::env::current_dir().is_ok_and(|cwd| cwd == Path::new(&recorded)) {
        return Ok(EnteredDir::Unchanged);
    }
    std::env::set_current_dir(&recorded)?;
    Ok(EnteredDir::Moved(recorded))
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
        sandbox: bool,
        verbose: bool,
        highlight: bool,
        stream: bool,
        working_dir: Option<String>,
    ) -> Result<Self> {
        let id = store::create_session(
            &conn,
            &model,
            kind,
            effort_level.as_deref(),
            max_iterations.map(|n| n as i64),
            temperature.map(|n| n as f64),
            &approval,
            sandbox,
            verbose,
            highlight,
            stream,
            working_dir.as_deref(),
        )?;
        Ok(ChatSession {
            conn,
            id,
            title: "Untitled".to_string(),
            model,
            kind: kind.to_string(),
            effort_level,
            verbose,
            highlight,
            max_iterations,
            temperature,
            approval,
            sandbox,
            stream,
            working_dir,
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
                highlight: summary.highlight,
                max_iterations: summary.max_iterations.map(|n| n as usize),
                sandbox: summary.sandbox,
                stream: summary.stream,
                working_dir: summary.working_dir.clone(),
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
    /// Whether this session bands your own messages.
    pub fn highlight(&self) -> bool {
        self.highlight
    }

    /// Switches it and records it, so a resume comes back the same.
    pub fn set_highlight(&mut self, highlight: bool) -> Result<()> {
        if self.highlight == highlight {
            return Ok(());
        }
        store::set_session_highlight(&self.conn, &self.id, highlight)?;
        self.highlight = highlight;
        Ok(())
    }

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

    /// Switches whether the agent's file writes are confined to the working
    /// directory, and records it.
    pub fn set_sandbox(&mut self, sandbox: bool) -> Result<()> {
        if self.sandbox == sandbox {
            return Ok(());
        }
        store::set_session_sandbox(&self.conn, &self.id, sandbox)?;
        self.sandbox = sandbox;
        Ok(())
    }

    /// Whether this session confines the agent's file writes to the working
    /// directory.
    pub fn sandbox(&self) -> bool {
        self.sandbox
    }

    /// Switches whether this session streams replies, and records it.
    pub fn set_stream(&mut self, stream: bool) -> Result<()> {
        if self.stream == stream {
            return Ok(());
        }
        store::set_session_stream(&self.conn, &self.id, stream)?;
        self.stream = stream;
        Ok(())
    }

    /// Whether this session streams replies token-by-token.
    pub fn stream(&self) -> bool {
        self.stream
    }

    /// Says what this session's process is doing, for anything watching the
    /// list, or clears it with `None`. Best-effort: failing to announce a
    /// state is no reason to interrupt the turn producing it.
    pub fn set_activity(&self, activity: Option<store::Activity>, detail: Option<&str>) {
        let _ = store::set_session_activity(&self.conn, &self.id, activity, detail);
    }

    /// The directory this session was started in, if it recorded one.
    pub fn working_dir(&self) -> Option<&str> {
        self.working_dir.as_deref()
    }

    /// Repoints this session at `working_dir`, for a project that moved.
    pub fn set_working_dir(&mut self, working_dir: String) -> Result<()> {
        store::set_session_working_dir(&self.conn, &self.id, &working_dir)?;
        self.working_dir = Some(working_dir);
        Ok(())
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
        // All of the pending messages or none of them. Advancing
        // `saved_len` past one that failed to save used to drop it for
        // good, and saving the ones after it left a hole in `seq` — the
        // transcript then loaded cleanly with a turn missing from the
        // middle and nothing anywhere to say so. Silent gaps are worse
        // than a loud failure.
        //
        // Leaving `saved_len` alone on failure is what makes a retry work:
        // `cmd_agent` persists once before the turn and once after, so a
        // user message that could not be written the first time is written
        // by the second attempt, together with the reply it prompted,
        // rather than leaving an answer to a question nobody can see.
        let tx = self.conn.unchecked_transaction()?;

        for (seq, message) in self.messages.iter().enumerate().skip(self.saved_len) {
            store::append_message(
                &tx,
                &self.id,
                seq,
                message,
                &self.model,
                self.effort_level.as_deref(),
            )?;
        }

        // Derived from the first user message, so it belongs in the same
        // commit as the message that decides it.
        let title = (!self.title_set && self.messages.iter().any(|m| m.role == "user"))
            .then(|| store::derive_title(&self.messages));
        if let Some(title) = &title {
            store::set_session_title(&tx, &self.id, title)?;
        }

        tx.commit()?;

        // Only once the write is durable does the in-memory copy agree that
        // it happened.
        self.saved_len = self.messages.len();
        if let Some(title) = title {
            self.title = title;
            self.title_set = true;
        }
        Ok(())
    }

    /// Appends one message and writes it (plus anything else pending)
    /// immediately.
    pub fn push_and_persist(&mut self, message: ChatMessage) -> Result<()> {
        self.push(message);
        self.persist_pending()
    }

    /// Deletes the session if it was never named and nothing was ever said
    /// in it, reporting whether it did.
    ///
    /// A session row is created up front so messages have somewhere to go,
    /// which means opening a conversation and backing out without typing
    /// would leave an empty "Untitled" behind — clutter, now that the launch
    /// screen lists every session.
    ///
    /// Naming one is enough to keep it, though. Typing a title and
    /// confirming is a deliberate act: the session is something the user
    /// decided to start, whether or not they got as far as saying anything
    /// in it. Only backing out of the naming screen with a blank title, and
    /// then saying nothing, reads as "never mind".
    pub fn discard_if_unused(&self) -> Result<bool> {
        if self.title_set || self.messages.iter().any(|m| m.role == "user") {
            return Ok(false);
        }
        store::delete_session(&self.conn, &self.id)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn highlight_persists_so_a_later_resume_looks_the_same() {
        let mut session = memory_session();
        assert!(session.highlight(), "on unless the config said otherwise");

        session.set_highlight(false).unwrap();
        assert!(!session.highlight());

        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert!(!summary.highlight, "recorded, not just held in memory");
    }
    use super::*;
    use crate::store::KIND_CHAT;

    /// An in-memory database with the same schema `store::open_db` builds,
    /// so sessions can be exercised without touching the real one.
    fn memory_conn() -> Connection {
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
                highlight       INTEGER NOT NULL DEFAULT 1,
                max_iterations  INTEGER,
                temperature     REAL,
                approval_read      INTEGER NOT NULL DEFAULT 1,
                approval_write     INTEGER NOT NULL DEFAULT 1,
                approval_terminal  INTEGER NOT NULL DEFAULT 1,
                sandbox            INTEGER NOT NULL DEFAULT 1,
                stream             INTEGER NOT NULL DEFAULT 1,
                working_dir        TEXT,
                activity           TEXT,
                activity_detail    TEXT,
                heartbeat          INTEGER,
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
        conn
    }

    fn memory_session() -> ChatSession {
        ChatSession::create(
            memory_conn(),
            "test-model".to_string(),
            KIND_CHAT,
            Some("high".to_string()),
            Some(20),
            Some(0.7),
            ApprovalSettings::default(),
            true,
            false,
            true,
            true,
            None,
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

    fn session_in(dir: Option<&str>) -> ChatSession {
        ChatSession::create(
            memory_conn(),
            "test-model".to_string(),
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            ApprovalSettings::default(),
            true,
            false,
            true,
            true,
            dir.map(str::to_string),
        )
        .unwrap()
    }

    #[test]
    fn a_session_records_the_directory_it_started_in() {
        let dir = std::env::temp_dir().display().to_string();
        let session = session_in(Some(&dir));
        assert_eq!(session.working_dir(), Some(dir.as_str()));

        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.working_dir.as_deref(), Some(dir.as_str()));
    }

    #[test]
    fn a_session_without_a_recorded_directory_resumes_where_it_is() {
        // Rows written before this was tracked. Refusing to resume them, or
        // moving the process somewhere arbitrary, would break sessions that
        // worked yesterday.
        let session = session_in(None);
        assert!(matches!(
            enter_working_dir(&session).unwrap(),
            EnteredDir::Unchanged
        ));
    }

    #[test]
    fn a_missing_directory_is_reported_rather_than_ignored() {
        // The session's sandbox is anchored to a directory that isn't there,
        // so neither front end can honour it — and quietly rebinding the
        // bound to whatever is current is the one outcome worth refusing.
        let session = session_in(Some("/clank-no-such-directory-exists"));
        assert!(matches!(
            enter_working_dir(&session).unwrap(),
            EnteredDir::Missing(_)
        ));
        // Nothing moved.
        assert_ne!(
            std::env::current_dir().unwrap().display().to_string(),
            "/clank-no-such-directory-exists"
        );
    }

    #[test]
    fn repointing_a_session_records_the_new_directory() {
        let mut session = session_in(Some("/clank-no-such-directory-exists"));
        session.set_working_dir("/tmp".to_string()).unwrap();

        assert_eq!(session.working_dir(), Some("/tmp"));
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert_eq!(summary.working_dir.as_deref(), Some("/tmp"));
    }

    #[test]
    fn a_new_session_snapshots_the_configured_verbose_default() {
        // The configured default is a starting value, not a live one: it is
        // written into the session at creation, and `/verbose` from then on
        // changes the session rather than the configuration.
        let conn = memory_conn();
        let session = ChatSession::create(
            conn,
            "test-model".to_string(),
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            ApprovalSettings::default(),
            true,
            true,
            true,
            true,
            None,
        )
        .unwrap();

        assert!(session.verbose());
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        assert!(
            summary.verbose,
            "it has to survive a reload, not just live in memory"
        );
    }

#[test]
    fn a_failed_save_keeps_the_messages_pending_for_the_next_attempt() {
        // `cmd_agent` persists once before the turn and once after, and
        // carries on when the first one fails. That is only safe if the
        // failure left the messages pending: otherwise the question is
        // dropped and the transcript keeps the answer alone.
        let mut session = memory_session();
        session.push_user("the question".to_string());

        // Fault injection: with the table gone the append cannot succeed.
        session
            .conn
            .execute("ALTER TABLE messages RENAME TO messages_hidden", [])
            .unwrap();
        assert!(session.persist_pending().is_err(), "the save should fail");

        session
            .conn
            .execute("ALTER TABLE messages_hidden RENAME TO messages", [])
            .unwrap();
        session.push_assistant("the answer".to_string());
        session.persist_pending().expect("the retry should succeed");

        let stored = store::load_messages(&session.conn, session.id()).unwrap();
        let text: Vec<&str> = stored
            .iter()
            .filter_map(|m| m.message.content.as_deref())
            .collect();
        assert_eq!(
            text,
            vec!["the question", "the answer"],
            "the retry must save what the failed attempt did not"
        );
    }

    #[test]
    fn a_failed_save_writes_nothing_at_all() {
        // Partial success would leave a hole in `seq`, which reads back as a
        // transcript that is simply missing a turn.
        let mut session = memory_session();
        session.push_user("first".to_string());
        session.persist_pending().unwrap();

        session.push_user("second".to_string());
        session.push_assistant("third".to_string());
        session
            .conn
            .execute("ALTER TABLE messages RENAME TO messages_hidden", [])
            .unwrap();
        assert!(session.persist_pending().is_err());
        session
            .conn
            .execute("ALTER TABLE messages_hidden RENAME TO messages", [])
            .unwrap();

        let stored = store::load_messages(&session.conn, session.id()).unwrap();
        assert_eq!(stored.len(), 1, "only the message saved before the fault");
    }

    #[test]
    fn a_failed_save_does_not_claim_the_title_it_could_not_write() {
        let mut session = memory_session();
        session.push_user("name me from this".to_string());
        session
            .conn
            .execute("ALTER TABLE messages RENAME TO messages_hidden", [])
            .unwrap();
        assert!(session.persist_pending().is_err());
        assert_eq!(session.title(), "Untitled", "title followed a failed write");
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
    fn a_named_session_is_kept_even_with_nothing_said_in_it() {
        // Typing a title and confirming is a deliberate act — the session is
        // one the user decided to start, whether or not they got as far as
        // saying anything in it.
        let mut session = memory_session();
        session.set_title("Plan the migration".to_string()).unwrap();

        assert!(!session.discard_if_unused().unwrap());
        assert!(store::find_session(&session.conn, session.id())
            .unwrap()
            .is_some());
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
