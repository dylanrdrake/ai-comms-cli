use crate::client::{ChatMessage, ToolCall};
use crate::config::get_config_dir;
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
            id          TEXT PRIMARY KEY,
            title       TEXT NOT NULL,
            model       TEXT NOT NULL,
            kind        TEXT NOT NULL,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS messages (
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

        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
        ",
    )?;

    // `model`/`effort_level` were added after messages already shipped without
    // them; back them onto any database created before this change.
    ensure_column(&conn, "messages", "model", "TEXT")?;
    ensure_column(&conn, "messages", "effort_level", "TEXT")?;

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
pub fn create_session(conn: &Connection, model: &str, kind: &str) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    let title = crypto::encrypt("Untitled")?;

    conn.execute(
        "INSERT INTO sessions (id, title, model, kind, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![id, title, model, kind, ts],
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

    let content = crypto::encrypt_opt(message.content.as_deref())?;
    let tool_calls_json = crypto::encrypt_opt(tool_calls_json.as_deref())?;

    conn.execute(
        "INSERT INTO messages (session_id, seq, role, content, tool_calls, tool_call_id, model, effort_level) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            session_id,
            seq as i64,
            message.role,
            content,
            tool_calls_json,
            message.tool_call_id,
            model,
            effort_level,
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
        "SELECT id, title, model, kind, created_at, updated_at FROM sessions ORDER BY updated_at DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(SessionSummary {
            id: row.get(0)?,
            title: row.get(1)?,
            model: row.get(2)?,
            kind: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
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
        "SELECT id, title, model, kind, created_at, updated_at FROM sessions WHERE id = ?1 OR id LIKE ?2",
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
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        }))
    } else {
        Ok(None)
    }
}

/// Loads the full message history for a session, in order, along with the
/// model/effort level that produced each message.
pub fn load_messages(conn: &Connection, session_id: &str) -> Result<Vec<StoredMessage>> {
    let mut stmt = conn.prepare(
        "SELECT role, content, tool_calls, tool_call_id, model, effort_level FROM messages WHERE session_id = ?1 ORDER BY seq ASC",
    )?;

    let rows = stmt.query_map(params![session_id], |row| {
        let role: String = row.get(0)?;
        let content: Option<String> = row.get(1)?;
        let tool_calls_json: Option<String> = row.get(2)?;
        let tool_call_id: Option<String> = row.get(3)?;
        let model: Option<String> = row.get(4)?;
        let effort_level: Option<String> = row.get(5)?;
        Ok((
            role,
            content,
            tool_calls_json,
            tool_call_id,
            model,
            effort_level,
        ))
    })?;

    let mut messages = Vec::new();
    for row in rows {
        let (role, content, tool_calls_json, tool_call_id, model, effort_level) = row?;
        let content = crypto::decrypt_opt(content)?;
        let tool_calls_json = crypto::decrypt_opt(tool_calls_json)?;
        let tool_calls: Option<Vec<ToolCall>> = match tool_calls_json {
            Some(json) => Some(serde_json::from_str(&json)?),
            None => None,
        };
        messages.push(StoredMessage {
            message: ChatMessage {
                role,
                content,
                tool_calls,
                tool_call_id,
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

    fn memory_db() -> Connection {
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
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some("Write me a snake game".to_string()),
                tool_calls: None,
                tool_call_id: None,
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
        }];
        let title = derive_title(&messages);
        assert!(title.ends_with("..."));
        assert_eq!(title.chars().count(), 63);
    }

    #[test]
    fn create_and_load_session_roundtrip() {
        let conn = memory_db();
        let id = create_session(&conn, "orcarouter/auto", KIND_CHAT).unwrap();

        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some("hello".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("hi there".to_string()),
                tool_calls: None,
                tool_call_id: None,
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
    fn list_sessions_orders_by_updated_desc() {
        let conn = memory_db();
        let id1 = create_session(&conn, "model-a", KIND_CHAT).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let id2 = create_session(&conn, "model-b", KIND_AGENT_CHAT).unwrap();

        let sessions = list_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, id2);
        assert_eq!(sessions[1].id, id1);
    }

    #[test]
    fn find_session_by_prefix() {
        let conn = memory_db();
        let id = create_session(&conn, "model-a", KIND_CHAT).unwrap();
        let prefix = &id[..8];

        let found = find_session(&conn, prefix).unwrap();
        assert_eq!(found.unwrap().id, id);
    }

    #[test]
    fn delete_session_removes_messages_via_cascade() {
        let conn = memory_db();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        let id = create_session(&conn, "model-a", KIND_CHAT).unwrap();
        append_message(
            &conn,
            &id,
            0,
            &ChatMessage {
                role: "user".to_string(),
                content: Some("hi".to_string()),
                tool_calls: None,
                tool_call_id: None,
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
