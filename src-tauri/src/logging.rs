//! Logging that can't carry captured content (GUARDRAILS §6).
//!
//! A log entry is an event name and a list of fields. The event name must be a
//! `&'static str`, a fixed name written in the code, and fields can only hold
//! numbers, durations, ids and booleans. There is no way to put a `String`
//! into a log entry, so window text, titles, OCR output, transcripts, episodes
//! and memory pages can't end up in a log by mistake. Errors are logged by a
//! fixed kind (`ErrorKind`), never by their message, which can contain data.
//!
//! Entries go to `logs/lax.log` in the data folder (`0600`, folder `0700`),
//! one line each, and the file is rotated at [`MAX_FILE_BYTES`] with
//! [`KEPT_FILES`] old files kept. Debug builds also print to stderr.
//!
//! `println!`, `eprintln!`, `print!`, `eprint!` and `dbg!` are denied by clippy
//! everywhere else (see `Cargo.toml`), so nothing can bypass this module.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};

pub const LOG_DIR: &str = "logs";
pub const LOG_FILE: &str = "lax.log";
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
pub const KEPT_FILES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// A field in a log entry. Only numbers, durations, ids, booleans and fixed
/// error kinds: never text from outside the code.
#[derive(Debug, Clone, Copy)]
pub enum Field {
    Count(&'static str, u64),
    Duration(&'static str, Duration),
    Id(&'static str, i64),
    Flag(&'static str, bool),
    Error(ErrorKind),
}

/// What went wrong, without the error message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Io,
    Database,
    Permission,
    Timeout,
    Other,
}

impl ErrorKind {
    fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Io => "io",
            ErrorKind::Database => "database",
            ErrorKind::Permission => "permission",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Other => "other",
        }
    }
}

static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();

/// Starts writing to `<data_dir>/logs/lax.log`. Before this, and if it
/// fails, entries are dropped (and printed to stderr in debug builds).
pub fn init(data_dir: &Path) -> std::io::Result<()> {
    let logger = Logger::new(&data_dir.join(LOG_DIR), Level::Info)?;
    let _ = LOGGER.set(Mutex::new(logger));
    Ok(())
}

pub fn debug(event: &'static str, fields: &[Field]) {
    log(Level::Debug, event, fields);
}

pub fn info(event: &'static str, fields: &[Field]) {
    log(Level::Info, event, fields);
}

pub fn warn(event: &'static str, fields: &[Field]) {
    log(Level::Warn, event, fields);
}

pub fn error(event: &'static str, fields: &[Field]) {
    log(Level::Error, event, fields);
}

fn log(level: Level, event: &'static str, fields: &[Field]) {
    let logger = LOGGER.get();
    let to_file =
        logger.is_some_and(|l| level >= l.lock().unwrap_or_else(PoisonError::into_inner).min_level);
    // Release builds only write to the file, so skip the work for entries
    // below its level.
    if !to_file && !cfg!(debug_assertions) {
        return;
    }
    let line = format_line(&timestamp(), level, event, fields);
    #[cfg(debug_assertions)]
    #[allow(clippy::print_stderr)] // The one sanctioned place; see the module docs.
    {
        eprintln!("{line}");
    }
    if let (true, Some(logger)) = (to_file, logger) {
        let mut logger = logger.lock().unwrap_or_else(PoisonError::into_inner);
        // Logging must never take Lax down; a failed write is dropped.
        let _ = logger.write(level, &line);
    }
}

/// Logs that Lax crashed, and where in its own code: the line, and the
/// source file as a number (see [`file_code`]). Never the panic's message,
/// which can hold captured text. Release builds abort on a panic
/// (`panic = "abort"`), so without this a crash leaves no trace.
pub fn log_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let (file, line) = info
            .location()
            .map_or((0, 0), |at| (file_code(at.file()), i64::from(at.line())));
        let entry = format_line(
            &timestamp(),
            Level::Error,
            "app.panicked",
            &[Field::Id("file", file), Field::Id("line", line)],
        );
        // The panic may have happened while the log was being written, so
        // never wait for it.
        if let Some(logger) = LOGGER.get()
            && let Ok(mut logger) = logger.try_lock()
        {
            let _ = logger.write(Level::Error, &entry);
        }
        previous(info);
    }));
}

/// A source file's path as a number for the log, which takes no text: the
/// FNV-1a hash of the path, in 31 bits. A developer finds the file with
/// `file_code` over the source paths; the path itself is never logged.
pub fn file_code(path: &str) -> i64 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in path.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    i64::from(hash >> 1)
}

/// Deletes all log files, for "Delete all data". Logs hold no captured
/// content, but their timestamps show when Lax was in use.
pub fn delete_all(data_dir: &Path) -> std::io::Result<()> {
    let dir = data_dir.join(LOG_DIR);
    if let Some(logger) = LOGGER.get() {
        let mut logger = logger.lock().unwrap_or_else(PoisonError::into_inner);
        logger.file = open_log_truncated(&dir.join(LOG_FILE))?;
        logger.size = 0;
    }
    for n in 1..=KEPT_FILES {
        match fs::remove_file(dir.join(format!("{LOG_FILE}.{n}"))) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// `2026-06-15T12:00:00.000Z INFO capture.tick nodes=42 elapsed_ms=12`
fn format_line(timestamp: &str, level: Level, event: &'static str, fields: &[Field]) -> String {
    let mut line = format!("{timestamp} {} {}", level.as_str(), clean_name(event));
    for field in fields {
        let (name, value) = match *field {
            Field::Count(name, n) => (name, n.to_string()),
            Field::Duration(name, d) => (name, format!("{}ms", d.as_millis())),
            Field::Id(name, id) => (name, id.to_string()),
            Field::Flag(name, flag) => (name, flag.to_string()),
            Field::Error(kind) => ("error", kind.as_str().to_string()),
        };
        line.push(' ');
        line.push_str(&clean_name(name));
        line.push('=');
        line.push_str(&value);
    }
    line
}

/// Names are fixed in the code; anything outside `a-z 0-9 . _` is replaced,
/// so a name can never smuggle in other text or break the line format.
fn clean_name(name: &str) -> String {
    let clean = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_');
    if clean && !name.is_empty() {
        name.to_string()
    } else {
        "invalid_name".to_string()
    }
}

struct Logger {
    dir: PathBuf,
    min_level: Level,
    file: File,
    size: u64,
}

impl Logger {
    fn new(dir: &Path, min_level: Level) -> std::io::Result<Self> {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        fs::set_permissions(dir, Permissions::from_mode(0o700))?;
        let file = open_log(&dir.join(LOG_FILE))?;
        let size = file.metadata()?.len();
        Ok(Logger {
            dir: dir.to_path_buf(),
            min_level,
            file,
            size,
        })
    }

    fn write(&mut self, level: Level, line: &str) -> std::io::Result<()> {
        if level < self.min_level {
            return Ok(());
        }
        let bytes = line.len() as u64 + 1;
        if self.size + bytes > MAX_FILE_BYTES {
            self.rotate()?;
        }
        writeln!(self.file, "{line}")?;
        self.size += bytes;
        Ok(())
    }

    /// lax.log → lax.log.1 → lax.log.2 …, dropping the oldest. If that fails,
    /// the current file is emptied instead, so logging carries on.
    fn rotate(&mut self) -> std::io::Result<()> {
        if self.shift_files().is_err() {
            self.file = open_log_truncated(&self.dir.join(LOG_FILE))?;
            self.size = 0;
        }
        Ok(())
    }

    fn shift_files(&mut self) -> std::io::Result<()> {
        let path = |n: usize| {
            if n == 0 {
                self.dir.join(LOG_FILE)
            } else {
                self.dir.join(format!("{LOG_FILE}.{n}"))
            }
        };
        let _ = fs::remove_file(path(KEPT_FILES));
        for n in (0..KEPT_FILES).rev() {
            if path(n).exists() {
                fs::rename(path(n), path(n + 1))?;
            }
        }
        self.file = open_log(&path(0))?;
        self.size = 0;
        Ok(())
    }
}

fn open_log_truncated(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o600))?;
    Ok(file)
}

fn open_log(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let name = format!(
                "lax-log-test-{}-{}",
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

    const TS: &str = "2026-06-15T12:00:00.000Z";

    #[test]
    fn lines_hold_only_names_and_numbers() {
        let line = format_line(
            TS,
            Level::Info,
            "capture.tick",
            &[
                Field::Count("nodes", 42),
                Field::Duration("elapsed", Duration::from_millis(12)),
                Field::Id("capture_id", 7),
                Field::Flag("deduplicated", true),
                Field::Error(ErrorKind::Database),
            ],
        );
        assert_eq!(
            line,
            "2026-06-15T12:00:00.000Z INFO capture.tick nodes=42 elapsed=12ms capture_id=7 deduplicated=true error=database"
        );
    }

    #[test]
    fn names_outside_the_allowed_characters_are_replaced() {
        let line = format_line(TS, Level::Warn, "Anna Example\nINFO fake", &[]);
        assert_eq!(line, "2026-06-15T12:00:00.000Z WARN invalid_name");
        let line = format_line(TS, Level::Warn, "ok", &[Field::Count("bad name", 1)]);
        assert_eq!(line, "2026-06-15T12:00:00.000Z WARN ok invalid_name=1");
    }

    #[test]
    fn log_folder_and_file_are_private() {
        let tmp = TempDir::new();
        let dir = tmp.0.join(LOG_DIR);
        let mut logger = Logger::new(&dir, Level::Info).unwrap();
        logger.write(Level::Info, "x").unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(LOG_FILE)), 0o600);
    }

    #[test]
    fn levels_below_the_minimum_are_dropped() {
        let tmp = TempDir::new();
        let mut logger = Logger::new(&tmp.0, Level::Info).unwrap();
        logger.write(Level::Debug, "hidden").unwrap();
        logger.write(Level::Error, "shown").unwrap();
        let content = fs::read_to_string(tmp.0.join(LOG_FILE)).unwrap();
        assert_eq!(content, "shown\n");
    }

    #[test]
    fn files_rotate_and_old_ones_are_dropped() {
        let tmp = TempDir::new();
        let mut logger = Logger::new(&tmp.0, Level::Info).unwrap();
        let line = "x".repeat(1023); // 1 KiB with the newline
        let lines_per_file = (MAX_FILE_BYTES / 1024) as usize;
        for _ in 0..lines_per_file * (KEPT_FILES + 2) {
            logger.write(Level::Info, &line).unwrap();
        }
        assert!(tmp.0.join(LOG_FILE).exists());
        for n in 1..=KEPT_FILES {
            let rotated = tmp.0.join(format!("{LOG_FILE}.{n}"));
            assert!(rotated.exists(), "{}", rotated.display());
            assert!(fs::metadata(&rotated).unwrap().len() <= MAX_FILE_BYTES);
        }
        assert!(
            !tmp.0
                .join(format!("{LOG_FILE}.{}", KEPT_FILES + 1))
                .exists()
        );
    }

    #[test]
    fn delete_all_removes_rotated_files() {
        let tmp = TempDir::new();
        let dir = tmp.0.join(LOG_DIR);
        let mut logger = Logger::new(&dir, Level::Info).unwrap();
        logger.write(Level::Info, "x").unwrap();
        logger.rotate().unwrap();
        assert!(dir.join(format!("{LOG_FILE}.1")).exists());
        delete_all(&tmp.0).unwrap();
        assert!(!dir.join(format!("{LOG_FILE}.1")).exists());
    }

    #[test]
    fn failed_rotation_keeps_logging() {
        let tmp = TempDir::new();
        let mut logger = Logger::new(&tmp.0, Level::Info).unwrap();
        // lax.log.2 can't be moved onto a non-empty directory at lax.log.3,
        // so shifting the files fails.
        fs::write(tmp.0.join(format!("{LOG_FILE}.2")), "old").unwrap();
        fs::create_dir_all(tmp.0.join(format!("{LOG_FILE}.3")).join("blocker")).unwrap();
        logger.write(Level::Info, "before").unwrap();
        logger.rotate().unwrap();
        logger.write(Level::Info, "after").unwrap();
        assert!(
            !tmp.0.join(format!("{LOG_FILE}.1")).exists(),
            "files were not shifted"
        );
        let content = fs::read_to_string(tmp.0.join(LOG_FILE)).unwrap();
        assert_eq!(content, "after\n");
    }

    #[test]
    fn nothing_prints_outside_this_module() {
        // Clippy denies these too; this also covers code clippy doesn't see,
        // such as tests and code behind other cfgs.
        let forbidden = [
            "println!",
            "eprintln!",
            "print!(",
            "eprint!(",
            "dbg!(",
            "io::stdout",
            "io::stderr",
            ".leak(",
            "Box::leak",
        ];
        let mut dirs = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = dirs.pop() {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && path.file_name().is_some_and(|n| n != "logging.rs")
                {
                    let source = fs::read_to_string(&path).unwrap();
                    for macro_name in forbidden {
                        assert!(
                            !source.contains(macro_name),
                            "{} uses {macro_name}",
                            path.display()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn crashes_are_logged_as_numbers() {
        // Stable, and different for different files.
        assert_eq!(file_code("src/ask.rs"), file_code("src/ask.rs"));
        assert_ne!(file_code("src/ask.rs"), file_code("src/write.rs"));
        assert!(file_code("src/ask.rs") >= 0);
        // The entry holds only the event and numbers.
        let line = format_line(
            "2026-10-13T08:00:00.000Z",
            Level::Error,
            "app.panicked",
            &[Field::Id("file", 12345), Field::Id("line", 42)],
        );
        assert!(
            line.ends_with("ERROR app.panicked file=12345 line=42"),
            "{line}"
        );
    }
}
