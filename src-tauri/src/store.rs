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

use chrono::{DateTime, TimeDelta, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::pause::CaptureState;
use crate::redact::redact;

pub const DATABASE_FILE: &str = "lax.sqlite";

/// Schema changes, applied in order. The database's `user_version` is the
/// number of migrations it has.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_captures.sql"),
    include_str!("../migrations/0002_settings.sql"),
];

/// How long raw captures are kept unless the user chooses otherwise
/// (GUARDRAILS §4). Long enough that nothing is lost before the journal
/// (Stage 3) can summarize it; from then on only summarized captures expire.
pub const DEFAULT_RETENTION_DAYS: u32 = 90;
/// The longest retention the user can choose: ten years.
pub const MAX_RETENTION_DAYS: u32 = 3650;

/// How often capture reads the active window unless the user chooses
/// otherwise, and the range they can choose from.
pub const DEFAULT_CAPTURE_INTERVAL_SECONDS: u32 = 5;
pub const CAPTURE_INTERVAL_RANGE: std::ops::RangeInclusive<u32> = 2..=300;

const RETENTION_KEY: &str = "retention_days";
const CAPTURE_INTERVAL_KEY: &str = "capture_interval_seconds";
const PAUSE_KEY: &str = "paused_until";
const PAUSED_UNTIL_RESUMED: &str = "resumed";

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    /// Retention must be between 1 and [`MAX_RETENTION_DAYS`] days.
    InvalidRetention(u32),
    /// The capture interval must be in [`CAPTURE_INTERVAL_RANGE`].
    InvalidCaptureInterval(u32),
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
            StoreError::InvalidRetention(days) => write!(
                f,
                "retention of {days} days is outside 1 to {MAX_RETENTION_DAYS}"
            ),
            StoreError::InvalidCaptureInterval(seconds) => write!(
                f,
                "capture interval of {seconds} seconds is outside {} to {}",
                CAPTURE_INTERVAL_RANGE.start(),
                CAPTURE_INTERVAL_RANGE.end()
            ),
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

/// What to forget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forget {
    /// Everything captured before this moment (retention).
    Before(DateTime<Utc>),
    /// Everything captured from this moment on ("the last 15 minutes").
    Since(DateTime<Utc>),
    /// Everything captured from `from` up to, not including, `to`.
    Between(DateTime<Utc>, DateTime<Utc>),
    /// Everything captured from one app, by bundle id.
    App(String),
    /// Everything.
    Everything,
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
    pub(crate) fn open_in_memory() -> Result<Self> {
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

impl Store {
    /// Deletes what `what` covers, including everything derived from it, and
    /// makes sure the deleted text is gone from the database files. Returns
    /// how many captures were deleted.
    ///
    /// When episodes and memory pages arrive, what is derived from the
    /// deleted captures must be deleted here too (GUARDRAILS §4).
    pub fn forget(&self, what: &Forget) -> Result<usize> {
        let deleted = self.delete(what)?;
        if deleted > 0 {
            self.scrub()?;
        }
        Ok(deleted)
    }

    /// Deletes captures older than the retention period.
    ///
    /// This runs every hour, so it skips the full rebuild that [`Store::forget`]
    /// does. Deleted text is still gone from the files: `secure_delete`
    /// overwrites the freed pages, FTS5 secure-delete removes it from the index,
    /// and the checkpoint empties the write-ahead log.
    pub fn expire(&self, now: DateTime<Utc>) -> Result<usize> {
        let days = i64::from(self.retention_days()?);
        let deleted = self.delete(&Forget::Before(now - TimeDelta::days(days)))?;
        if deleted > 0 {
            self.checkpoint()?;
        }
        Ok(deleted)
    }

    fn delete(&self, what: &Forget) -> Result<usize> {
        Ok(match what {
            Forget::Before(t) => self.conn.execute(
                "DELETE FROM captures WHERE captured_at < ?1",
                [t.timestamp_millis()],
            )?,
            Forget::Since(t) => self.conn.execute(
                "DELETE FROM captures WHERE captured_at >= ?1",
                [t.timestamp_millis()],
            )?,
            Forget::Between(from, to) => self.conn.execute(
                "DELETE FROM captures WHERE captured_at >= ?1 AND captured_at < ?2",
                [from.timestamp_millis(), to.timestamp_millis()],
            )?,
            Forget::App(bundle_id) => self.conn.execute(
                "DELETE FROM captures WHERE bundle_id = ?1 COLLATE NOCASE",
                [bundle_id],
            )?,
            Forget::Everything => self.conn.execute("DELETE FROM captures", [])?,
        })
    }

    pub fn retention_days(&self) -> Result<u32> {
        let value = self.setting(RETENTION_KEY)?;
        // A missing or damaged value falls back to the default rather than
        // to keeping captures forever.
        Ok(value
            .and_then(|v| v.parse().ok())
            .filter(|days| (1..=MAX_RETENTION_DAYS).contains(days))
            .unwrap_or(DEFAULT_RETENTION_DAYS))
    }

    /// Changes the retention and applies it right away, so captures older
    /// than a shorter retention are deleted now, not at the next hourly run.
    pub fn set_retention_days(&self, days: u32, now: DateTime<Utc>) -> Result<()> {
        if !(1..=MAX_RETENTION_DAYS).contains(&days) {
            return Err(StoreError::InvalidRetention(days));
        }
        self.set_setting(RETENTION_KEY, &days.to_string())?;
        self.expire(now)?;
        Ok(())
    }

    /// How many seconds capture waits between reads of the active window.
    pub fn capture_interval_seconds(&self) -> Result<u32> {
        let value = self.setting(CAPTURE_INTERVAL_KEY)?;
        Ok(value
            .and_then(|v| v.parse().ok())
            .filter(|seconds| CAPTURE_INTERVAL_RANGE.contains(seconds))
            .unwrap_or(DEFAULT_CAPTURE_INTERVAL_SECONDS))
    }

    pub fn set_capture_interval_seconds(&self, seconds: u32) -> Result<()> {
        if !CAPTURE_INTERVAL_RANGE.contains(&seconds) {
            return Err(StoreError::InvalidCaptureInterval(seconds));
        }
        self.set_setting(CAPTURE_INTERVAL_KEY, &seconds.to_string())
    }

    /// The pause state saved by [`Store::set_pause_state`], so a pause
    /// survives restarting Lax.
    pub fn pause_state(&self) -> Result<CaptureState> {
        let value = self.setting(PAUSE_KEY)?;
        Ok(match value.as_deref() {
            None => CaptureState::Running,
            Some(PAUSED_UNTIL_RESUMED) => CaptureState::Paused { until: None },
            Some(millis) => match millis
                .parse()
                .ok()
                .and_then(DateTime::from_timestamp_millis)
            {
                Some(until) => CaptureState::Paused { until: Some(until) },
                // A damaged value keeps capture paused until the user resumes:
                // stopping by mistake is safer than capturing by mistake.
                None => CaptureState::Paused { until: None },
            },
        })
    }

    pub fn set_pause_state(&self, state: CaptureState) -> Result<()> {
        match state {
            CaptureState::Running => {
                self.conn
                    .execute("DELETE FROM settings WHERE key = ?1", [PAUSE_KEY])?;
                Ok(())
            }
            CaptureState::Paused { until: None } => {
                self.set_setting(PAUSE_KEY, PAUSED_UNTIL_RESUMED)
            }
            CaptureState::Paused { until: Some(t) } => {
                self.set_setting(PAUSE_KEY, &t.timestamp_millis().to_string())
            }
        }
    }

    fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Removes deleted content from the files on disk. `secure_delete` has
    /// already overwritten the freed pages; VACUUM rebuilds the file without
    /// them, and the checkpoint empties the write-ahead log, which can still
    /// hold old copies of pages.
    fn scrub(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO captures_fts (captures_fts) VALUES ('optimize')",
            [],
        )?;
        self.conn.execute_batch("VACUUM")?;
        self.checkpoint()
    }

    /// Moves everything from the write-ahead log into the database and
    /// empties the log, which can still hold old copies of deleted pages.
    fn checkpoint(&self) -> Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
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

    fn at_minute(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_781_520_000_000).unwrap() + TimeDelta::minutes(minute)
    }

    /// Stores one capture per (minute, bundle id, text).
    fn store_with(store: &Store, captures: &[(i64, &str, &str)]) {
        for &(minute, bundle_id, text) in captures {
            store
                .insert(&NewCapture {
                    captured_at: at_minute(minute),
                    bundle_id,
                    ..capture("Window", text)
                })
                .unwrap();
        }
    }

    fn remaining(store: &Store) -> Vec<String> {
        let mut statement = store
            .conn
            .prepare("SELECT text FROM captures ORDER BY captured_at")
            .unwrap();
        statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    const SAMPLE: &[(i64, &str, &str)] = &[
        (0, "com.apple.mail", "alpha"),
        (10, "com.tinyspeck.slackmacgap", "bravo"),
        (20, "com.apple.mail", "charlie"),
        (30, "com.apple.Safari", "delta"),
    ];

    #[test]
    fn forget_before() {
        let store = Store::open_in_memory().unwrap();
        store_with(&store, SAMPLE);
        assert_eq!(store.forget(&Forget::Before(at_minute(20))).unwrap(), 2);
        assert_eq!(remaining(&store), ["charlie", "delta"]);
    }

    #[test]
    fn forget_since() {
        let store = Store::open_in_memory().unwrap();
        store_with(&store, SAMPLE);
        assert_eq!(store.forget(&Forget::Since(at_minute(20))).unwrap(), 2);
        assert_eq!(remaining(&store), ["alpha", "bravo"]);
    }

    #[test]
    fn forget_between() {
        let store = Store::open_in_memory().unwrap();
        store_with(&store, SAMPLE);
        let range = Forget::Between(at_minute(10), at_minute(30));
        assert_eq!(store.forget(&range).unwrap(), 2);
        assert_eq!(remaining(&store), ["alpha", "delta"]);
    }

    #[test]
    fn forget_app() {
        let store = Store::open_in_memory().unwrap();
        store_with(&store, SAMPLE);
        let app = Forget::App("COM.APPLE.MAIL".to_string());
        assert_eq!(store.forget(&app).unwrap(), 2);
        assert_eq!(remaining(&store), ["bravo", "delta"]);
    }

    #[test]
    fn forget_everything() {
        let store = Store::open_in_memory().unwrap();
        store_with(&store, SAMPLE);
        assert_eq!(store.forget(&Forget::Everything).unwrap(), 4);
        assert!(remaining(&store).is_empty());
    }

    #[test]
    fn forgotten_captures_leave_the_search_index() {
        let store = Store::open_in_memory().unwrap();
        store_with(&store, SAMPLE);
        store
            .forget(&Forget::App("com.apple.mail".to_string()))
            .unwrap();
        assert!(store.search("alpha", 10).unwrap().is_empty());
        assert!(store.search("charlie", 10).unwrap().is_empty());
        assert_eq!(store.search("bravo", 10).unwrap().len(), 1);
    }

    #[test]
    fn forgotten_text_is_gone_from_the_files_on_disk() {
        let tmp = TempDir::new();
        let store = Store::open(&tmp.0).unwrap();
        // A word that appears nowhere else, so finding its bytes anywhere in
        // the files means the text survived.
        let marker = "zebrafinkmarker";
        for _ in 0..50 {
            store_with(&store, &[(0, "com.apple.mail", marker)]);
        }
        store_with(&store, &[(60, "com.apple.Safari", "keep")]);
        store.forget(&Forget::Before(at_minute(30))).unwrap();

        for entry in fs::read_dir(&tmp.0).unwrap() {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            let found = bytes.windows(marker.len()).any(|w| w == marker.as_bytes());
            assert!(!found, "{} still contains forgotten text", path.display());
        }
        assert_eq!(remaining(&store), ["keep"]);
    }

    #[test]
    fn expired_text_is_gone_from_the_files_on_disk() {
        let tmp = TempDir::new();
        let store = Store::open(&tmp.0).unwrap();
        store.set_retention_days(1, at_minute(0)).unwrap();
        let marker = "zebrafinkexpired";
        for _ in 0..50 {
            store_with(&store, &[(0, "com.apple.mail", marker)]);
        }
        store_with(&store, &[(3000, "com.apple.Safari", "keep")]);
        assert_eq!(store.expire(at_minute(3000)).unwrap(), 50);

        for entry in fs::read_dir(&tmp.0).unwrap() {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            let found = bytes.windows(marker.len()).any(|w| w == marker.as_bytes());
            assert!(!found, "{} still contains expired text", path.display());
        }
    }

    #[test]
    fn lowering_the_retention_deletes_right_away() {
        let store = Store::open_in_memory().unwrap();
        store_with(
            &store,
            &[
                (0, "com.apple.mail", "old"),
                (3000, "com.apple.mail", "new"),
            ],
        );
        store.set_retention_days(1, at_minute(3000)).unwrap();
        assert_eq!(remaining(&store), ["new"]);
    }

    #[test]
    fn retention_defaults_to_90_days() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.retention_days().unwrap(), DEFAULT_RETENTION_DAYS);
        assert_eq!(DEFAULT_RETENTION_DAYS, 90);
    }

    #[test]
    fn retention_can_be_changed_and_is_kept() {
        let tmp = TempDir::new();
        Store::open(&tmp.0)
            .unwrap()
            .set_retention_days(7, Utc::now())
            .unwrap();
        assert_eq!(Store::open(&tmp.0).unwrap().retention_days().unwrap(), 7);
    }

    #[test]
    fn retention_outside_the_limits_is_refused() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.set_retention_days(0, Utc::now()).is_err());
        assert!(
            store
                .set_retention_days(MAX_RETENTION_DAYS + 1, Utc::now())
                .is_err()
        );
        assert_eq!(store.retention_days().unwrap(), DEFAULT_RETENTION_DAYS);
    }

    #[test]
    fn capture_interval_defaults_and_can_be_changed() {
        let tmp = TempDir::new();
        let store = Store::open(&tmp.0).unwrap();
        assert_eq!(
            store.capture_interval_seconds().unwrap(),
            DEFAULT_CAPTURE_INTERVAL_SECONDS
        );
        store.set_capture_interval_seconds(10).unwrap();
        assert_eq!(
            Store::open(&tmp.0)
                .unwrap()
                .capture_interval_seconds()
                .unwrap(),
            10
        );
    }

    #[test]
    fn capture_interval_outside_the_range_is_refused() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.set_capture_interval_seconds(1).is_err());
        assert!(store.set_capture_interval_seconds(301).is_err());
        store
            .conn
            .execute(
                "INSERT INTO settings (key, value) VALUES ('capture_interval_seconds', '0')",
                [],
            )
            .unwrap();
        assert_eq!(
            store.capture_interval_seconds().unwrap(),
            DEFAULT_CAPTURE_INTERVAL_SECONDS
        );
    }

    #[test]
    fn damaged_retention_falls_back_to_the_default() {
        let store = Store::open_in_memory().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, 'forever')",
                [RETENTION_KEY],
            )
            .unwrap();
        assert_eq!(store.retention_days().unwrap(), DEFAULT_RETENTION_DAYS);
    }

    #[test]
    fn expire_deletes_captures_older_than_the_retention() {
        let store = Store::open_in_memory().unwrap();
        store.set_retention_days(1, Utc::now()).unwrap();
        store_with(
            &store,
            &[
                (0, "com.apple.mail", "old"),
                (1500, "com.apple.mail", "new"),
            ],
        );
        // One day after minute 1000: minute 0 is older than a day, 1500 is not.
        let now = at_minute(1000) + TimeDelta::days(1);
        assert_eq!(store.expire(now).unwrap(), 1);
        assert_eq!(remaining(&store), ["new"]);
    }

    #[test]
    fn pause_survives_a_restart() {
        let tmp = TempDir::new();
        let until = at_minute(15);
        let states = [
            CaptureState::Running,
            CaptureState::Paused { until: None },
            CaptureState::Paused { until: Some(until) },
            CaptureState::Running,
        ];
        for state in states {
            Store::open(&tmp.0).unwrap().set_pause_state(state).unwrap();
            assert_eq!(Store::open(&tmp.0).unwrap().pause_state().unwrap(), state);
        }
    }

    #[test]
    fn a_damaged_pause_keeps_capture_paused() {
        let store = Store::open_in_memory().unwrap();
        store.set_setting(PAUSE_KEY, "garbage").unwrap();
        assert_eq!(
            store.pause_state().unwrap(),
            CaptureState::Paused { until: None }
        );
    }
}
