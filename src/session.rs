//! Conversation state and its persistence, independent of any front end.
//!
//! A [`ChatSession`] owns the message history for one `chat`/`agent-chat`
//! session plus the database handle backing it, and knows how to create,
//! resume, and durably record turns. It does no I/O with the user and holds
//! no opinion about how a conversation is displayed or driven, so the CLI
//! loops and any future GUI can share the same bookkeeping instead of each
//! reimplementing "append, persist, name the session".

use crate::client::ChatMessage;
use crate::store::{self, SessionSummary, StoredMessage};
use anyhow::Result;
use rusqlite::Connection;

pub struct ChatSession {
    conn: Connection,
    id: String,
    model: String,
    effort_level: Option<String>,
    messages: Vec<ChatMessage>,
    /// Whether the session has been given a title derived from a user
    /// message yet. Sessions start as "Untitled".
    title_set: bool,
    /// How many of `messages` have been written to the database. Everything
    /// from here on is pending; see [`ChatSession::persist_pending`].
    saved_len: usize,
}

impl ChatSession {
    /// Starts a new session, registering it in the database.
    pub fn create(
        conn: Connection,
        model: String,
        kind: &str,
        effort_level: Option<String>,
    ) -> Result<Self> {
        let id = store::create_session(&conn, &model, kind)?;
        Ok(ChatSession {
            conn,
            id,
            model,
            effort_level,
            messages: Vec::new(),
            title_set: false,
            saved_len: 0,
        })
    }

    /// Reopens a saved session. `model` overrides the model it was created
    /// with (a `--model` flag on resume); pass `summary.model` to keep it.
    ///
    /// Returns the stored history alongside the session so a caller can
    /// render the prior transcript — including the per-message model/effort
    /// each turn was produced with, which the in-memory history drops.
    pub fn resume(
        conn: Connection,
        summary: &SessionSummary,
        model: String,
        effort_level: Option<String>,
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
                model,
                effort_level,
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
    #[allow(dead_code)]
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
                Ok(()) => self.title_set = true,
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
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE sessions (
                id          TEXT PRIMARY KEY,
                title       TEXT NOT NULL,
                model       TEXT NOT NULL,
                kind        TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );
            CREATE TABLE messages (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq           INTEGER NOT NULL,
                role          TEXT NOT NULL,
                content       TEXT,
                tool_calls    TEXT,
                tool_call_id  TEXT,
                model         TEXT,
                effort_level  TEXT
            );
            ",
        )
        .unwrap();
        ChatSession::create(
            conn,
            "test-model".to_string(),
            KIND_CHAT,
            Some("high".to_string()),
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
    fn resuming_with_a_different_model_records_the_switch() {
        let mut session = memory_session();
        session.push_user("hello".to_string());
        session.persist_pending().unwrap();
        let summary = store::find_session(&session.conn, session.id())
            .unwrap()
            .unwrap();
        let ChatSession { conn, .. } = session;

        let (resumed, _) =
            ChatSession::resume(conn, &summary, "switched-model".to_string(), None).unwrap();
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
            ChatSession::resume(conn, &summary, summary.model.clone(), None).unwrap();
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
