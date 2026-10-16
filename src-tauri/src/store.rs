//! Local storage for captures (GUARDRAILS §6).
//!
//! Everything lives in one data folder that only the user can open (`0700`).
//! The database is SQLite with an FTS5 index, so it can be inspected without
//! Lax. See docs/adr/0006-sqlite-with-rusqlite.md. Lax is built with
//! SQLCipher's SQLite, but databases have no key yet, so they are still
//! plaintext (docs/adr/0010-encrypted-database.md, rollout step 2).
//!
//! The store redacts titles and text itself before writing them, so a caller
//! can't store a secret by forgetting to redact (GUARDRAILS §4).

use std::cell::RefCell;
use std::fmt;
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use chrono::{DateTime, TimeDelta, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::exclusions::Exclusions;
use crate::pause::CaptureState;
use crate::redact::redact;

mod ask;
#[cfg(target_os = "macos")]
mod cipher;
mod memory;
mod pages;
mod pairs;
mod vectors;
#[cfg(target_os = "macos")]
pub use cipher::{FileKind, Opened, Postponed, conversion_fits, file_kind};
pub use memory::{Entity, EntityKind, Merged, Renamed, UnrecognizedEpisode, name_key};
#[cfg(test)]
pub use pages::{LAX_VERSIONS_KEPT, USER_EDITS_KEPT};
pub use pages::{MovedFile, MovedPage, Page, PageKind, PageState, fact_key, facts_in};
#[cfg(test)]
pub use pairs::Signal;
pub use pairs::{Candidate, ObviousPair, Suggestion, Verdict};
#[cfg(test)]
pub use vectors::cosine;

pub const DATABASE_FILE: &str = "lax.sqlite";

/// Schema changes, applied in order. The database's `user_version` is the
/// number of migrations it has.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_captures.sql"),
    include_str!("../migrations/0002_settings.sql"),
    include_str!("../migrations/0003_exclusions.sql"),
    include_str!("../migrations/0004_outbox.sql"),
    include_str!("../migrations/0005_journal.sql"),
    include_str!("../migrations/0006_memory.sql"),
    include_str!("../migrations/0007_memory_choices.sql"),
    include_str!("../migrations/0008_pages.sql"),
    include_str!("../migrations/0009_page_facts.sql"),
    include_str!("../migrations/0010_about_me.sql"),
    include_str!("../migrations/0011_episode_vectors.sql"),
    include_str!("../migrations/0012_page_text.sql"),
    include_str!("../migrations/0013_page_edits.sql"),
    include_str!("../migrations/0014_moved_page_files.sql"),
    include_str!("../migrations/0015_not_same.sql"),
    include_str!("../migrations/0016_pair_verdicts.sql"),
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
const OUTBOX_RETENTION_KEY: &str = "outbox_retention_days";
const JOURNAL_KEY: &str = "journal_enabled";
/// How long raw captures are kept once the journal has summarized them
/// (decided with the maintainer, #39). Unsummarized ones are kept up to the
/// retention setting, so nothing is lost before the journal has read it.
pub const SUMMARIZED_KEPT_DAYS: i64 = 30;
const WRITING_KEY: &str = "writing_enabled";
const WRITING_CALL_KEY: &str = "writing_key";
const SETUP_KEY: &str = "setup_finished";
/// How long the outbox log is kept unless the user chooses otherwise.
pub const DEFAULT_OUTBOX_RETENTION_DAYS: u32 = 90;
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
    /// The keychain wouldn't hand over the key: locked, the prompt was
    /// denied or cancelled, or something unexpected. The key is probably
    /// fine, so trying again is the way out. Holds no key and no SQLite
    /// text, which could (ADR 0010, "No key in error messages").
    KeyUnavailable,
    /// The database is encrypted and its key isn't in the keychain.
    KeyMissing,
    /// The key doesn't open the database: a wrong key, or a damaged file.
    /// The two look the same.
    CannotUnlock,
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
            StoreError::KeyUnavailable => f.write_str("Lax can't read its key from the keychain"),
            StoreError::KeyMissing => f.write_str("Lax can't find its key in the keychain"),
            StoreError::CannotUnlock => {
                f.write_str("the database doesn't open with Lax's key (a wrong key, or damaged)")
            }
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

/// A capture as the viewer lists it: the start of its text, not all of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSummary {
    pub id: i64,
    pub captured_at: DateTime<Utc>,
    pub app_name: String,
    pub bundle_id: String,
    pub window_title: String,
    pub site: Option<String>,
    /// At most [`PREVIEW_CHARS`] characters from the start of the text.
    pub preview: String,
    /// The length of the whole text, in characters.
    pub text_chars: usize,
}

/// A search result: the capture, and the part of it that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub capture: CaptureSummary,
    /// A few words around the match. Each matched word is enclosed in
    /// [`MATCH_START`] and [`MATCH_END`].
    pub snippet: String,
}

pub const PREVIEW_CHARS: usize = 240;
/// Mark matched words in [`SearchHit::snippet`]. Control characters, so they
/// can't be confused with the text, which never reaches the web UI as HTML.
pub const MATCH_START: char = '\u{2}';
pub const MATCH_END: char = '\u{3}';
/// Words of context around a match in a snippet.
const SNIPPET_WORDS: i64 = 16;

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

impl Forget {
    /// Whether this covers a capture made at `at` in the app `bundle_id`.
    fn covers(&self, at: DateTime<Utc>, bundle_id: &str) -> bool {
        match self {
            Forget::Before(t) => at < *t,
            Forget::Since(t) => at >= *t,
            Forget::Between(from, to) => at >= *from && at < *to,
            Forget::App(app) => app.eq_ignore_ascii_case(bundle_id),
            Forget::Everything => true,
        }
    }
}

/// `PRAGMA auto_vacuum`'s value for INCREMENTAL.
const INCREMENTAL: i64 = 2;

/// How long a forget is remembered, to refuse captures read before it but
/// handed to the store after it. Capture holds a capture back for at most
/// 30 seconds, so this leaves plenty of room.
const FORGET_MEMORY: TimeDelta = TimeDelta::minutes(10);

/// Whether a connection has a key, for the settings that differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    Plaintext,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Encrypted,
}

/// The database file in `data_dir`, private from the start: the folder and
/// an empty file are made `0700` and `0600` before SQLite opens it, not
/// chmod-ed afterwards.
fn database_file(data_dir: &Path) -> Result<std::path::PathBuf> {
    prepare_data_dir(data_dir)?;
    let path = data_dir.join(DATABASE_FILE);
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)?;
    fs::set_permissions(&path, Permissions::from_mode(0o600))?;
    Ok(path)
}

pub struct Store {
    conn: Connection,
    /// Recent forgets and when they happened.
    recent_forgets: RefCell<Vec<(Forget, DateTime<Utc>)>>,
    /// Whether the database has a key (ADR 0010). The pages move in only
    /// then (ADR 0011).
    encrypted: bool,
}

impl Store {
    /// Opens (or creates) the database in `data_dir`, without a key.
    pub fn open(data_dir: &Path) -> Result<Self> {
        let conn = Connection::open(database_file(data_dir)?)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, Layout::Plaintext)
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, Layout::Plaintext)
    }

    /// A new database without a key but laid out like an encrypted one
    /// (`auto_vacuum = INCREMENTAL`), so the tests that read the files for
    /// forgotten text can still see through it (ADR 0010, "Tests").
    #[cfg(test)]
    pub(crate) fn open_incremental(data_dir: &Path) -> Result<Self> {
        let conn = Connection::open(database_file(data_dir)?)?;
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, Layout::Plaintext)
    }

    fn init(mut conn: Connection, layout: Layout) -> Result<Self> {
        // Overwrite deleted content instead of leaving it in free pages.
        conn.pragma_update(None, "secure_delete", "ON")?;
        // SQLCipher encrypts the database, its journal and the write-ahead
        // log, but not temporary files, so an encrypted database keeps
        // temporary data in memory. A plaintext one keeps it in files, as
        // before: they hold nothing the database doesn't, and VACUUM in
        // `scrub` would otherwise build its copy of the whole database in
        // RAM (ADR 0010, "Settings").
        let temp_store = match layout {
            Layout::Plaintext => "FILE",
            Layout::Encrypted => "MEMORY",
        };
        conn.pragma_update(None, "temp_store", temp_store)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn)?;
        repair(&conn)?;
        Ok(Store {
            conn,
            recent_forgets: RefCell::new(Vec::new()),
            encrypted: layout == Layout::Encrypted,
        })
    }

    /// Whether the database is encrypted (ADR 0010).
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    /// For tests: as if the database were encrypted, so the pages may move
    /// in.
    #[cfg(test)]
    pub(crate) fn pretend_encrypted(&mut self) {
        self.encrypted = true;
    }

    /// Stores a capture and returns its id. The window title and text are
    /// redacted here, and a site that isn't a bare host is dropped.
    ///
    /// A capture that was read before the user forgot the time or app it
    /// belongs to is refused (`None`): forgetting takes effect immediately
    /// (GUARDRAILS §4), also for what capture had read but not yet stored.
    pub fn insert(&self, capture: &NewCapture) -> Result<Option<i64>> {
        if self.forgotten(capture, Utc::now()) {
            return Ok(None);
        }
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
        Ok(Some(self.conn.last_insert_rowid()))
    }

    fn forgotten(&self, capture: &NewCapture, now: DateTime<Utc>) -> bool {
        let mut forgets = self.recent_forgets.borrow_mut();
        forgets.retain(|(_, at)| now - *at < FORGET_MEMORY);
        forgets.iter().any(|(what, at)| {
            capture.captured_at < *at && what.covers(capture.captured_at, capture.bundle_id)
        })
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

/// What a model is used for. Chat and embeddings are configured separately
/// (GUARDRAILS §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTask {
    Chat,
    Embeddings,
}

impl ModelTask {
    fn as_str(self) -> &'static str {
        match self {
            ModelTask::Chat => "chat",
            ModelTask::Embeddings => "embeddings",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub base_url: String,
    pub model: String,
}

/// An episode about to be stored, before it has an id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEpisode {
    pub summary: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
}

/// A stored episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    pub id: i64,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub summary: String,
    pub app_names: Vec<String>,
    pub captures: usize,
}

/// The journal (Stage 3).
impl Store {
    /// Captures not yet summarized, oldest first, captured before `before`.
    pub fn unsummarized(&self, before: DateTime<Utc>, limit: usize) -> Result<Vec<Capture>> {
        let mut statement = self.conn.prepare(
            "SELECT id, captured_at, app_name, bundle_id, window_title, site, text
             FROM captures
             WHERE episode_id IS NULL AND captured_at < ?1
             ORDER BY captured_at, id
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![before.timestamp_millis(), sql_limit(limit)],
            capture_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether the user turned the journal on. Off until they do.
    pub fn journal_enabled(&self) -> Result<bool> {
        Ok(self.setting(JOURNAL_KEY)?.as_deref() == Some("on"))
    }

    pub fn set_journal_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting(JOURNAL_KEY, if enabled { "on" } else { "off" })
    }

    /// Whether the user turned the writing assistant on (ADR 0009). Off
    /// until they do.
    pub fn writing_enabled(&self) -> Result<bool> {
        Ok(self.setting(WRITING_KEY)?.as_deref() == Some("on"))
    }

    pub fn set_writing_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting(WRITING_KEY, if enabled { "on" } else { "off" })
    }

    /// Whether the user finished the first-start guide.
    pub fn setup_finished(&self) -> Result<bool> {
        Ok(self.setting(SETUP_KEY)?.as_deref() == Some("yes"))
    }

    pub fn set_setup_finished(&self) -> Result<()> {
        self.set_setting(SETUP_KEY, "yes")
    }

    /// The name of the key the user double-taps to call the writing
    /// assistant, if they picked one.
    pub fn writing_key(&self) -> Result<Option<String>> {
        self.setting(WRITING_CALL_KEY)
    }

    pub fn set_writing_key(&self, key: &str) -> Result<()> {
        self.set_setting(WRITING_CALL_KEY, key)
    }

    /// How many captures wait to be summarized.
    pub fn unsummarized_count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM captures WHERE episode_id IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// Stores `episode` as the summary of `captures` and marks them
    /// summarized. Refused (`None`) if any of them is gone or already
    /// summarized: the user may have forgotten them while the model was
    /// writing, and nothing may be kept from them.
    pub fn record_episode(
        &mut self,
        episode: &NewEpisode,
        captures: &[i64],
    ) -> Result<Option<i64>> {
        if captures.is_empty() {
            return Ok(None);
        }
        let tx = self.conn.transaction()?;
        // Ids are numbers from the database, so joining them is safe.
        let ids = captures
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let (present, started, ended): (i64, Option<i64>, Option<i64>) = tx.query_row(
            &format!(
                "SELECT count(*), min(captured_at), max(captured_at)
                 FROM captures WHERE id IN ({ids}) AND episode_id IS NULL"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let (Some(started), Some(ended)) = (started, ended) else {
            return Ok(None);
        };
        if usize::try_from(present).unwrap_or(0) != captures.len() {
            return Ok(None);
        }
        let mut apps: Vec<String> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        {
            let mut statement = tx.prepare(&format!(
                "SELECT bundle_id, app_name FROM captures WHERE id IN ({ids})
                 ORDER BY captured_at"
            ))?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (bundle_id, name) = row?;
                if !apps.iter().any(|a| a.eq_ignore_ascii_case(&bundle_id)) {
                    apps.push(bundle_id);
                    names.push(name);
                }
            }
        }
        tx.execute(
            "INSERT INTO episodes (started_at, ended_at, summary, apps, app_names, model, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                started,
                ended,
                episode.summary,
                apps.join("\n"),
                names.join("\n"),
                episode.model,
                episode.created_at.timestamp_millis(),
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            &format!("UPDATE captures SET episode_id = ?1 WHERE id IN ({ids})"),
            [id],
        )?;
        tx.commit()?;
        Ok(Some(id))
    }

    /// Episodes newest first, at most `limit`; `before` continues a listing.
    pub fn episodes(
        &self,
        before: Option<(DateTime<Utc>, i64)>,
        limit: usize,
    ) -> Result<Vec<Episode>> {
        let (before_ms, before_id) = before
            .map(|(at, id)| (at.timestamp_millis(), id))
            .unwrap_or((i64::MAX, i64::MAX));
        let mut statement = self.conn.prepare(
            "SELECT e.id, e.started_at, e.ended_at, e.summary, e.app_names,
                    (SELECT count(*) FROM captures AS c WHERE c.episode_id = e.id)
             FROM episodes AS e
             WHERE (e.started_at, e.id) < (?1, ?2)
             ORDER BY e.started_at DESC, e.id DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![before_ms, before_id, sql_limit(limit)],
            episode_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// The columns `id, started_at, ended_at, summary, app_names, captures` as
/// an [`Episode`].
fn episode_from_row(row: &Row) -> rusqlite::Result<Episode> {
    let started: i64 = row.get(1)?;
    let ended: i64 = row.get(2)?;
    let names: String = row.get(4)?;
    let count: i64 = row.get(5)?;
    Ok(Episode {
        id: row.get(0)?,
        started_at: DateTime::from_timestamp_millis(started).unwrap_or_default(),
        ended_at: DateTime::from_timestamp_millis(ended).unwrap_or_default(),
        summary: row.get(3)?,
        app_names: names
            .lines()
            .filter(|n| !n.is_empty())
            .map(String::from)
            .collect(),
        captures: usize::try_from(count).unwrap_or(0),
    })
}

/// One request to a remote provider, as it was sent (GUARDRAILS §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    pub sent_at: DateTime<Utc>,
    pub provider: String,
    pub task: String,
    pub model: String,
    /// The request body exactly as sent, after redaction.
    pub payload: String,
    /// HTTP status, 0 if nothing came back.
    pub status: u16,
    pub response_bytes: usize,
    /// The earliest and latest capture time the request was made from, if
    /// it was made from captures.
    pub covers: Option<(DateTime<Utc>, DateTime<Utc>)>,
    /// The apps (bundle ids) whose captures it held.
    pub apps: Vec<String>,
}

/// An outbox entry as the window lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxSummary {
    pub id: i64,
    pub sent_at: DateTime<Utc>,
    pub provider: String,
    pub task: String,
    pub model: String,
    pub preview: String,
    pub payload_chars: usize,
    pub status: u16,
    pub response_bytes: usize,
}

/// The outbox log.
impl Store {
    pub fn record_outbox(&self, entry: &OutboxEntry) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO outbox (sent_at, provider, task, model, payload, status, response_bytes,
                                 covers_from, covers_to, apps)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                entry.sent_at.timestamp_millis(),
                entry.provider,
                entry.task,
                entry.model,
                entry.payload,
                entry.status,
                i64::try_from(entry.response_bytes).unwrap_or(i64::MAX),
                entry.covers.map(|(from, _)| from.timestamp_millis()),
                entry.covers.map(|(_, to)| to.timestamp_millis()),
                entry.apps.join("\n"),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Entries newest first, at most `limit`; with `query`, only those whose
    /// payload matches it. `before` continues a listing.
    pub fn outbox(
        &self,
        query: Option<&str>,
        before: Option<(DateTime<Utc>, i64)>,
        limit: usize,
    ) -> Result<Vec<OutboxSummary>> {
        let (before_ms, before_id) = before
            .map(|(at, id)| (at.timestamp_millis(), id))
            .unwrap_or((i64::MAX, i64::MAX));
        let fts = query.and_then(match_query);
        let sql = format!(
            "SELECT o.id, o.sent_at, o.provider, o.task, o.model, substr(o.payload, 1, ?1),
                    length(o.payload), o.status, o.response_bytes
             FROM outbox AS o {}
             WHERE (o.sent_at, o.id) < (?2, ?3) {}
             ORDER BY o.sent_at DESC, o.id DESC
             LIMIT ?4",
            if fts.is_some() {
                "JOIN outbox_fts ON outbox_fts.rowid = o.id"
            } else {
                ""
            },
            if fts.is_some() {
                "AND outbox_fts MATCH ?5"
            } else {
                ""
            },
        );
        let mut statement = self.conn.prepare(&sql)?;
        let map = |row: &Row| -> rusqlite::Result<OutboxSummary> {
            let millis: i64 = row.get(1)?;
            let chars: i64 = row.get(6)?;
            let bytes: i64 = row.get(8)?;
            Ok(OutboxSummary {
                id: row.get(0)?,
                sent_at: DateTime::from_timestamp_millis(millis).unwrap_or_default(),
                provider: row.get(2)?,
                task: row.get(3)?,
                model: row.get(4)?,
                preview: row.get(5)?,
                payload_chars: usize::try_from(chars).unwrap_or(0),
                status: row.get(7)?,
                response_bytes: usize::try_from(bytes).unwrap_or(0),
            })
        };
        let preview = PREVIEW_CHARS as i64;
        let limit = sql_limit(limit);
        let rows = match &fts {
            Some(fts) => statement
                .query_map(params![preview, before_ms, before_id, limit, fts], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => statement
                .query_map(params![preview, before_ms, before_id, limit], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    /// The whole payload of one entry.
    pub fn outbox_payload(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT payload FROM outbox WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Deletes one entry (`Some(id)`) or the whole log (`None`), also from
    /// the files on disk. Returns how many entries were deleted.
    pub fn delete_outbox(&self, id: Option<i64>) -> Result<usize> {
        let deleted = match id {
            Some(id) => self
                .conn
                .execute("DELETE FROM outbox WHERE id = ?1", [id])?,
            None => self.conn.execute("DELETE FROM outbox", [])?,
        };
        if deleted > 0 {
            self.scrub()?;
        }
        Ok(deleted)
    }

    pub fn outbox_retention_days(&self) -> Result<u32> {
        let value = self.setting(OUTBOX_RETENTION_KEY)?;
        Ok(value
            .and_then(|v| v.parse().ok())
            .filter(|days| (1..=MAX_RETENTION_DAYS).contains(days))
            .unwrap_or(DEFAULT_OUTBOX_RETENTION_DAYS))
    }

    pub fn set_outbox_retention_days(&self, days: u32, now: DateTime<Utc>) -> Result<()> {
        if !(1..=MAX_RETENTION_DAYS).contains(&days) {
            return Err(StoreError::InvalidRetention(days));
        }
        self.set_setting(OUTBOX_RETENTION_KEY, &days.to_string())?;
        self.expire(now)?;
        Ok(())
    }
}

/// A kind of user exclusion (GUARDRAILS §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionKind {
    /// A bundle id.
    App,
    /// A host; its subdomains are excluded too.
    Site,
    /// Text anywhere in a window title.
    Title,
    /// The name of a tier-2 default the user turned off.
    DefaultOff,
}

impl ExclusionKind {
    fn as_str(self) -> &'static str {
        match self {
            ExclusionKind::App => "app",
            ExclusionKind::Site => "site",
            ExclusionKind::Title => "title",
            ExclusionKind::DefaultOff => "default_off",
        }
    }
}

/// The user's exclusions.
impl Store {
    pub fn exclusions(&self) -> Result<Exclusions> {
        let mut statement = self
            .conn
            .prepare("SELECT kind, value FROM exclusions ORDER BY rowid")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut exclusions = Exclusions::default();
        for row in rows {
            let (kind, value) = row?;
            match kind.as_str() {
                "app" => exclusions.apps.push(value),
                "site" => exclusions.sites.push(value),
                "title" => exclusions.title_patterns.push(value),
                "default_off" => {
                    exclusions.re_enabled_defaults.insert(value);
                }
                // The table only allows the kinds above.
                _ => {}
            }
        }
        Ok(exclusions)
    }

    /// Adds an exclusion. Adding one that exists already changes nothing.
    pub fn add_exclusion(&self, kind: ExclusionKind, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO exclusions (kind, value) VALUES (?1, ?2)",
            params![kind.as_str(), value],
        )?;
        Ok(())
    }

    pub fn remove_exclusion(&self, kind: ExclusionKind, value: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM exclusions WHERE kind = ?1 AND value = ?2",
            params![kind.as_str(), value],
        )?;
        Ok(())
    }
}

/// Reading for the viewer (#10).
impl Store {
    /// Captures from newest to oldest, at most `limit`. `before` continues a
    /// listing after the last capture shown, by its time and id.
    pub fn recent(
        &self,
        before: Option<(DateTime<Utc>, i64)>,
        limit: usize,
    ) -> Result<Vec<CaptureSummary>> {
        let (before_ms, before_id) = before
            .map(|(at, id)| (at.timestamp_millis(), id))
            .unwrap_or((i64::MAX, i64::MAX));
        let mut statement = self.conn.prepare(
            "SELECT id, captured_at, app_name, bundle_id, window_title, site,
                    substr(text, 1, ?1), length(text)
             FROM captures
             WHERE (captured_at, id) < (?2, ?3)
             ORDER BY captured_at DESC, id DESC
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![PREVIEW_CHARS as i64, before_ms, before_id, sql_limit(limit)],
            summary_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One capture with all its text.
    pub fn get(&self, id: i64) -> Result<Option<Capture>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, captured_at, app_name, bundle_id, window_title, site, text
                 FROM captures WHERE id = ?1",
                [id],
                capture_from_row,
            )
            .optional()?)
    }

    /// Like [`Store::search`], with a snippet of the matching text instead of
    /// all of it.
    pub fn search_hits(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let Some(query) = match_query(query) else {
            return Ok(Vec::new());
        };
        let mut statement = self.conn.prepare(
            "SELECT c.id, c.captured_at, c.app_name, c.bundle_id, c.window_title, c.site,
                    substr(c.text, 1, ?2), length(c.text),
                    snippet(captures_fts, -1, ?3, ?4, '…', ?5)
             FROM captures_fts
             JOIN captures AS c ON c.id = captures_fts.rowid
             WHERE captures_fts MATCH ?1
             ORDER BY rank, c.captured_at DESC
             LIMIT ?6",
        )?;
        let rows = statement.query_map(
            params![
                query,
                PREVIEW_CHARS as i64,
                MATCH_START.to_string(),
                MATCH_END.to_string(),
                SNIPPET_WORDS,
                sql_limit(limit)
            ],
            |row| {
                Ok(SearchHit {
                    capture: summary_from_row(row)?,
                    snippet: row.get(8)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// An app Lax has captured from, for "forget this app".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedApp {
    pub bundle_id: String,
    /// The most recent name it had.
    pub app_name: String,
    pub captures: usize,
}

impl Store {
    /// Every app with captures, the most captured first.
    pub fn apps(&self) -> Result<Vec<CapturedApp>> {
        let mut statement = self.conn.prepare(
            // With max(), SQLite takes the bare columns from the row that
            // has the maximum, so app_name is the latest name.
            "SELECT bundle_id, app_name, count(*), max(captured_at)
             FROM captures
             GROUP BY bundle_id COLLATE NOCASE
             ORDER BY count(*) DESC, bundle_id",
        )?;
        let rows = statement.query_map([], |row| {
            let count: i64 = row.get(2)?;
            Ok(CapturedApp {
                bundle_id: row.get(0)?,
                app_name: row.get(1)?,
                captures: usize::try_from(count).unwrap_or(0),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn sql_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

fn summary_from_row(row: &Row) -> rusqlite::Result<CaptureSummary> {
    let millis: i64 = row.get(1)?;
    let chars: i64 = row.get(7)?;
    Ok(CaptureSummary {
        id: row.get(0)?,
        captured_at: DateTime::from_timestamp_millis(millis).unwrap_or_default(),
        app_name: row.get(2)?,
        bundle_id: row.get(3)?,
        window_title: row.get(4)?,
        site: row.get(5)?,
        preview: row.get(6)?,
        text_chars: usize::try_from(chars).unwrap_or(0),
    })
}

impl Store {
    /// Deletes what `what` covers, including everything derived from it, and
    /// makes sure the deleted text is gone from the database files. Returns
    /// how many captures were deleted.
    ///
    /// What is derived from the deleted captures is deleted here too
    /// (GUARDRAILS §4): the outbox entries and episodes made from them, and
    /// with the episodes, the memory's mentions of entities and the entities
    /// that only they mentioned (a trigger in `0006_memory.sql`).
    pub fn forget(&self, what: &Forget) -> Result<usize> {
        let deleted = self.delete(what)?;
        // What was sent off the machine about the forgotten captures goes
        // too. (Everything already includes the outbox.)
        let outbox_deleted = self.forget_in_outbox(what)?;
        let episodes_deleted = self.forget_in_journal(what)?;
        // Remembered once the rows are gone, so a failed delete refuses
        // nothing. The store is behind a lock, so no capture can be stored
        // in between.
        self.recent_forgets
            .borrow_mut()
            .push((what.clone(), Utc::now()));
        if deleted + outbox_deleted + episodes_deleted > 0 {
            self.scrub()?;
        }
        Ok(deleted)
    }

    /// Episodes made from captures that `what` covers. Captures of other
    /// apps or times in them are summarized again on the next run.
    fn forget_in_journal(&self, what: &Forget) -> Result<usize> {
        Ok(match what {
            Forget::Before(t) => self.conn.execute(
                "DELETE FROM episodes WHERE started_at < ?1",
                [t.timestamp_millis()],
            )?,
            Forget::Since(t) => self.conn.execute(
                "DELETE FROM episodes WHERE ended_at >= ?1",
                [t.timestamp_millis()],
            )?,
            Forget::Between(from, to) => self.conn.execute(
                "DELETE FROM episodes WHERE started_at < ?2 AND ended_at >= ?1",
                [from.timestamp_millis(), to.timestamp_millis()],
            )?,
            Forget::App(bundle_id) => self.conn.execute(
                "DELETE FROM episodes
                 WHERE instr(char(10) || lower(apps) || char(10),
                             char(10) || lower(?1) || char(10)) > 0",
                [bundle_id],
            )?,
            Forget::Everything => 0,
        })
    }

    /// Outbox entries made from captures that `what` covers.
    fn forget_in_outbox(&self, what: &Forget) -> Result<usize> {
        Ok(match what {
            Forget::Before(t) => self.conn.execute(
                "DELETE FROM outbox WHERE covers_from < ?1",
                [t.timestamp_millis()],
            )?,
            Forget::Since(t) => self.conn.execute(
                "DELETE FROM outbox WHERE covers_to >= ?1",
                [t.timestamp_millis()],
            )?,
            Forget::Between(from, to) => self.conn.execute(
                "DELETE FROM outbox WHERE covers_from < ?2 AND covers_to >= ?1",
                [from.timestamp_millis(), to.timestamp_millis()],
            )?,
            Forget::App(bundle_id) => self.conn.execute(
                "DELETE FROM outbox
                 WHERE instr(char(10) || lower(apps) || char(10),
                             char(10) || lower(?1) || char(10)) > 0",
                [bundle_id],
            )?,
            Forget::Everything => 0,
        })
    }

    /// Deletes captures older than the retention period.
    ///
    /// This runs every hour, so it skips the full rebuild that [`Store::forget`]
    /// does. Deleted text is still gone from the files: `secure_delete`
    /// overwrites the freed pages, FTS5 secure-delete removes it from the index,
    /// and the checkpoint empties the write-ahead log.
    /// Deletes raw captures that have done their job (#39): those the
    /// journal summarized more than [`SUMMARIZED_KEPT_DAYS`] ago (counted
    /// from when they were captured), and every capture older than the
    /// retention setting, summarized or not. The episodes made from them
    /// stay: the knowledge lives on in the journal and the memory.
    pub fn expire(&self, now: DateTime<Utc>) -> Result<usize> {
        let days = i64::from(self.retention_days()?);
        let mut deleted = self.delete(&Forget::Before(now - TimeDelta::days(days)))?;
        let summarized_days = SUMMARIZED_KEPT_DAYS.min(days);
        deleted += self.conn.execute(
            "DELETE FROM captures WHERE episode_id IS NOT NULL AND captured_at < ?1",
            [(now - TimeDelta::days(summarized_days)).timestamp_millis()],
        )?;
        let outbox_days = i64::from(self.outbox_retention_days()?);
        let outbox_deleted = self.conn.execute(
            "DELETE FROM outbox WHERE sent_at < ?1",
            [(now - TimeDelta::days(outbox_days)).timestamp_millis()],
        )?;
        if deleted + outbox_deleted > 0 {
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
            Forget::Everything => {
                // What was sent off the machine, and the journal written
                // from the captures, are part of "everything".
                self.conn.execute("DELETE FROM outbox", [])?;
                self.conn.execute("DELETE FROM episodes", [])?;
                // The names the user asked Lax not to learn are names too.
                self.conn.execute("DELETE FROM dont_learn", [])?;
                self.conn.execute("DELETE FROM captures", [])?
            }
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

    /// The model chosen for `task`: a base address and a model id. Settings
    /// only, never a key (keys live in the Keychain, GUARDRAILS §3).
    pub fn model_choice(&self, task: ModelTask) -> Result<Option<ModelChoice>> {
        let base_url = self.setting(&format!("model.{}.base_url", task.as_str()))?;
        let model = self.setting(&format!("model.{}.model", task.as_str()))?;
        Ok(match (base_url, model) {
            (Some(base_url), Some(model)) => Some(ModelChoice { base_url, model }),
            _ => None,
        })
    }

    pub fn set_model_choice(&self, task: ModelTask, choice: Option<&ModelChoice>) -> Result<()> {
        let keys = [
            format!("model.{}.base_url", task.as_str()),
            format!("model.{}.model", task.as_str()),
        ];
        match choice {
            Some(choice) => {
                self.set_setting(&keys[0], &choice.base_url)?;
                self.set_setting(&keys[1], &choice.model)
            }
            None => {
                for key in &keys {
                    self.conn
                        .execute("DELETE FROM settings WHERE key = ?1", [key])?;
                }
                Ok(())
            }
        }
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
    /// already overwritten the freed pages and cells. A database laid out
    /// for incremental vacuum (every encrypted one) gives its free pages
    /// back without a copy; any other is rebuilt by VACUUM, which with
    /// temporary data in memory would copy the whole database into RAM
    /// (ADR 0010, "Forgetting without VACUUM"). The checkpoint empties the
    /// write-ahead log, which can still hold old copies of pages.
    fn scrub(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO captures_fts (captures_fts) VALUES ('optimize')",
            [],
        )?;
        self.conn.execute(
            "INSERT INTO outbox_fts (outbox_fts) VALUES ('optimize')",
            [],
        )?;
        self.conn.execute(
            "INSERT INTO episodes_fts (episodes_fts) VALUES ('optimize')",
            [],
        )?;
        let auto_vacuum: i64 = self
            .conn
            .pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
        if auto_vacuum == INCREMENTAL {
            // One page per step, so it is stepped until it has no more.
            let mut statement = self.conn.prepare("PRAGMA incremental_vacuum")?;
            let mut rows = statement.query([])?;
            while rows.next()?.is_some() {}
        } else {
            self.conn.execute_batch("VACUUM")?;
        }
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

/// Brings back schema that a database can lack although its version says it
/// has it: a few migrations were changed after they had already run on the
/// maintainer's database during development, so his `outbox` lacked the
/// columns forgetting needs, and `captures` an index. Safe to run every time.
/// From now on, a migration is never changed once merged; `migration_files_
/// are_never_changed` holds that.
fn repair(conn: &Connection) -> Result<()> {
    let columns = |table: &str| -> Result<Vec<String>> {
        let mut statement =
            conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    };
    let outbox = columns("outbox")?;
    let mut added = false;
    for (column, definition) in [
        ("covers_from", "covers_from INTEGER"),
        ("covers_to", "covers_to INTEGER"),
        ("apps", "apps TEXT NOT NULL DEFAULT ''"),
    ] {
        if !outbox.iter().any(|c| c == column) {
            conn.execute_batch(&format!("ALTER TABLE outbox ADD COLUMN {definition}"))?;
            added = true;
        }
    }
    if added {
        // Rows from before don't say which captures they came from. They are
        // taken to cover everything up to when they were sent, so any
        // forget of a time they might hold removes them: forgetting too much
        // of the outbox is better than keeping something forgotten.
        conn.execute(
            "UPDATE outbox SET covers_from = 0, covers_to = sent_at WHERE covers_from IS NULL",
            [],
        )?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS captures_by_app ON captures (bundle_id COLLATE NOCASE)",
    )?;
    Ok(())
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

    /// SQLCipher encrypts with Apple's CommonCrypto. It would switch to
    /// OpenSSL without a word if `OPENSSL_DIR`, or `OPENSSL_LIB_DIR` and
    /// `OPENSSL_INCLUDE_DIR`, were set at build time, also with a target
    /// prefix (`AARCH64_APPLE_DARWIN_OPENSSL_DIR`). Its build script doesn't
    /// rerun when they change, so a stale `target/` keeps OpenSSL until
    /// `cargo clean -p libsqlite3-sys` (ADR 0010).
    #[test]
    fn cipher_provider_is_commoncrypto() {
        let tmp = TempDir::new();
        fs::create_dir_all(&tmp.0).unwrap();
        let path = tmp.0.join("keyed.sqlite");
        let conn = Connection::open(&path).unwrap();
        // A fixed test key in the raw form, which skips key derivation.
        conn.execute_batch(&format!(
            "PRAGMA key = \"x'{}'\"; CREATE TABLE t (x TEXT); INSERT INTO t VALUES ('hello');",
            "ab".repeat(32)
        ))
        .unwrap();
        let provider: String = conn
            .query_row("PRAGMA cipher_provider", [], |r| r.get(0))
            .unwrap();
        assert_eq!(provider, "commoncrypto");
        drop(conn);
        let bytes = fs::read(&path).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3\0"));
    }

    #[test]
    fn the_database_is_plaintext_until_it_has_a_key() {
        let tmp = TempDir::new();
        let store = Store::open(&tmp.0).unwrap();
        // SQLCipher 4 is linked in, its defaults are the ones ADR 0010 names...
        let version: String = store
            .conn
            .query_row("PRAGMA cipher_version", [], |r| r.get(0))
            .unwrap();
        assert!(version.starts_with("4."), "{version}");
        // ...but without a key it encrypts nothing.
        let provider: Option<String> = store
            .conn
            .query_row("PRAGMA cipher_provider", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert_eq!(provider, None);
        store.insert(&capture("Inbox", "hello")).unwrap();
        drop(store);
        let bytes = fs::read(tmp.0.join(DATABASE_FILE)).unwrap();
        assert!(bytes.starts_with(b"SQLite format 3\0"));
    }

    /// SQLCipher's build would keep temporary data in memory, and VACUUM
    /// would then copy the whole database into RAM.
    #[test]
    fn temporary_data_goes_to_files_while_the_database_is_plaintext() {
        let tmp = TempDir::new();
        let store = Store::open(&tmp.0).unwrap();
        let temp_store: i64 = store
            .conn
            .pragma_query_value(None, "temp_store", |r| r.get(0))
            .unwrap();
        assert_eq!(temp_store, 1, "temp_store should be FILE");
    }

    /// SQLCipher's logging is compiled out (`SQLCIPHER_OMIT_LOG` in
    /// `.cargo/config.toml`), so it refuses every log target, even "off".
    #[test]
    fn sqlcipher_cannot_log() {
        let conn = Connection::open_in_memory().unwrap();
        let status: String = conn
            .query_row("PRAGMA cipher_log = off", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "1", "SQLCipher was built with its logging");
    }

    #[test]
    fn a_database_from_an_earlier_outbox_is_repaired() {
        // The outbox as an early version of 0004 made it, before the columns
        // forgetting needs; and no captures_by_app index.
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..4] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(
            "DROP INDEX captures_by_app;
             DROP TABLE outbox_fts; DROP TABLE outbox;
             CREATE TABLE outbox (
                 id INTEGER PRIMARY KEY, sent_at INTEGER NOT NULL, provider TEXT NOT NULL,
                 task TEXT NOT NULL, model TEXT NOT NULL, payload TEXT NOT NULL,
                 status INTEGER NOT NULL, response_bytes INTEGER NOT NULL) STRICT;
             CREATE VIRTUAL TABLE outbox_fts USING fts5(payload, content = 'outbox',
                 content_rowid = 'id', tokenize = 'unicode61 remove_diacritics 0');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outbox (sent_at, provider, task, model, payload, status, response_bytes)
             VALUES (1000, 'api.example.com', 'summarize', 'm', '{}', 200, 2)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 4).unwrap();
        let store = Store::init(conn, Layout::Plaintext).unwrap();
        // A row from before, which doesn't say what it covered, goes with
        // any forget of a time up to when it was sent.
        let from_before = |store: &Store| -> i64 {
            store
                .conn
                .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(from_before(&store), 1);
        store
            .forget(&Forget::Between(
                DateTime::from_timestamp_millis(500).unwrap(),
                DateTime::from_timestamp_millis(600).unwrap(),
            ))
            .unwrap();
        assert_eq!(from_before(&store), 0);
        // Every kind of forget works, the ones that clean the outbox too.
        let now = Utc::now();
        for what in [
            Forget::Since(now - TimeDelta::minutes(15)),
            Forget::Before(now - TimeDelta::days(1)),
            Forget::Between(now - TimeDelta::hours(2), now - TimeDelta::hours(1)),
            Forget::App("com.example.mail".to_string()),
            Forget::Everything,
        ] {
            store.forget(&what).unwrap();
        }
        // And repairing again changes nothing.
        repair(&store.conn).unwrap();
    }

    /// A migration that has run anywhere must never change: a database that
    /// already ran it would never get the change (that is how the outbox
    /// above lost its columns). Change the schema with a new migration; add
    /// a new file's hash here when it is merged.
    #[test]
    fn migration_files_are_never_changed() {
        fn fnv(text: &str) -> u64 {
            text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            })
        }
        // Line endings don't count, so a checkout that converts them passes.
        let hashes: Vec<u64> = MIGRATIONS
            .iter()
            .map(|sql| fnv(&sql.replace("\r\n", "\n")))
            .collect();
        assert_eq!(
            hashes, KNOWN_MIGRATIONS,
            "a migration file changed; add a new one instead"
        );
    }

    const KNOWN_MIGRATIONS: &[u64] = &[
        1269219197457439031,  // 0001 captures
        5865538975833966697,  // 0002 settings
        6648104758707644946,  // 0003 exclusions
        9557723628897049019,  // 0004 outbox
        1421081335903528270,  // 0005 journal
        16283696603318477248, // 0006 memory
        520382079594332428,   // 0007 memory choices
        11210449842226117433, // 0008 pages
        15278673770390570042, // 0009 page facts
        2814010323252384379,  // 0010 about me
        1465373273103624669,  // 0011 episode vectors
        5724453071597441843,  // 0012 page text
        10269123721939706164, // 0013 page edits
        6651659427512421612,  // 0014 moved page files
        14272306742173636111, // 0015 not same
        12863801613182836056, // 0016 pair verdicts
    ];

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

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_781_520_000_000 + minute * 60_000).unwrap()
    }

    fn insert_at(store: &Store, minute: i64, title: &str, text: &str) -> i64 {
        store
            .insert(&NewCapture {
                captured_at: at(minute),
                ..capture(title, text)
            })
            .unwrap()
            .unwrap()
    }

    #[test]
    fn captures_read_before_a_forget_are_refused_after_it() {
        let store = Store::open_in_memory().unwrap();
        let read_at = Utc::now() - TimeDelta::seconds(20);
        let held = NewCapture {
            captured_at: read_at,
            ..capture("Chat", "read, then held back")
        };
        // The user forgets the last 15 minutes while capture holds `held`.
        store
            .forget(&Forget::Since(Utc::now() - TimeDelta::minutes(15)))
            .unwrap();
        assert_eq!(store.insert(&held).unwrap(), None);
        assert!(store.search("held", 10).unwrap().is_empty());

        // Something read after the forget is stored.
        let later = NewCapture {
            captured_at: Utc::now() + TimeDelta::seconds(1),
            ..capture("Chat", "new activity")
        };
        assert!(store.insert(&later).unwrap().is_some());

        // So is an older capture the forget didn't cover.
        let other = NewCapture {
            captured_at: read_at - TimeDelta::hours(2),
            ..capture("Chat", "outside the range")
        };
        assert!(store.insert(&other).unwrap().is_some());
    }

    #[test]
    fn forgetting_an_app_refuses_its_held_captures() {
        let store = Store::open_in_memory().unwrap();
        let held = NewCapture {
            captured_at: Utc::now() - TimeDelta::seconds(5),
            ..capture("Inbox", "held mail")
        };
        store
            .forget(&Forget::App("com.apple.mail".to_string()))
            .unwrap();
        assert_eq!(store.insert(&held).unwrap(), None);
        let notes = NewCapture {
            bundle_id: "com.apple.Notes",
            ..held.clone()
        };
        assert!(store.insert(&notes).unwrap().is_some());
    }

    #[test]
    fn apps_are_listed_with_their_latest_name_and_count() {
        let store = Store::open_in_memory().unwrap();
        insert_at(&store, 0, "a", "x");
        insert_at(&store, 1, "b", "x");
        store
            .insert(&NewCapture {
                captured_at: at(2),
                app_name: "Anteckningar",
                bundle_id: "com.apple.Notes",
                ..capture("c", "x")
            })
            .unwrap();
        // The same app under a newer name: the newest name is shown.
        store
            .insert(&NewCapture {
                captured_at: at(3),
                app_name: "Mail (new)",
                ..capture("d", "x")
            })
            .unwrap();
        let apps = store.apps().unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].app_name, "Mail (new)");
        assert_eq!(apps[0].bundle_id, "com.apple.mail");
        assert_eq!(apps[0].captures, 3);
        assert_eq!(apps[1].app_name, "Anteckningar");
    }

    #[test]
    fn exclusions_are_kept_and_matched_without_case() {
        let tmp = TempDir::new();
        let store = Store::open(&tmp.0).unwrap();
        assert_eq!(store.exclusions().unwrap().apps.len(), 0);
        store
            .add_exclusion(ExclusionKind::App, "com.tinyspeck.slackmacgap")
            .unwrap();
        store
            .add_exclusion(ExclusionKind::App, "COM.TINYSPECK.SLACKMACGAP")
            .unwrap();
        store
            .add_exclusion(ExclusionKind::Site, "example.com")
            .unwrap();
        store
            .add_exclusion(ExclusionKind::Title, "private chat")
            .unwrap();
        store
            .add_exclusion(ExclusionKind::DefaultOff, "Klarna")
            .unwrap();
        drop(store);

        let store = Store::open(&tmp.0).unwrap();
        let exclusions = store.exclusions().unwrap();
        assert_eq!(exclusions.apps, ["com.tinyspeck.slackmacgap"]);
        assert_eq!(exclusions.sites, ["example.com"]);
        assert_eq!(exclusions.title_patterns, ["private chat"]);
        assert!(exclusions.re_enabled_defaults.contains("Klarna"));

        store
            .remove_exclusion(ExclusionKind::App, "Com.Tinyspeck.SlackMacGap")
            .unwrap();
        assert!(store.exclusions().unwrap().apps.is_empty());
    }

    #[test]
    fn recent_lists_newest_first_and_continues() {
        let store = Store::open_in_memory().unwrap();
        for minute in 0..5 {
            insert_at(&store, minute, &format!("t{minute}"), "x");
        }
        // Two captures in the same millisecond keep a stable order by id.
        insert_at(&store, 4, "t4b", "x");

        let first = store.recent(None, 3).unwrap();
        let titles: Vec<_> = first.iter().map(|c| c.window_title.as_str()).collect();
        assert_eq!(titles, ["t4b", "t4", "t3"]);

        let last = first.last().unwrap();
        let next = store.recent(Some((last.captured_at, last.id)), 10).unwrap();
        let titles: Vec<_> = next.iter().map(|c| c.window_title.as_str()).collect();
        assert_eq!(titles, ["t2", "t1", "t0"]);
    }

    #[test]
    fn recent_shows_only_the_start_of_long_text() {
        let store = Store::open_in_memory().unwrap();
        let long = "å".repeat(PREVIEW_CHARS + 100);
        insert_at(&store, 0, "Long", &long);
        let summary = &store.recent(None, 1).unwrap()[0];
        assert_eq!(summary.preview.chars().count(), PREVIEW_CHARS);
        assert_eq!(summary.text_chars, PREVIEW_CHARS + 100);
    }

    #[test]
    fn get_returns_all_the_text() {
        let store = Store::open_in_memory().unwrap();
        let long = "word ".repeat(1000);
        let id = insert_at(&store, 0, "Long", &long);
        assert_eq!(store.get(id).unwrap().unwrap().text, long);
        assert_eq!(store.get(id + 1).unwrap(), None);
    }

    #[test]
    fn search_hits_mark_the_matched_words() {
        let store = Store::open_in_memory().unwrap();
        insert_at(
            &store,
            0,
            "Q3 proposal",
            "Anna Example wants the offert för Q3 by Friday",
        );
        let hits = store.search_hits("offert", 10).unwrap();
        assert_eq!(hits.len(), 1);
        let snippet = &hits[0].snippet;
        assert!(
            snippet.contains(&format!("{MATCH_START}offert{MATCH_END}")),
            "{snippet:?}"
        );
        assert_eq!(hits[0].capture.window_title, "Q3 proposal");
        assert!(store.search_hits("   ", 10).unwrap().is_empty());
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
        let id = store
            .insert(&capture("Inbox", "forgettable"))
            .unwrap()
            .unwrap();
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
        let id = store.insert(&new).unwrap().unwrap();
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
        // Both layouts: rebuilt by VACUUM, and given back page by page as
        // an encrypted database is.
        for open in [Store::open, Store::open_incremental] {
            forgotten_text_gone(open);
        }
    }

    fn forgotten_text_gone(open: fn(&Path) -> Result<Store>) {
        let tmp = TempDir::new();
        let store = open(&tmp.0).unwrap();
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
        // Both layouts: rebuilt by VACUUM, and given back page by page as
        // an encrypted database is.
        for open in [Store::open, Store::open_incremental] {
            expired_text_gone(open);
        }
    }

    fn expired_text_gone(open: fn(&Path) -> Result<Store>) {
        let tmp = TempDir::new();
        let store = open(&tmp.0).unwrap();
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
    fn model_choices_are_kept_per_task() {
        let tmp = TempDir::new();
        let store = Store::open(&tmp.0).unwrap();
        assert_eq!(store.model_choice(ModelTask::Chat).unwrap(), None);
        let chat = ModelChoice {
            base_url: "http://localhost:1234/v1/".to_string(),
            model: "google/gemma-4-e4b".to_string(),
        };
        store
            .set_model_choice(ModelTask::Chat, Some(&chat))
            .unwrap();
        drop(store);
        let store = Store::open(&tmp.0).unwrap();
        assert_eq!(store.model_choice(ModelTask::Chat).unwrap(), Some(chat));
        assert_eq!(store.model_choice(ModelTask::Embeddings).unwrap(), None);
        store.set_model_choice(ModelTask::Chat, None).unwrap();
        assert_eq!(store.model_choice(ModelTask::Chat).unwrap(), None);
    }

    fn sent(minute: i64, payload: &str) -> OutboxEntry {
        OutboxEntry {
            sent_at: at(minute),
            provider: "api.example.com".to_string(),
            task: "summarize".to_string(),
            model: "example-model".to_string(),
            payload: payload.to_string(),
            status: 200,
            response_bytes: 512,
            covers: Some((at(minute - 15), at(minute))),
            apps: vec!["com.apple.mail".to_string()],
        }
    }

    #[test]
    fn forgetting_captures_forgets_what_was_sent_about_them() {
        let store = Store::open_in_memory().unwrap();
        // Requests covering minutes -15..0, 45..60 and 105..120.
        store.record_outbox(&sent(0, "first")).unwrap();
        store.record_outbox(&sent(60, "second")).unwrap();
        store
            .record_outbox(&OutboxEntry {
                apps: vec!["com.apple.Notes".to_string()],
                ..sent(120, "third")
            })
            .unwrap();
        // A request not made from captures stays through time forgets.
        store
            .record_outbox(&OutboxEntry {
                covers: None,
                apps: Vec::new(),
                ..sent(130, "not from captures")
            })
            .unwrap();
        let left = |store: &Store| -> Vec<String> {
            store
                .outbox(None, None, 10)
                .unwrap()
                .into_iter()
                .map(|o| o.preview)
                .collect()
        };

        store.forget(&Forget::Between(at(50), at(55))).unwrap();
        assert_eq!(left(&store), ["not from captures", "third", "first"]);
        store
            .forget(&Forget::App("COM.APPLE.NOTES".to_string()))
            .unwrap();
        assert_eq!(left(&store), ["not from captures", "first"]);
        store.forget(&Forget::Since(at(-5))).unwrap();
        assert_eq!(left(&store), ["not from captures"]);
    }

    #[test]
    fn outbox_lists_searches_and_deletes() {
        let store = Store::open_in_memory().unwrap();
        let first = store
            .record_outbox(&sent(0, r#"{"messages":["Offert för Q3"]}"#))
            .unwrap();
        store
            .record_outbox(&sent(1, r#"{"messages":["Weekly report"]}"#))
            .unwrap();
        let all = store.outbox(None, None, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].task, "summarize");
        assert!(all[1].preview.contains("Offert"));
        let found = store.outbox(Some("offert"), None, 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, first);
        assert_eq!(
            store.outbox_payload(first).unwrap().as_deref(),
            Some(r#"{"messages":["Offert för Q3"]}"#)
        );
        assert_eq!(store.delete_outbox(Some(first)).unwrap(), 1);
        assert!(store.outbox(Some("offert"), None, 10).unwrap().is_empty());
        assert_eq!(store.delete_outbox(None).unwrap(), 1);
        assert!(store.outbox(None, None, 10).unwrap().is_empty());
    }

    #[test]
    fn deleting_everything_includes_the_outbox() {
        let store = Store::open_in_memory().unwrap();
        store.record_outbox(&sent(0, "sent text")).unwrap();
        store.forget(&Forget::Everything).unwrap();
        assert!(store.outbox(None, None, 10).unwrap().is_empty());
    }

    #[test]
    fn the_outbox_expires_on_its_own_retention() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(
            store.outbox_retention_days().unwrap(),
            DEFAULT_OUTBOX_RETENTION_DAYS
        );
        store.record_outbox(&sent(0, "old")).unwrap();
        store
            .set_outbox_retention_days(1, at(0) + TimeDelta::days(2))
            .unwrap();
        assert!(store.outbox(None, None, 10).unwrap().is_empty());
        assert!(store.set_outbox_retention_days(0, Utc::now()).is_err());
    }

    fn episode(summary: &str) -> NewEpisode {
        NewEpisode {
            summary: summary.to_string(),
            model: "google/gemma-4-e4b".to_string(),
            created_at: at(100),
        }
    }

    #[test]
    fn episodes_summarize_captures_once() {
        let mut store = Store::open_in_memory().unwrap();
        let a = insert_at(&store, 0, "Inbox", "Offert för Q3");
        let b = insert_at(&store, 5, "Inbox", "Svar från Anna");
        insert_at(&store, 30, "Inbox", "Later");
        assert_eq!(store.unsummarized_count().unwrap(), 3);
        let waiting = store.unsummarized(at(20), 10).unwrap();
        assert_eq!(waiting.iter().map(|c| c.id).collect::<Vec<_>>(), [a, b]);

        let id = store
            .record_episode(&episode("Läste mejl om offerten för Q3."), &[a, b])
            .unwrap()
            .unwrap();
        assert_eq!(store.unsummarized_count().unwrap(), 1);
        let episodes = store.episodes(None, 10).unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].id, id);
        assert_eq!(episodes[0].started_at, at(0));
        assert_eq!(episodes[0].ended_at, at(5));
        assert_eq!(episodes[0].app_names, ["Mail"]);
        assert_eq!(episodes[0].captures, 2);
        // Already summarized captures can't be summarized again.
        assert_eq!(store.record_episode(&episode("again"), &[a]).unwrap(), None);
    }

    #[test]
    fn an_episode_about_forgotten_captures_is_refused() {
        let mut store = Store::open_in_memory().unwrap();
        let a = insert_at(&store, 0, "Inbox", "text");
        let b = insert_at(&store, 1, "Inbox", "text");
        // The model is writing while the user forgets one of them.
        store.forget(&Forget::Between(at(1), at(2))).unwrap();
        assert_eq!(store.record_episode(&episode("x"), &[a, b]).unwrap(), None);
        assert!(store.episodes(None, 10).unwrap().is_empty());
    }

    #[test]
    fn forgetting_captures_forgets_their_episodes() {
        let mut store = Store::open_in_memory().unwrap();
        let a = insert_at(&store, 0, "Inbox", "mail");
        let notes = store
            .insert(&NewCapture {
                captured_at: at(1),
                app_name: "Anteckningar",
                bundle_id: "com.apple.Notes",
                ..capture("Note", "note")
            })
            .unwrap()
            .unwrap();
        store
            .record_episode(&episode("Mail och anteckning."), &[a, notes])
            .unwrap();
        let later = insert_at(&store, 60, "Inbox", "later mail");
        store
            .record_episode(&episode("Senare mejl."), &[later])
            .unwrap();

        store
            .forget(&Forget::App("com.apple.notes".to_string()))
            .unwrap();
        let left: Vec<String> = store
            .episodes(None, 10)
            .unwrap()
            .into_iter()
            .map(|e| e.summary)
            .collect();
        assert_eq!(left, ["Senare mejl."]);
        // The mail from the forgotten episode waits to be summarized again.
        assert_eq!(store.unsummarized(at(30), 10).unwrap()[0].id, a);

        store.forget(&Forget::Everything).unwrap();
        assert!(store.episodes(None, 10).unwrap().is_empty());
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
    fn summarized_captures_go_after_30_days_and_the_rest_at_the_cap() {
        let mut store = Store::open_in_memory().unwrap();
        let day = 24 * 60;
        // Captured 40 days and 10 days before "now", summarized or not.
        let old_summarized = insert_at(&store, 0, "Inbox", "old, summarized");
        insert_at(&store, 1, "Inbox", "old, waiting");
        let new_summarized = insert_at(&store, 30 * day, "Inbox", "new, summarized");
        let episode_old = store
            .record_episode(&episode("Läste offerten."), &[old_summarized])
            .unwrap()
            .unwrap();
        store
            .record_episode(&episode("Svarade Anna."), &[new_summarized])
            .unwrap()
            .unwrap();
        let now = at(40 * day);
        assert_eq!(store.expire(now).unwrap(), 1);
        assert_eq!(remaining(&store), ["old, waiting", "new, summarized"]);
        // The episode made from it stays.
        assert!(
            store
                .episodes(None, 10)
                .unwrap()
                .iter()
                .any(|e| e.id == episode_old)
        );
        // At the 90-day cap the unsummarized one goes too.
        assert_eq!(store.expire(at(92 * day)).unwrap(), 2);
        assert_eq!(store.episodes(None, 10).unwrap().len(), 2);
        // A retention under 30 days applies to summarized captures as well.
        let short = Store::open_in_memory().unwrap();
        short.set_retention_days(7, at(0)).unwrap();
        let a = insert_at(&short, 0, "Inbox", "a");
        let mut short = short;
        short.record_episode(&episode("A."), &[a]).unwrap().unwrap();
        assert_eq!(short.expire(at(8 * day)).unwrap(), 1);
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
