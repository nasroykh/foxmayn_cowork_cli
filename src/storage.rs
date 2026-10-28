use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::config::{Config, Provider, ThinkingDisplay, ToolDisplayVerbosity};
use crate::llm::types::{OllamaThink, ReasoningEffort, RequestReasoning};

/// Current schema version of both databases (`PRAGMA user_version`). Bump and add a migration
/// step in `migrate` when a table changes.
const SCHEMA_VERSION: i64 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Set when the on-disk settings DB could not be opened and settings fall back to memory
    /// (nothing persists). Shown to the user once at startup.
    pub init_warning: Option<String>,
}

// ── Path helpers ──────────────────────────────────────────────────────────────

fn base_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("foxmayn-cowork")
}

/// Create `dir` (and parents), owner-only on unix: session logs can contain file contents.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Stable 64-bit FNV-1a. Not `DefaultHasher`: its output may change between Rust releases,
/// which would orphan every project DB.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Directory name for a project DB: a readable tail of the path plus a hash of the full path,
/// so distinct directories never collide (`/a-b/c` vs `/a/b-c`) and long paths stay short.
/// `/home/nas/myproject` → `home-nas-myproject-3f2a9c41d07be655` (tail capped at 48 chars).
/// `path` should already be canonical.
pub fn sanitize_path(path: &Path) -> String {
    let full = path.to_string_lossy();
    let readable: String = full
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = readable.trim_matches('-');
    let tail: String = {
        let mut t: Vec<char> = trimmed.chars().rev().take(48).collect();
        t.reverse();
        t.into_iter().collect()
    };
    format!(
        "{}-{:016x}",
        tail.trim_start_matches('-'),
        fnv1a64(full.as_bytes())
    )
}

/// Settings key under which `/model` is remembered for `provider`.
pub fn model_setting_key(provider: &Provider) -> &'static str {
    match provider {
        Provider::OpenRouter => "model.openrouter",
        Provider::Ollama => "model.ollama",
        Provider::Local => "model.local",
    }
}

// ── Schema ────────────────────────────────────────────────────────────────────

const GLOBAL_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );";

const PROJECT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS sessions (
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        started_at          INTEGER NOT NULL,
        title               TEXT    NOT NULL DEFAULT '',
        conversation_json   TEXT    NOT NULL DEFAULT '[]',
        chat_messages_json  TEXT    NOT NULL DEFAULT '[]'
    );";

/// Bring `conn` to `SCHEMA_VERSION`. Version 0 is both a fresh DB and a pre-versioning one;
/// the baseline schema uses `IF NOT EXISTS`, so both end up identical at version 1.
fn migrate(conn: &Connection, schema: &str) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_SCHEMA),
            Some(format!(
                "database schema v{version} is newer than this build supports (v{SCHEMA_VERSION})"
            )),
        ));
    }
    if version < 1 {
        conn.execute_batch(schema)?;
    }
    // Future migrations: `if version < 2 { ...ALTER TABLE...; }`
    if version < SCHEMA_VERSION {
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

fn open_db(path: &Path, schema: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    migrate(&conn, schema)?;
    Ok(conn)
}

impl Storage {
    /// Open the global settings DB, falling back to an in-memory DB (and recording why in
    /// `init_warning`) if the filesystem path cannot be created or opened.
    pub fn open() -> Self {
        match Self::try_open() {
            Ok(s) => s,
            Err(e) => {
                let mut s = Self::in_memory();
                s.init_warning = Some(format!(
                    "Settings and sessions cannot be saved ({e}); changes last only for this run."
                ));
                s
            }
        }
    }

    fn in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("in-memory SQLite");
        let _ = migrate(&conn, GLOBAL_SCHEMA);
        Storage {
            global: conn,
            project: None,
            init_warning: None,
        }
    }

    fn try_open() -> Result<Self, String> {
        let base = base_dir();
        create_private_dir(&base).map_err(|e| format!("{}: {e}", base.display()))?;
        let conn = open_db(&base.join("settings.db"), GLOBAL_SCHEMA).map_err(|e| e.to_string())?;
        Ok(Storage {
            global: conn,
            project: None,
            init_warning: None,
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

    /// Forget every remembered `/model` choice. Called after the config wizard writes `MODEL`,
    /// because a saved `model.<provider>` would otherwise override it on the next launch.
    pub fn clear_saved_models(&self) -> rusqlite::Result<()> {
        self.global.execute(
            "DELETE FROM settings WHERE key = 'model' OR key LIKE 'model.%'",
            [],
        )?;
        Ok(())
    }

    /// Open (or create) the project DB for `working_dir` (pass the canonical path). On failure
    /// the app keeps working without persistence and the error is returned for display.
    pub fn open_project(&mut self, working_dir: &Path) -> Result<(), String> {
        match Self::try_open_project(working_dir) {
            Ok(ps) => {
                self.project = Some(ps);
                Ok(())
            }
            Err(e) => {
                self.project = None;
                Err(e)
            }
        }
    }

    fn try_open_project(working_dir: &Path) -> Result<ProjectStorage, String> {
        let project_dir = base_dir().join("projects").join(sanitize_path(working_dir));
        create_private_dir(&project_dir).map_err(|e| format!("{}: {e}", project_dir.display()))?;
        let conn =
            open_db(&project_dir.join("data.db"), PROJECT_SCHEMA).map_err(|e| e.to_string())?;
        // WAL makes each per-turn save a cheap append instead of a full journal fsync, and lets
        // a second instance read while one writes. Best effort: some filesystems refuse it.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
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

/// Apply saved DB settings on top of a config built from env (and `--provider`). The caller
/// re-applies explicit CLI flags afterwards so they win. The model is stored per provider
/// (`model.<provider>`) and only the active provider's entry is used; the legacy provider-less
/// `model` key is ignored. Returns the warning to show if the settings could not be read.
pub fn apply_saved_settings(mut config: Config, storage: &Storage) -> (Config, Option<String>) {
    let settings = match storage.load_settings() {
        Ok(s) => s,
        Err(e) => return (config, Some(format!("Could not load saved settings: {e}"))),
    };
    let model_key = model_setting_key(&config.provider);
    for (key, value) in settings {
        match key.as_str() {
            k if k == model_key => config.model = value,
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
    (config, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_path_is_collision_free_and_stable() {
        let a = sanitize_path(Path::new("/a-b/c"));
        let b = sanitize_path(Path::new("/a/b-c"));
        assert_ne!(a, b);
        assert_eq!(a, sanitize_path(Path::new("/a-b/c")));
        assert!(a.starts_with("a-b-c-"), "{a}");
    }

    #[test]
    fn fnv1a64_matches_reference_vectors() {
        // Pinned: the hash must not change between releases or project DBs are orphaned.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn sanitize_path_is_a_single_safe_component() {
        let long = format!("/{}", "x/".repeat(300));
        for p in [".", "..", "/", "C:\\Users\\me", long.as_str(), "/tmp/ü ñ"] {
            let s = sanitize_path(Path::new(p));
            assert!(!s.contains(['/', '\\', ':']), "{s}");
            assert!(s.len() <= 48 * 4 + 17, "{s}");
            assert_ne!(s, "..");
        }
    }

    #[test]
    fn saved_model_applies_only_to_its_provider() {
        let storage = Storage::in_memory();
        storage.save_setting("model.ollama", "qwen3:8b").unwrap();
        storage.save_setting("model", "legacy/ignored").unwrap();

        let mut cfg = Config::from_env();
        cfg.provider = Provider::OpenRouter;
        cfg.model = "default/model".into();
        let (cfg, warn) = apply_saved_settings(cfg, &storage);
        assert!(warn.is_none());
        assert_eq!(cfg.model, "default/model");

        let mut cfg = Config::from_env();
        cfg.provider = Provider::Ollama;
        let (cfg, _) = apply_saved_settings(cfg, &storage);
        assert_eq!(cfg.model, "qwen3:8b");
    }

    #[test]
    fn clear_saved_models_removes_only_model_keys() {
        let storage = Storage::in_memory();
        storage.save_setting("model.ollama", "a").unwrap();
        storage.save_setting("model.openrouter", "b").unwrap();
        storage.save_setting("model", "legacy").unwrap();
        storage.save_setting("streaming_enabled", "true").unwrap();
        storage.clear_saved_models().unwrap();
        let left: Vec<String> = storage
            .load_settings()
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(left, vec!["streaming_enabled".to_string()]);
    }

    #[test]
    fn schema_version_is_stamped_and_newer_is_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn, PROJECT_SCHEMA).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(migrate(&conn, PROJECT_SCHEMA).is_err());
    }
}
