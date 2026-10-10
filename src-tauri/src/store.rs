//! Local storage for captures (GUARDRAILS §6).
//!
//! Everything lives in one data folder that only the user can open (`0700`).
//! The database is SQLite with an FTS5 index, so it can be inspected without
//! Lax. See docs/adr/0006-sqlite-with-rusqlite.md.
//!
//! The store redacts titles and text itself before writing them, so a caller
//! can't store a secret by forgetting to redact (GUARDRAILS §4).

use std::fmt;
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, Row, params};

use crate::redact::redact;

pub const DATABASE_FILE: &str = "lax.sqlite";

/// Schema changes, applied in order. The database's `user_version` is the
/// number of migrations it has.
const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_captures.sql")];

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    /// The database was created by a newer version of Lax.
    NewerSchema {
        found: i64,
        supported: i64,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "data folder: {e}"),
            StoreError::Sqlite(e) => write!(f, "database: {e}"),
            StoreError::NewerSchema { found, supported } => write!(
                f,
                "database schema {found} is newer than this version of Lax supports ({supported})"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sqlite(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// A capture as handed over by capture, before redaction.
#[derive(Debug, Clone)]
pub struct NewCapture<'a> {
    pub captured_at: DateTime<Utc>,
    pub app_name: &'a str,
    pub bundle_id: &'a str,
    pub window_title: &'a str,
    /// The host only, from `site::site_from_url`.
    pub site: Option<&'a str>,
    pub text: &'a str,
}

/// A stored capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub id: i64,
    pub captured_at: DateTime<Utc>,
    pub app_name: String,
    pub bundle_id: String,
    pub window_title: String,
    pub site: Option<String>,
    pub text: String,
}

/// Creates the data folder if needed, and makes sure only the user can open it,
/// also when it already existed with wider permissions.
pub fn prepare_data_dir(dir: &Path) -> std::io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    fs::set_permissions(dir, Permissions::from_mode(0o700))
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (or creates) the database in `data_dir`.
    pub fn open(data_dir: &Path) -> Result<Self> {
        prepare_data_dir(data_dir)?;
        let path = data_dir.join(DATABASE_FILE);
        // Create the file private from the start, not chmod it afterwards.
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        fs::set_permissions(&path, Permissions::from_mode(0o600))?;
        let conn = Connection::open(&path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        // Overwrite deleted content instead of leaving it in free pages.
        conn.pragma_update(None, "secure_delete", "ON")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn)?;
        Ok(Store { conn })
    }

    /// Stores a capture. The window title and text are redacted here, and a
    /// site that isn't a bare host is dropped.
    pub fn insert(&self, capture: &NewCapture) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO captures (captured_at, app_name, bundle_id, window_title, site, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                capture.captured_at.timestamp_millis(),
                capture.app_name,
                capture.bundle_id,
                redact(capture.window_title),
                capture.site.and_then(bare_host),
                redact(capture.text),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Full-text search over titles and text, best matches first. Every word
    /// in `query` must match, as a word or the start of one. Search syntax
    /// characters in `query` are treated as plain text.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Capture>> {
        let Some(query) = match_query(query) else {
            return Ok(Vec::new());
        };
        let mut statement = self.conn.prepare(
            "SELECT c.id, c.captured_at, c.app_name, c.bundle_id, c.window_title, c.site, c.text
             FROM captures_fts
             JOIN captures AS c ON c.id = captures_fts.rowid
             WHERE captures_fts MATCH ?1
             ORDER BY rank, c.captured_at DESC
             LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(params![query, limit], capture_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let supported = schema_version();
    if version > supported {
        return Err(StoreError::NewerSchema {
            found: version,
            supported,
        });
    }
    for (number, sql) in (1..).zip(MIGRATIONS).skip_while(|(n, _)| *n <= version) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", number)?;
        tx.commit()?;
    }
    Ok(())
}

/// The schema version this build of Lax creates and understands.
fn schema_version() -> i64 {
    i64::try_from(MIGRATIONS.len()).expect("fewer than 2^63 migrations")
}

fn capture_from_row(row: &Row) -> rusqlite::Result<Capture> {
    let millis: i64 = row.get(1)?;
    Ok(Capture {
        id: row.get(0)?,
        captured_at: DateTime::from_timestamp_millis(millis).unwrap_or_default(),
        app_name: row.get(2)?,
        bundle_id: row.get(3)?,
        window_title: row.get(4)?,
        site: row.get(5)?,
        text: row.get(6)?,
    })
}

/// A host like `figma.com`, `localhost`, `192.168.1.10` or `::1`. Anything
/// else, such as a full address with a path, is not stored.
fn bare_host(site: &str) -> Option<String> {
    let site = site.trim().to_ascii_lowercase();
    let name = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-');
    // Colons only in an IPv6 address, so "about:blank" isn't a host.
    let ipv6 = |c: char| c.is_ascii_hexdigit() || c == ':';
    let is_host = site.chars().all(name) || (site.contains(':') && site.chars().all(ipv6));
    (!site.is_empty() && is_host).then_some(site)
}

/// Turns what the user typed into an FTS5 query: each word is quoted, so
/// operators and punctuation are plain text. Only the last word is matched
/// as a prefix, so results follow typing without "ai" matching "air".
fn match_query(input: &str) -> Option<String> {
    let words: Vec<&str> = input.split_whitespace().collect();
    let last = words.len().checked_sub(1)?;
    let terms: Vec<String> = words
        .iter()
        .enumerate()
        .map(|(i, word)| {
            let quoted = format!("\"{}\"", word.replace('"', "\"\""));
            if i == last { quoted + "*" } else { quoted }
        })
        .collect();
    Some(terms.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh folder under the system temp folder, removed afterwards.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let name = format!(
                "lax-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(name);
            let _ = fs::remove_dir_all(&path);
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn capture<'a>(title: &'a str, text: &'a str) -> NewCapture<'a> {
        NewCapture {
            captured_at: DateTime::from_timestamp_millis(1_781_520_000_000).unwrap(),
            app_name: "Mail",
            bundle_id: "com.apple.mail",
            window_title: title,
            site: None,
            text,
        }
    }

    fn titles(hits: &[Capture]) -> Vec<&str> {
        hits.iter().map(|c| c.window_title.as_str()).collect()
    }

    #[test]
    fn data_folder_is_private() {
        let tmp = TempDir::new();
        let dir = tmp.0.join("Lax");
        prepare_data_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn existing_data_folder_is_made_private() {
        let tmp = TempDir::new();
        fs::create_dir_all(&tmp.0).unwrap();
        fs::set_permissions(&tmp.0, Permissions::from_mode(0o755)).unwrap();
        prepare_data_dir(&tmp.0).unwrap();
        assert_eq!(mode(&tmp.0), 0o700);
    }

    #[test]
    fn database_file_is_private() {
        let tmp = TempDir::new();
        Store::open(&tmp.0).unwrap();
        assert_eq!(mode(&tmp.0.join(DATABASE_FILE)), 0o600);
    }

    #[test]
    fn migrations_run_once() {
        let tmp = TempDir::new();
        let store = Store::open(&tmp.0).unwrap();
        store.insert(&capture("Inbox", "hello")).unwrap();
        drop(store);

        let store = Store::open(&tmp.0).unwrap();
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, schema_version());
        assert_eq!(store.search("hello", 10).unwrap().len(), 1);
    }

    #[test]
    fn newer_schema_is_refused() {
        let tmp = TempDir::new();
        drop(Store::open(&tmp.0).unwrap());
        let conn = Connection::open(tmp.0.join(DATABASE_FILE)).unwrap();
        conn.pragma_update(None, "user_version", schema_version() + 1)
            .unwrap();
        drop(conn);
        assert!(matches!(
            Store::open(&tmp.0),
            Err(StoreError::NewerSchema { .. })
        ));
    }

    #[test]
    fn secrets_are_redacted_before_they_are_stored() {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .insert(&capture(
                "password: hunter2 - Notes",
                "Anna Example's card 4111 1111 1111 1111, pnr 811218-9876",
            ))
            .unwrap();
        let stored: (String, String) = store
            .conn
            .query_row(
                "SELECT window_title, text FROM captures WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored.0, "password: [secret] - Notes");
        assert_eq!(
            stored.1,
            "Anna Example's card [card number], pnr [personal identity number]"
        );
        assert!(store.search("hunter2", 10).unwrap().is_empty());
        assert!(store.search("4111", 10).unwrap().is_empty());
    }

    #[test]
    fn only_a_bare_host_is_stored_as_site() {
        let store = Store::open_in_memory().unwrap();
        let mut c = capture("Q3 proposal", "draft");
        c.site = Some("Figma.com");
        store.insert(&c).unwrap();
        c.site = Some("https://figma.com/file/abc?token=xyz");
        store.insert(&c).unwrap();
        c.site = Some("about:blank");
        store.insert(&c).unwrap();
        c.site = Some("::1");
        store.insert(&c).unwrap();
        let sites: Vec<Option<String>> = store
            .search("draft", 10)
            .unwrap()
            .into_iter()
            .map(|c| c.site)
            .collect();
        assert!(sites.contains(&Some("figma.com".to_string())));
        assert!(sites.contains(&None));
        assert!(sites.contains(&Some("::1".to_string())));
        assert!(!sites.contains(&Some("about:blank".to_string())));
        assert!(!sites.iter().flatten().any(|s| s.contains('/')));
    }

    #[test]
    fn search_finds_words_and_prefixes() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert(&capture(
                "Re: Offerten",
                "Kan vi ses på torsdag om överenskommelsen?",
            ))
            .unwrap();
        store
            .insert(&capture("Weekly report", "Numbers for Q3 look good"))
            .unwrap();

        assert_eq!(
            titles(&store.search("torsdag", 10).unwrap()),
            ["Re: Offerten"]
        );
        assert_eq!(
            titles(&store.search("ÖVERENS", 10).unwrap()),
            ["Re: Offerten"]
        );
        assert_eq!(
            titles(&store.search("offert", 10).unwrap()),
            ["Re: Offerten"]
        );
        assert_eq!(
            titles(&store.search("q3 numbers", 10).unwrap()),
            ["Weekly report"]
        );
        assert!(store.search("torsdag numbers", 10).unwrap().is_empty());
        // Only the last word is a prefix.
        assert!(store.search("tors dag", 10).unwrap().is_empty());
        assert!(store.search("q numbers", 10).unwrap().is_empty());
    }

    #[test]
    fn search_keeps_swedish_letters_apart() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert(&capture("Anteckningar", "Det här är för dig"))
            .unwrap();
        assert_eq!(store.search("för", 10).unwrap().len(), 1);
        assert!(store.search("for", 10).unwrap().is_empty());
    }

    #[test]
    fn search_syntax_in_queries_is_plain_text() {
        let store = Store::open_in_memory().unwrap();
        store.insert(&capture("Inbox", "hello world")).unwrap();
        for query in [
            "\"",
            "NEAR(",
            "AND OR NOT",
            "*",
            "--",
            "hello\"",
            "text:",
            "^hello",
            "(",
        ] {
            assert!(store.search(query, 10).is_ok(), "{query}");
        }
        assert!(store.search("", 10).unwrap().is_empty());
        assert!(store.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn search_respects_the_limit() {
        let store = Store::open_in_memory().unwrap();
        for _ in 0..5 {
            store.insert(&capture("Inbox", "repeated")).unwrap();
        }
        assert_eq!(store.search("repeated", 3).unwrap().len(), 3);
    }

    #[test]
    fn deleted_captures_leave_the_index() {
        let store = Store::open_in_memory().unwrap();
        let id = store.insert(&capture("Inbox", "forgettable")).unwrap();
        store
            .conn
            .execute("DELETE FROM captures WHERE id = ?1", [id])
            .unwrap();
        assert!(store.search("forgettable", 10).unwrap().is_empty());
    }

    #[test]
    fn captures_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let new = NewCapture {
            site: Some("example.com"),
            ..capture("Inbox", "round trip")
        };
        let id = store.insert(&new).unwrap();
        let hits = store.search("round", 10).unwrap();
        assert_eq!(
            hits,
            [Capture {
                id,
                captured_at: new.captured_at,
                app_name: "Mail".to_string(),
                bundle_id: "com.apple.mail".to_string(),
                window_title: "Inbox".to_string(),
                site: Some("example.com".to_string()),
                text: "round trip".to_string(),
            }]
        );
    }
}
