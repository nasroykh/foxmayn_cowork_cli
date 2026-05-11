use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::config::{Config, ThinkingDisplay, ToolDisplayVerbosity};
use crate::llm::types::{OllamaThink, ReasoningEffort, RequestReasoning};

// ── Public types ──────────────────────────────────────────────────────────────

pub struct SessionSummary {
    pub id: i64,
    pub started_at: i64,
    pub title: String,
}

pub struct ProjectStorage {
    conn: Connection,
}

pub struct Storage {
    global: Connection,
    pub project: Option<ProjectStorage>,
}

// ── Path helpers ──────────────────────────────────────────────────────────────

fn base_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("foxmayn-cowork")
}

/// Convert an absolute path to a flat directory name by replacing separators with `-`.
/// `/home/nas/myproject` → `-home-nas-myproject`
pub fn sanitize_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect()
}

// ── Storage ───────────────────────────────────────────────────────────────────

fn init_global_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
}

fn init_project_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at          INTEGER NOT NULL,
            title               TEXT    NOT NULL DEFAULT '',
            conversation_json   TEXT    NOT NULL DEFAULT '[]',
            chat_messages_json  TEXT    NOT NULL DEFAULT '[]'
        );",
    )
}

impl Storage {
    /// Open the global settings DB, falling back to an in-memory DB if the
    /// filesystem path cannot be created or opened. Always succeeds.
    pub fn open() -> Self {
        match Self::try_open() {
            Ok(s) => s,
            Err(_) => {
                let conn = Connection::open_in_memory().expect("in-memory SQLite");
                init_global_schema(&conn).ok();
                Storage {
                    global: conn,
                    project: None,
                }
            }
        }
    }

    fn try_open() -> rusqlite::Result<Self> {
        let base = base_dir();
        std::fs::create_dir_all(&base).ok();
        let conn = Connection::open(base.join("settings.db"))?;
        init_global_schema(&conn)?;
        Ok(Storage {
            global: conn,
            project: None,
        })
    }

    /// Load all saved settings as (key, value) pairs.
    pub fn load_settings(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = self.global.prepare("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect()
    }

    /// Upsert a single setting.
    pub fn save_setting(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.global.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Open (or create) the project DB for `working_dir`. Silently swallows
    /// errors so the app keeps working without persistence.
    pub fn open_project(&mut self, working_dir: &Path) {
        match self.try_open_project(working_dir) {
            Ok(ps) => self.project = Some(ps),
            Err(_) => self.project = None,
        }
    }

    fn try_open_project(&self, working_dir: &Path) -> rusqlite::Result<ProjectStorage> {
        let project_dir = base_dir().join("projects").join(sanitize_path(working_dir));
        std::fs::create_dir_all(&project_dir).ok();
        let conn = Connection::open(project_dir.join("data.db"))?;
        init_project_schema(&conn)?;
        Ok(ProjectStorage { conn })
    }
}

// ── ProjectStorage ────────────────────────────────────────────────────────────

impl ProjectStorage {
    /// Create a new session row and return its id.
    pub fn create_session(&self, title: &str) -> rusqlite::Result<i64> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.conn.execute(
            "INSERT INTO sessions (started_at, title) VALUES (?1, ?2)",
            params![now, title],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Overwrite the JSON blobs for an existing session.
    pub fn save_session(
        &self,
        id: i64,
        conversation_json: &str,
        chat_messages_json: &str,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions
             SET conversation_json = ?1, chat_messages_json = ?2
             WHERE id = ?3",
            params![conversation_json, chat_messages_json, id],
        )?;
        Ok(())
    }

    /// Return the 20 most recent sessions, newest first.
    pub fn list_sessions(&self) -> rusqlite::Result<Vec<SessionSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, title FROM sessions ORDER BY started_at DESC LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SessionSummary {
                id: row.get(0)?,
                started_at: row.get(1)?,
                title: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    /// Load conversation and chat-messages JSON for a session, or `None` if not found.
    pub fn load_session(&self, id: i64) -> rusqlite::Result<Option<(String, String)>> {
        let result = self.conn.query_row(
            "SELECT conversation_json, chat_messages_json FROM sessions WHERE id = ?1",
            params![id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        match result {
            Ok(pair) => Ok(Some(pair)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// ── Settings → Config ─────────────────────────────────────────────────────────

/// Apply saved DB settings on top of a config built from env/CLI. DB wins on conflict,
/// so slash-command changes from the previous session are restored on next launch.
pub fn apply_saved_settings(mut config: Config, storage: &Storage) -> Config {
    let settings = match storage.load_settings() {
        Ok(s) => s,
        Err(_) => return config,
    };
    for (key, value) in settings {
        match key.as_str() {
            "model" => config.model = value,
            // skip_confirmations is never restored — always starts disabled for safety.
            // Purge any stale value that may have been written by an older version.
            "skip_confirmations" => {
                let _ = storage
                    .global
                    .execute("DELETE FROM settings WHERE key = 'skip_confirmations'", []);
            }
            "streaming_enabled" => config.streaming_enabled = value == "true",
            "thinking_display" => {
                if let Ok(v) = value.parse::<ThinkingDisplay>() {
                    config.thinking_display = v;
                }
            }
            "tool_display_verbosity" => {
                if let Ok(v) = value.parse::<ToolDisplayVerbosity>() {
                    config.tool_display_verbosity = v;
                }
            }
            "openrouter_reasoning" => {
                if value == "off" {
                    config.openrouter_reasoning = None;
                } else if let Ok(effort) = value.parse::<ReasoningEffort>() {
                    let summary = config.openrouter_reasoning.as_ref().and_then(|r| r.summary);
                    config.openrouter_reasoning = Some(RequestReasoning {
                        effort: Some(effort),
                        summary,
                    });
                }
            }
            "ollama_think" => {
                if value == "off" {
                    config.ollama_think = None;
                } else if let Ok(t) = value.parse::<OllamaThink>() {
                    config.ollama_think = Some(t);
                }
            }
            _ => {}
        }
    }
    config
}
