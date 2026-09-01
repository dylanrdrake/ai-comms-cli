use crate::client::{ChatMessage, ToolCall};
use crate::config::{get_config_dir, ApprovalSettings};
use crate::crypto;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};

/// Kind of session, used to distinguish plain chat from agentic chat when listing.
pub const KIND_CHAT: &str = "chat";
pub const KIND_AGENT_CHAT: &str = "agent_chat";

/// A message as loaded from history, along with the model and effort level
/// that were active when it was recorded (both `None` for rows written
/// before this tracking was added).
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub message: ChatMessage,
    pub model: Option<String>,
    pub effort_level: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub kind: String,
    /// The reasoning effort this session is currently set to — a snapshot of
    /// the configured default taken when it was created, mutable from inside
    /// it with `/effort`. `None` is itself a real value (no effort field
    /// sent), not "unset".
    pub effort_level: Option<String>,
    /// Whether this session's TUI view currently shows verbose tool detail.
    pub verbose: bool,
    /// The max tool-calling iterations per turn this session is currently
    /// set to. Starts as a snapshot of the configured default taken when it
    /// was created, mutable from inside it with `/max-iterations <n>`.
    /// `None` means nullified (`/max-iterations clear`, or a row written
    /// before this was tracked) — a caller resolves it against the
    /// configured default per turn, not here.
    pub max_iterations: Option<i64>,
    /// The sampling temperature this session is currently set to — same
    /// deal as `max_iterations`.
    pub temperature: Option<f64>,
    /// This session's current tool-approval settings — a snapshot of the
    /// configured default taken when it was created, mutable from inside
    /// it with `/approval`.
    pub approval: ApprovalSettings,
    /// Whether this session confines file writes to the working directory
    /// and home — a snapshot of the configured default taken when it was
    /// created, mutable from inside it with `/sandbox`.
    pub sandbox: bool,
    /// Not surfaced by the CLI, but kept for sorting and display.
    #[allow(dead_code)]
    pub created_at: i64,
    /// Drives "12m ago" in the TUI's session lists.
    pub updated_at: i64,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Opens (creating if needed) the sessions database under the config dir and
/// ensures the schema exists.
pub fn open_db() -> Result<Connection> {
    let path = get_config_dir()?.join("chats.db");
    let conn = Connection::open(path)?;

    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS sessions (
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
            sandbox            INTEGER NOT NULL DEFAULT 1,
            created_at      INTEGER NOT NULL,
            updated_at      INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS messages (
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

        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
        ",
    )?;

    // `model`/`effort_level` were added after messages already shipped without
    // them; back them onto any database created before this change.
    ensure_column(&conn, "messages", "model", "TEXT")?;
    ensure_column(&conn, "messages", "effort_level", "TEXT")?;
    // A reasoning model's own thinking blocks, needed to keep a follow-up
    // request valid when a tool-calling turn continues past one — see
    // `ChatMessage::reasoning_details`.
    ensure_column(&conn, "messages", "reasoning_details", "TEXT")?;
    // The same thinking as prose, kept only to show back to the user under
    // `/verbose` — never resent, unlike the blocks above.
    ensure_column(&conn, "messages", "reasoning", "TEXT")?;

    // Likewise for sessions gaining per-session effort/verbose/max-iterations
    // overrides, so those can be switched mid-conversation and remembered.
    ensure_column(&conn, "sessions", "effort_level", "TEXT")?;
    ensure_column(&conn, "sessions", "verbose", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "sessions", "max_iterations", "INTEGER")?;
    ensure_column(&conn, "sessions", "temperature", "REAL")?;
    ensure_column(
        &conn,
        "sessions",
        "approval_read",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        &conn,
        "sessions",
        "approval_write",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        &conn,
        "sessions",
        "approval_terminal",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    // Where the file-writing tools may write, per session — see
    // `Config::sandbox`. Defaults on, so a session written before this
    // existed comes back confined rather than unbounded.
    ensure_column(&conn, "sessions", "sandbox", "INTEGER NOT NULL DEFAULT 1")?;

    Ok(conn)
}

/// Adds `column` to `table` if it isn't already there. Used to migrate
/// databases created before a column existed, without disturbing existing
/// rows (new column comes back `NULL` for them).
fn ensure_column(conn: &Connection, table: &str, column: &str, sql_type: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|name| name.ok())
        .any(|name| name == column);

    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {sql_type}"),
            [],
        )?;
    }

    Ok(())
}

/// Derives a short title from the first user message, falling back to "Untitled".
pub fn derive_title(messages: &[ChatMessage]) -> String {
    let first_user = messages
        .iter()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_deref())
        .unwrap_or("");

    let trimmed = first_user.trim();
    if trimmed.is_empty() {
        return "Untitled".to_string();
    }

    let mut title: String = trimmed.chars().take(60).collect();
    if trimmed.chars().count() > 60 {
        title.push_str("...");
    }
    title
}

/// Creates a new session row and returns its id.
#[allow(clippy::too_many_arguments)]
pub fn create_session(
    conn: &Connection,
    model: &str,
    kind: &str,
    effort_level: Option<&str>,
    max_iterations: Option<i64>,
    temperature: Option<f64>,
    approval: &ApprovalSettings,
    sandbox: bool,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    let title = crypto::encrypt("Untitled")?;

    conn.execute(
        "INSERT INTO sessions (id, title, model, kind, effort_level, max_iterations, temperature, approval_read, approval_write, approval_terminal, sandbox, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
        params![
            id,
            title,
            model,
            kind,
            effort_level,
            max_iterations,
            temperature,
            approval.read_disk,
            approval.write_disk,
            approval.terminal,
            sandbox,
            ts
        ],
    )?;

    Ok(id)
}

/// Updates the session's title (e.g. once the first user message is known).
pub fn set_session_title(conn: &Connection, session_id: &str, title: &str) -> Result<()> {
    let title = crypto::encrypt(title)?;
    conn.execute(
        "UPDATE sessions SET title = ?1 WHERE id = ?2",
        params![title, session_id],
    )?;
    Ok(())
}

/// Records the model a session is now using, so `sessions list` and a later
/// resume reflect the switch rather than reverting to whatever it started
/// with. Stored in the clear, like the other session metadata.
pub fn set_session_model(conn: &Connection, session_id: &str, model: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET model = ?1 WHERE id = ?2",
        params![model, session_id],
    )?;
    Ok(())
}

/// Records a session's current kind (`KIND_CHAT` / `KIND_AGENT_CHAT`), so
/// switching between plain and agent mode mid-session sticks on resume and
/// in `sessions list`, the same way [`set_session_model`] does for models.
pub fn set_session_kind(conn: &Connection, session_id: &str, kind: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET kind = ?1 WHERE id = ?2",
        params![kind, session_id],
    )?;
    Ok(())
}

/// Records a `/effort`-style override for this session's reasoning effort.
/// `None` clears it back to following the configured default.
pub fn set_session_effort_level(
    conn: &Connection,
    session_id: &str,
    effort_level: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET effort_level = ?1 WHERE id = ?2",
        params![effort_level, session_id],
    )?;
    Ok(())
}

/// Records a `/verbose`-style toggle for this session's TUI view.
pub fn set_session_verbose(conn: &Connection, session_id: &str, verbose: bool) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET verbose = ?1 WHERE id = ?2",
        params![verbose, session_id],
    )?;
    Ok(())
}

/// Records a `/max-iterations`-style override for this session's tool-loop
/// cap. `None` nullifies it (`/max-iterations clear`) — a turn falls back to
/// whatever the configured default is when it actually runs, rather than
/// this storing a frozen snapshot of it.
pub fn set_session_max_iterations(
    conn: &Connection,
    session_id: &str,
    max_iterations: Option<i64>,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET max_iterations = ?1 WHERE id = ?2",
        params![max_iterations, session_id],
    )?;
    Ok(())
}

/// Records a `/temperature`-style override for this session's sampling
/// temperature. `None` nullifies it, same deal as
/// [`set_session_max_iterations`].
pub fn set_session_temperature(
    conn: &Connection,
    session_id: &str,
    temperature: Option<f64>,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET temperature = ?1 WHERE id = ?2",
        params![temperature, session_id],
    )?;
    Ok(())
}

/// Records an `/approval`-style override for this session's tool-approval
/// gates.
pub fn set_session_approval(
    conn: &Connection,
    session_id: &str,
    approval: &ApprovalSettings,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET approval_read = ?1, approval_write = ?2, approval_terminal = ?3 WHERE id = ?4",
        params![
            approval.read_disk,
            approval.write_disk,
            approval.terminal,
            session_id
        ],
    )?;
    Ok(())
}

/// Records whether this session confines file writes to the working
/// directory and home.
pub fn set_session_sandbox(conn: &Connection, session_id: &str, sandbox: bool) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET sandbox = ?1 WHERE id = ?2",
        params![sandbox, session_id],
    )?;
    Ok(())
}

/// Appends a single message to a session and bumps its updated_at timestamp.
/// `seq` should be the message's 0-based position within the session.
/// `model`/`effort_level` record what was active when the message was
/// produced, so history can show which model generated each reply.
pub fn append_message(
    conn: &Connection,
    session_id: &str,
    seq: usize,
    message: &ChatMessage,
    model: &str,
    effort_level: Option<&str>,
) -> Result<()> {
    let tool_calls_json = message
        .tool_calls
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let reasoning_details_json = message
        .reasoning_details
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    let content = crypto::encrypt_opt(message.content.as_deref())?;
    let tool_calls_json = crypto::encrypt_opt(tool_calls_json.as_deref())?;
    let reasoning_details_json = crypto::encrypt_opt(reasoning_details_json.as_deref())?;
    let reasoning = crypto::encrypt_opt(message.reasoning.as_deref())?;

    conn.execute(
        "INSERT INTO messages (session_id, seq, role, content, tool_calls, tool_call_id, model, effort_level, reasoning_details, reasoning) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            session_id,
            seq as i64,
            message.role,
            content,
            tool_calls_json,
            message.tool_call_id,
            model,
            effort_level,
            reasoning_details_json,
            reasoning,
        ],
    )?;

    conn.execute(
        "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
        params![now(), session_id],
    )?;

    Ok(())
}

/// Lists all sessions, most recently updated first.
pub fn list_sessions(conn: &Connection) -> Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, model, kind, effort_level, verbose, max_iterations, temperature, \
         approval_read, approval_write, approval_terminal, sandbox, created_at, updated_at \
         FROM sessions ORDER BY updated_at DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(SessionSummary {
            id: row.get(0)?,
            title: row.get(1)?,
            model: row.get(2)?,
            kind: row.get(3)?,
            effort_level: row.get(4)?,
            verbose: row.get(5)?,
            max_iterations: row.get(6)?,
            temperature: row.get(7)?,
            approval: ApprovalSettings {
                read_disk: row.get(8)?,
                write_disk: row.get(9)?,
                terminal: row.get(10)?,
            },
            sandbox: row.get(11)?,
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
        })
    })?;

    let mut sessions = Vec::new();
    for row in rows {
        let mut session = row?;
        session.title = crypto::decrypt(&session.title)?;
        sessions.push(session);
    }
    Ok(sessions)
}

/// Fetches a single session's summary by id (or id prefix, if unambiguous).
pub fn find_session(conn: &Connection, id_or_prefix: &str) -> Result<Option<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, model, kind, effort_level, verbose, max_iterations, temperature, \
         approval_read, approval_write, approval_terminal, sandbox, created_at, updated_at \
         FROM sessions WHERE id = ?1 OR id LIKE ?2",
    )?;

    let pattern = format!("{}%", id_or_prefix);
    let mut rows = stmt.query(params![id_or_prefix, pattern])?;

    if let Some(row) = rows.next()? {
        let title: String = row.get(1)?;
        Ok(Some(SessionSummary {
            id: row.get(0)?,
            title: crypto::decrypt(&title)?,
            model: row.get(2)?,
            kind: row.get(3)?,
            effort_level: row.get(4)?,
            verbose: row.get(5)?,
            max_iterations: row.get(6)?,
            temperature: row.get(7)?,
            approval: ApprovalSettings {
                read_disk: row.get(8)?,
                write_disk: row.get(9)?,
                terminal: row.get(10)?,
            },
            sandbox: row.get(11)?,
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
        }))
    } else {
        Ok(None)
    }
}

/// Loads the full message history for a session, in order, along with the
/// model/effort level that produced each message.
pub fn load_messages(conn: &Connection, session_id: &str) -> Result<Vec<StoredMessage>> {
    let mut stmt = conn.prepare(
        "SELECT role, content, tool_calls, tool_call_id, model, effort_level, reasoning_details, reasoning FROM messages WHERE session_id = ?1 ORDER BY seq ASC",
    )?;

    let rows = stmt.query_map(params![session_id], |row| {
        let role: String = row.get(0)?;
        let content: Option<String> = row.get(1)?;
        let tool_calls_json: Option<String> = row.get(2)?;
        let tool_call_id: Option<String> = row.get(3)?;
        let model: Option<String> = row.get(4)?;
        let effort_level: Option<String> = row.get(5)?;
        let reasoning_details_json: Option<String> = row.get(6)?;
        let reasoning: Option<String> = row.get(7)?;
        Ok((
            role,
            content,
            tool_calls_json,
            tool_call_id,
            model,
            effort_level,
            reasoning_details_json,
            reasoning,
        ))
    })?;

    let mut messages = Vec::new();
    for row in rows {
        let (
            role,
            content,
            tool_calls_json,
            tool_call_id,
            model,
            effort_level,
            reasoning_details_json,
            reasoning,
        ) = row?;
        let content = crypto::decrypt_opt(content)?;
        let tool_calls_json = crypto::decrypt_opt(tool_calls_json)?;
        let tool_calls: Option<Vec<ToolCall>> = match tool_calls_json {
            Some(json) => Some(serde_json::from_str(&json)?),
            None => None,
        };
        let reasoning = crypto::decrypt_opt(reasoning)?;
        let reasoning_details_json = crypto::decrypt_opt(reasoning_details_json)?;
        let reasoning_details: Option<Vec<serde_json::Value>> = match reasoning_details_json {
            Some(json) => Some(serde_json::from_str(&json)?),
            None => None,
        };
        messages.push(StoredMessage {
            message: ChatMessage {
                role,
                content,
                tool_calls,
                tool_call_id,
                reasoning,
                reasoning_details,
            },
            model,
            effort_level,
        });
    }

    Ok(messages)
}

/// Deletes a session and its messages (via ON DELETE CASCADE).
pub fn delete_session(conn: &Connection, session_id: &str) -> Result<bool> {
    let affected = conn.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
    Ok(affected > 0)
}

/// Convenience check for whether a session exists, without loading it.
#[allow(dead_code)]
pub fn session_exists(conn: &Connection, session_id: &str) -> Result<bool> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{function_call_type, FunctionCall};

    fn memory_db() -> Connection {
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
                sandbox            INTEGER NOT NULL DEFAULT 1,
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

    #[test]
    fn derive_title_uses_first_user_message() {
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some("system prompt".to_string()),
                tool_calls: None,
                tool_call_id: None,
                ..Default::default()
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some("Write me a snake game".to_string()),
                tool_calls: None,
                tool_call_id: None,
                ..Default::default()
            },
        ];
        assert_eq!(derive_title(&messages), "Write me a snake game");
    }

    #[test]
    fn derive_title_falls_back_when_empty() {
        assert_eq!(derive_title(&[]), "Untitled");
    }

    #[test]
    fn derive_title_truncates_long_messages() {
        let long = "a".repeat(100);
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: Some(long),
            tool_calls: None,
            tool_call_id: None,
            ..Default::default()
        }];
        let title = derive_title(&messages);
        assert!(title.ends_with("..."));
        assert_eq!(title.chars().count(), 63);
    }

    #[test]
    fn create_and_load_session_roundtrip() {
        let conn = memory_db();
        let id = create_session(
            &conn,
            "orcarouter/auto",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();

        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
                ..Default::default()
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("hi there".to_string()),
                tool_calls: None,
                tool_call_id: None,
                ..Default::default()
            },
        ];

        for (seq, message) in messages.iter().enumerate() {
            append_message(&conn, &id, seq, message, "orcarouter/auto", Some("high")).unwrap();
        }

        let loaded = load_messages(&conn, &id).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].message.role, "user");
        assert_eq!(loaded[1].message.content, Some("hi there".to_string()));
        assert_eq!(loaded[1].model.as_deref(), Some("orcarouter/auto"));
        assert_eq!(loaded[1].effort_level.as_deref(), Some("high"));
    }

    #[test]
    fn reasoning_details_round_trip_through_persistence() {
        let conn = memory_db();
        let id = create_session(
            &conn,
            "anthropic/claude-sonnet-5",
            KIND_AGENT_CHAT,
            Some("high"),
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();

        let message = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: function_call_type(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: r#"{"filepath":"a.rs"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            reasoning: Some("checking the file first".to_string()),
            reasoning_details: Some(vec![serde_json::json!({
                "type": "reasoning.text",
                "text": "checking the file first",
                "signature": "sig-abc",
                "index": 0,
            })]),
        };
        append_message(
            &conn,
            &id,
            0,
            &message,
            "anthropic/claude-sonnet-5",
            Some("high"),
        )
        .unwrap();

        let loaded = load_messages(&conn, &id).unwrap();
        let details = loaded[0]
            .message
            .reasoning_details
            .as_ref()
            .expect("reasoning details survive a round trip");
        assert_eq!(details[0]["text"], "checking the file first");
        assert_eq!(details[0]["signature"], "sig-abc");
        // The prose survives too, so `/verbose` can still show the thinking
        // behind a reply after the session is resumed.
        assert_eq!(
            loaded[0].message.reasoning.as_deref(),
            Some("checking the file first")
        );
    }

    #[test]
    fn list_sessions_orders_by_updated_desc() {
        let conn = memory_db();
        let id1 = create_session(
            &conn,
            "model-a",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let id2 = create_session(
            &conn,
            "model-b",
            KIND_AGENT_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();

        let sessions = list_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, id2);
        assert_eq!(sessions[1].id, id1);
    }

    #[test]
    fn find_session_by_prefix() {
        let conn = memory_db();
        let id = create_session(
            &conn,
            "model-a",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();
        let prefix = &id[..8];

        let found = find_session(&conn, prefix).unwrap();
        assert_eq!(found.unwrap().id, id);
    }

    #[test]
    fn set_session_kind_switches_the_stored_kind() {
        let conn = memory_db();
        let id = create_session(
            &conn,
            "model-a",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();

        set_session_kind(&conn, &id, KIND_AGENT_CHAT).unwrap();
        assert_eq!(
            find_session(&conn, &id).unwrap().unwrap().kind,
            KIND_AGENT_CHAT
        );

        set_session_kind(&conn, &id, KIND_CHAT).unwrap();
        assert_eq!(find_session(&conn, &id).unwrap().unwrap().kind, KIND_CHAT);
    }

    #[test]
    fn new_sessions_store_the_given_snapshot() {
        let conn = memory_db();
        let id = create_session(
            &conn,
            "model-a",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();
        let summary = find_session(&conn, &id).unwrap().unwrap();
        // `effort_level` was given as `None` — a real "no effort sent" value,
        // not left unset.
        assert_eq!(summary.effort_level, None);
        assert!(!summary.verbose);
        // `max_iterations`/`temperature` are written immediately too, not
        // left `NULL` to be resolved against the config on every resume.
        assert_eq!(summary.max_iterations, Some(20));
        assert_eq!(summary.temperature, Some(0.7));
        assert_eq!(summary.approval, ApprovalSettings::default());
    }

    #[test]
    fn set_session_effort_level_switches_and_clears() {
        let conn = memory_db();
        let id = create_session(
            &conn,
            "model-a",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();

        set_session_effort_level(&conn, &id, Some("high")).unwrap();
        assert_eq!(
            find_session(&conn, &id).unwrap().unwrap().effort_level,
            Some("high".to_string())
        );

        set_session_effort_level(&conn, &id, None).unwrap();
        assert_eq!(
            find_session(&conn, &id).unwrap().unwrap().effort_level,
            None
        );
    }

    #[test]
    fn set_session_verbose_switches_the_flag() {
        let conn = memory_db();
        let id = create_session(
            &conn,
            "model-a",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();

        set_session_verbose(&conn, &id, true).unwrap();
        assert!(find_session(&conn, &id).unwrap().unwrap().verbose);

        set_session_verbose(&conn, &id, false).unwrap();
        assert!(!find_session(&conn, &id).unwrap().unwrap().verbose);
    }

    #[test]
    fn set_session_max_iterations_switches_and_clears() {
        let conn = memory_db();
        let id = create_session(
            &conn,
            "model-a",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();

        set_session_max_iterations(&conn, &id, Some(30)).unwrap();
        assert_eq!(
            find_session(&conn, &id).unwrap().unwrap().max_iterations,
            Some(30)
        );

        set_session_max_iterations(&conn, &id, None).unwrap();
        assert_eq!(
            find_session(&conn, &id).unwrap().unwrap().max_iterations,
            None
        );
    }

    #[test]
    fn set_session_temperature_switches_and_clears() {
        let conn = memory_db();
        let id = create_session(
            &conn,
            "model-a",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();

        set_session_temperature(&conn, &id, Some(1.2)).unwrap();
        assert_eq!(
            find_session(&conn, &id).unwrap().unwrap().temperature,
            Some(1.2)
        );

        set_session_temperature(&conn, &id, None).unwrap();
        assert_eq!(find_session(&conn, &id).unwrap().unwrap().temperature, None);
    }

    #[test]
    fn set_session_approval_updates_the_stored_gates() {
        let conn = memory_db();
        let id = create_session(
            &conn,
            "model-a",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();

        let custom = ApprovalSettings {
            read_disk: true,
            write_disk: false,
            terminal: false,
        };
        set_session_approval(&conn, &id, &custom).unwrap();
        assert_eq!(find_session(&conn, &id).unwrap().unwrap().approval, custom);
    }

    #[test]
    fn delete_session_removes_messages_via_cascade() {
        let conn = memory_db();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        let id = create_session(
            &conn,
            "model-a",
            KIND_CHAT,
            None,
            Some(20),
            Some(0.7),
            &ApprovalSettings::default(),
            true,
        )
        .unwrap();
        append_message(
            &conn,
            &id,
            0,
            &ChatMessage {
                role: "user".to_string(),
                content: Some("hi".to_string()),
                tool_calls: None,
                tool_call_id: None,
                ..Default::default()
            },
            "model-a",
            None,
        )
        .unwrap();

        let deleted = delete_session(&conn, &id).unwrap();
        assert!(deleted);
        assert!(!session_exists(&conn, &id).unwrap());
        assert!(load_messages(&conn, &id).unwrap().is_empty());
    }
}
