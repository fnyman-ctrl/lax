//! The encrypted database (ADR 0010).
//!
//! A new database is encrypted with SQLCipher under the key in the login
//! keychain ([`crate::keychain`]), and a plaintext one is converted once. An
//! encrypted one opens only with its own key: a missing key is an error,
//! never a reason to make a new one.
//!
//! The key never appears in SQL: it is set with `sqlite3_key_v2`, and every
//! failure on the way is one of the fixed errors in [`StoreError`], with no
//! SQLite text, which can quote the statement it failed on.

// SQLite's C API to set the key, and statvfs for free space; see SAFETY.
#![allow(unsafe_code)]

use std::ffi::c_int;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, ffi};
use zeroize::Zeroizing;

use super::{Layout, Result, Store, StoreError, database_file};
use crate::keychain::{self, DatabaseFile, DatabaseKey, KeyError, KeySource};

/// What every plaintext SQLite database starts with. An encrypted one
/// starts with random bytes.
const PLAINTEXT_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// What `lax.sqlite` is, from its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Absent or empty: a new install, or a start that stopped before the
    /// database was made.
    Empty,
    /// A plaintext SQLite database.
    Plaintext,
    /// Anything else: encrypted, or damaged, which looks the same.
    Encrypted,
}

/// Reads the first bytes of the database file at `path`.
pub fn file_kind(path: &Path) -> std::io::Result<FileKind> {
    let mut file = match File::open(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(FileKind::Empty),
        file => file?,
    };
    let mut header = [0u8; PLAINTEXT_HEADER.len()];
    let mut read = 0;
    while read < header.len() {
        match file.read(&mut header[read..])? {
            0 => break,
            n => read += n,
        }
    }
    Ok(match read {
        0 => FileKind::Empty,
        _ if header == *PLAINTEXT_HEADER => FileKind::Plaintext,
        _ => FileKind::Encrypted,
    })
}

/// How the database was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// Made now, encrypted.
    New,
    /// Encrypted already.
    Encrypted,
    /// Plaintext until now: converted, and opened encrypted.
    Converted,
    /// Plaintext, and still plaintext: the conversion didn't happen this
    /// time, and is tried again at the next start.
    Postponed(Postponed),
}

/// Why a plaintext database wasn't converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Postponed {
    /// Less free space than twice the database.
    LowSpace,
    /// The copy couldn't be made or didn't match the original. The
    /// original is untouched.
    Failed,
}

/// The encrypted copy made by a conversion, until it replaces the database.
const ENCRYPTING_SUFFIX: &str = ".encrypting";

impl Store {
    /// Opens (or creates) the database in `data_dir`, encrypted under the
    /// key from `keys`:
    ///
    /// - **Empty:** the stored key, or a new one if there is none, stored
    ///   and read back first; then a new encrypted database.
    /// - **Plaintext:** the same key rule, then converted (ADR 0010,
    ///   "Converting an existing database"). If that can't be done now, it
    ///   opens as before and is tried again next time.
    /// - **Encrypted:** the stored key only. Without it, [`StoreError::
    ///   KeyMissing`]; nothing new is made.
    ///
    /// Reading an existing key may make macOS ask for the login password
    /// (after an update); the caller says so first (ADR 0010, "After an
    /// update").
    pub fn open_keyed(data_dir: &Path, keys: &impl KeySource) -> Result<(Self, Opened)> {
        let path = database_file(data_dir)?;
        let kind = file_kind(&path)?;
        let key = key(
            keys,
            match kind {
                FileKind::Encrypted => DatabaseFile::Encrypted,
                FileKind::Empty | FileKind::Plaintext => DatabaseFile::NotEncrypted,
            },
        )?;
        let opened = match kind {
            FileKind::Empty => Opened::New,
            FileKind::Encrypted => Opened::Encrypted,
            FileKind::Plaintext => match convert(&path, &key, free_space(data_dir)) {
                Ok(()) => Opened::Converted,
                Err(postponed) => return Ok((Self::open(data_dir)?, Opened::Postponed(postponed))),
            },
        };
        let conn = Connection::open(&path)?;
        unlock(&conn, &key)?;
        if opened == Opened::New {
            // Before WAL: set after it, it silently stays off.
            conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Ok((Self::init(conn, Layout::Encrypted)?, opened))
    }
}

/// Converts the plaintext database at `path` into an encrypted one under
/// `key`, with `available` bytes free on its disk (ADR 0010, "Converting
/// an existing database"):
///
/// 1. At least twice the database's size must be free.
/// 2. The write-ahead log is folded into the database and removed.
/// 3. An encrypted copy is exported next to it, laid out for incremental
///    vacuum, with the schema version copied by hand.
/// 4. The copy is checked with the key: integrity, the search indexes, and
///    the same rows in every table.
/// 5. The copy replaces the database by a rename, after both are synced.
///
/// Until the rename, the database is the untouched plaintext one, and a
/// copy left by a stop halfway is deleted and made again next time. After
/// it, the database is the checked encrypted copy. There is no moment with
/// neither. The plaintext's old blocks become free space; FileVault, where
/// on, covers them.
fn convert(
    path: &Path,
    key: &DatabaseKey,
    available: Option<u64>,
) -> std::result::Result<(), Postponed> {
    let copy = path_with(path, ENCRYPTING_SUFFIX);
    remove_copy(&copy);
    if !fits(path, available) {
        return Err(Postponed::LowSpace);
    }
    let result = export(path, &copy, key).and_then(|()| check(path, &copy, key));
    if result.is_err() {
        remove_copy(&copy);
        return Err(Postponed::Failed);
    }
    swap(&copy, path).map_err(|_| {
        remove_copy(&copy);
        Postponed::Failed
    })
}

/// Exports the plaintext database into an encrypted copy.
fn export(path: &Path, copy: &Path, key: &DatabaseKey) -> rusqlite::Result<()> {
    let conn = Connection::open(path)?;
    // Everything in the database file itself, and no -wal or -shm left.
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    conn.pragma_update(None, "journal_mode", "DELETE")?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    // Private from the start, like the database.
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(copy)
        .map_err(|_| rusqlite::Error::InvalidPath(copy.to_path_buf()))?;
    let raw = raw_key(key);
    // Path and key are bound, never written into the SQL, so no error text
    // can hold the key.
    conn.execute(
        "ATTACH ?1 AS encrypted KEY ?2",
        rusqlite::params![copy.to_string_lossy(), raw.as_str()],
    )?;
    conn.pragma_update(Some("encrypted"), "cipher_compatibility", 4)?;
    conn.pragma_update(Some("encrypted"), "auto_vacuum", "INCREMENTAL")?;
    conn.query_row("SELECT sqlcipher_export('encrypted')", [], |_| Ok(()))?;
    // The export doesn't copy the schema version.
    conn.pragma_update(Some("encrypted"), "user_version", version)?;
    conn.execute("DETACH encrypted", [])?;
    Ok(())
}

/// Checks the encrypted copy against the plaintext database.
fn check(path: &Path, copy: &Path, key: &DatabaseKey) -> rusqlite::Result<()> {
    let plain = Connection::open(path)?;
    let encrypted = Connection::open(copy)?;
    unlock(&encrypted, key).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let ok: String = encrypted.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if ok != "ok" {
        return Err(rusqlite::Error::InvalidQuery);
    }
    for index in SEARCH_INDEXES {
        encrypted.execute(
            &format!("INSERT INTO {index} ({index}) VALUES ('integrity-check')"),
            [],
        )?;
    }
    let version = |conn: &Connection| -> rusqlite::Result<i64> {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
    };
    if version(&plain)? != version(&encrypted)? {
        return Err(rusqlite::Error::InvalidQuery);
    }
    // The same tables, indexes, triggers and views, with the same SQL: a
    // lost trigger would quietly break a forget.
    let schema = |conn: &Connection| -> rusqlite::Result<Vec<(String, String, Option<String>)>> {
        let mut statement =
            conn.prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect()
    };
    if schema(&plain)? != schema(&encrypted)? {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let tables = |conn: &Connection| -> rusqlite::Result<Vec<String>> {
        let mut statement = conn.prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND sql NOT LIKE 'CREATE VIRTUAL%' ORDER BY name",
        )?;
        statement.query_map([], |row| row.get(0))?.collect()
    };
    let names = tables(&plain)?;
    for name in names {
        // Names from the database's own schema, quoted.
        let sql = format!("SELECT count(*) FROM \"{}\"", name.replace('"', "\"\""));
        let count = |conn: &Connection| -> rusqlite::Result<i64> {
            conn.query_row(&sql, [], |row| row.get(0))
        };
        if count(&plain)? != count(&encrypted)? {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    Ok(())
}

/// Puts the checked copy in the database's place: the copy is synced, then
/// renamed over the database, then the folder is synced, so the rename
/// survives a power cut.
fn swap(copy: &Path, path: &Path) -> std::io::Result<()> {
    File::open(copy)?.sync_all()?;
    fs::rename(copy, path)?;
    // The plaintext database's write-ahead files, if any were left: they
    // belong to the old file.
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(path_with(path, suffix));
    }
    if let Some(dir) = path.parent() {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// The FTS5 indexes, checked after a conversion.
const SEARCH_INDEXES: [&str; 3] = ["captures_fts", "outbox_fts", "episodes_fts"];

fn path_with(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Removes a conversion's copy and its rollback journal, if any.
fn remove_copy(copy: &Path) {
    let _ = fs::remove_file(copy);
    let _ = fs::remove_file(path_with(copy, "-journal"));
}

/// Whether there is room to convert the database in `data_dir` now: free
/// space of twice its size, or free space that can't be told.
pub fn conversion_fits(data_dir: &Path) -> bool {
    fits(&data_dir.join(super::DATABASE_FILE), free_space(data_dir))
}

fn fits(path: &Path, available: Option<u64>) -> bool {
    let size = [path.to_path_buf(), path_with(path, "-wal")]
        .iter()
        .filter_map(|file| fs::metadata(file).ok())
        .map(|meta| meta.len())
        .sum::<u64>();
    available.is_none_or(|free| free >= size.saturating_mul(2))
}

/// Free bytes on the disk that holds `dir`, for this user. `None` if it
/// can't be told, and then the conversion goes ahead: the copy is checked
/// before it replaces anything.
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated; on success `statvfs` fills `stats`.
    let status = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
    if status != 0 {
        return None;
    }
    // SAFETY: filled by the successful call above.
    let stats = unsafe { stats.assume_init() };
    Some(u64::from(stats.f_bavail).saturating_mul(stats.f_frsize))
}

/// The key for a file of this kind, with the keychain's error turned into
/// the store's.
fn key(keys: &impl KeySource, file: DatabaseFile) -> Result<DatabaseKey> {
    keychain::key_for(keys, file).map_err(|error| match error {
        KeyError::Missing => StoreError::KeyMissing,
        KeyError::Locked | KeyError::Denied | KeyError::Other(_) => StoreError::KeyUnavailable,
    })
}

/// Sets `key` on `conn` and checks that it opens the file: SQLite reads
/// nothing until the first statement, so a wrong key shows only then.
/// Every failure is [`StoreError::CannotUnlock`], without SQLite's text.
fn unlock(conn: &Connection, key: &DatabaseKey) -> Result<()> {
    let raw = raw_key(key);
    let length = c_int::try_from(raw.len()).map_err(|_| StoreError::CannotUnlock)?;
    // SAFETY: the handle belongs to `conn`, which outlives the call; the
    // name is a NUL-terminated string; the key pointer and length describe
    // `raw`, which SQLCipher copies before returning.
    let status = unsafe {
        ffi::sqlite3_key_v2(conn.handle(), c"main".as_ptr(), raw.as_ptr().cast(), length)
    };
    if status != ffi::SQLITE_OK {
        return Err(StoreError::CannotUnlock);
    }
    // SQLCipher 4's defaults, kept if a later SQLCipher changes them.
    conn.pragma_update(None, "cipher_compatibility", 4)
        .and_then(|()| conn.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(())))
        .map_err(|_| StoreError::CannotUnlock)
}

/// SQLCipher's raw key form, `x'<64 hex>'`, which uses the key as it is,
/// with no key derivation. Allocated once at its full size, and zeroed when
/// dropped.
fn raw_key(key: &DatabaseKey) -> Zeroizing<String> {
    let hex = key.to_hex();
    let mut raw = Zeroizing::new(String::with_capacity(hex.len() + 3));
    raw.push_str("x'");
    raw.push_str(&hex);
    raw.push('\'');
    raw
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::keychain::fake::FakeKeychain;
    use crate::store::{DATABASE_FILE, Forget, NewCapture, schema_version};

    const KEY: &[u8] = b"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const OTHER_KEY: &[u8] = b"ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";
    /// A synthetic marker, so finding its bytes in a file means plaintext.
    const MARKER: &str = "Anna Example zebrafinkcipher";

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "lax-cipher-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            TempDir(dir)
        }

        fn database(&self) -> PathBuf {
            self.0.join(DATABASE_FILE)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn capture(text: &str) -> NewCapture<'_> {
        NewCapture {
            captured_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            app_name: "Mail",
            bundle_id: "com.apple.mail",
            window_title: "Inbox",
            site: None,
            text,
        }
    }

    /// Whether any file in `dir` holds `marker`'s bytes.
    fn on_disk(dir: &Path, marker: &str) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                let bytes = fs::read(path).unwrap();
                bytes.windows(marker.len()).any(|w| w == marker.as_bytes())
            })
            .map(|path| path.display().to_string())
            .collect()
    }

    fn pragma(store: &Store, name: &str) -> i64 {
        store
            .conn
            .pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn the_kind_of_file_comes_from_its_first_bytes() {
        let tmp = TempDir::new();
        fs::create_dir_all(&tmp.0).unwrap();
        let path = tmp.database();
        assert_eq!(file_kind(&path).unwrap(), FileKind::Empty);
        fs::write(&path, b"").unwrap();
        assert_eq!(file_kind(&path).unwrap(), FileKind::Empty);
        fs::write(&path, b"SQLite format 3\0and the rest").unwrap();
        assert_eq!(file_kind(&path).unwrap(), FileKind::Plaintext);
        fs::write(&path, [7u8; 64]).unwrap();
        assert_eq!(file_kind(&path).unwrap(), FileKind::Encrypted);
        // Too short to be SQLite: not something to start over on silently.
        fs::write(&path, b"SQLite").unwrap();
        assert_eq!(file_kind(&path).unwrap(), FileKind::Encrypted);
    }

    #[test]
    fn a_new_database_is_encrypted_with_a_new_key() {
        let tmp = TempDir::new();
        let keys = FakeKeychain::default();
        let (store, _) = Store::open_keyed(&tmp.0, &keys).unwrap();
        assert_eq!(keys.adds.get(), 1, "a key was made");
        assert!(keys.item.borrow().is_some());
        for _ in 0..20 {
            store.insert(&capture(MARKER)).unwrap();
        }
        store.set_setting("note", MARKER).unwrap();
        // A memory page's text, with a line of the user's own (ADR 0011).
        store
            .add_about_me_page(
                "page-marker",
                "about-me.md",
                Some(&format!("# About me\n\n{MARKER}\n")),
                Utc::now(),
            )
            .unwrap();
        let provider: String = store
            .conn
            .query_row("PRAGMA cipher_provider", [], |r| r.get(0))
            .unwrap();
        assert_eq!(provider, "commoncrypto");
        // Temporary data in memory, and free pages given back without VACUUM.
        assert_eq!(
            pragma(&store, "temp_store"),
            2,
            "temp_store should be MEMORY"
        );
        assert_eq!(
            pragma(&store, "auto_vacuum"),
            2,
            "auto_vacuum should be INCREMENTAL"
        );
        // While it is open, with the write-ahead log and its index on disk...
        assert_eq!(on_disk(&tmp.0, MARKER), Vec::<String>::new());
        drop(store);
        // ...and after.
        assert_eq!(on_disk(&tmp.0, MARKER), Vec::<String>::new());
        assert_eq!(file_kind(&tmp.database()).unwrap(), FileKind::Encrypted);
    }

    #[test]
    fn an_encrypted_database_opens_again_with_its_key() {
        let tmp = TempDir::new();
        let keys = FakeKeychain::default();
        let (store, _) = Store::open_keyed(&tmp.0, &keys).unwrap();
        store.insert(&capture("hello")).unwrap();
        drop(store);
        let (store, _) = Store::open_keyed(&tmp.0, &keys).unwrap();
        assert_eq!(keys.adds.get(), 1, "no second key");
        let count: i64 = store
            .conn
            .query_row("SELECT count(*) FROM captures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        // A plain connection without the key sees nothing.
        drop(store);
        let plain = Connection::open(tmp.database()).unwrap();
        assert!(
            plain
                .query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
                .is_err()
        );
    }

    #[test]
    fn a_missing_key_makes_no_new_one_for_an_encrypted_database() {
        let tmp = TempDir::new();
        drop(Store::open_keyed(&tmp.0, &FakeKeychain::holding(KEY)).unwrap());
        let empty = FakeKeychain::default();
        let error = Store::open_keyed(&tmp.0, &empty).err().unwrap();
        assert!(matches!(error, StoreError::KeyMissing), "{error:?}");
        assert_eq!(
            empty.adds.get(),
            0,
            "never a new key next to an encrypted file"
        );
        assert_eq!(file_kind(&tmp.database()).unwrap(), FileKind::Encrypted);
    }

    #[test]
    fn a_locked_or_denied_keychain_makes_nothing() {
        let tmp = TempDir::new();
        drop(Store::open_keyed(&tmp.0, &FakeKeychain::holding(KEY)).unwrap());
        for error in [
            KeyError::Locked,
            KeyError::Denied,
            KeyError::Other(Some(-1)),
        ] {
            let keys = FakeKeychain::failing_reads(&[error]);
            *keys.item.borrow_mut() = Some(KEY.to_vec());
            let found = Store::open_keyed(&tmp.0, &keys).err().unwrap();
            assert!(matches!(found, StoreError::KeyUnavailable), "{found:?}");
            assert_eq!(keys.adds.get(), 0);
        }
        // And on a new install: no database without a stored key.
        let new = TempDir::new();
        let keys = FakeKeychain::default();
        keys.add_error.set(Some(KeyError::Denied));
        let found = Store::open_keyed(&new.0, &keys).err().unwrap();
        assert!(matches!(found, StoreError::KeyUnavailable), "{found:?}");
        assert_eq!(file_kind(&new.database()).unwrap(), FileKind::Empty);
    }

    #[test]
    fn a_wrong_key_says_so_without_the_key() {
        let tmp = TempDir::new();
        drop(Store::open_keyed(&tmp.0, &FakeKeychain::holding(KEY)).unwrap());
        let error = Store::open_keyed(&tmp.0, &FakeKeychain::holding(OTHER_KEY))
            .err()
            .unwrap();
        assert!(matches!(error, StoreError::CannotUnlock), "{error:?}");
        for text in [error.to_string(), format!("{error:?}")] {
            for key in [KEY, OTHER_KEY] {
                let key = std::str::from_utf8(key).unwrap();
                assert!(!text.contains(key), "{text}");
                assert!(!text.contains(&key[..16]), "{text}");
            }
        }
        // A damaged file looks the same.
        let damaged = TempDir::new();
        fs::create_dir_all(&damaged.0).unwrap();
        fs::write(damaged.database(), [7u8; 8192]).unwrap();
        let error = Store::open_keyed(&damaged.0, &FakeKeychain::holding(KEY))
            .err()
            .unwrap();
        assert!(matches!(error, StoreError::CannotUnlock), "{error:?}");
    }

    /// A plaintext database with a capture, a setting and the marker, as
    /// an older Lax left it: closed, so its write-ahead log is folded in.
    fn plaintext_database(dir: &Path) {
        let store = Store::open(dir).unwrap();
        for _ in 0..30 {
            store.insert(&capture(MARKER)).unwrap();
        }
        store.insert(&capture("quarterly offer")).unwrap();
        store.set_setting("note", MARKER).unwrap();
    }

    fn count(store: &Store, table: &str) -> i64 {
        store
            .conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_plaintext_database_is_converted_once() {
        let tmp = TempDir::new();
        plaintext_database(&tmp.0);
        let version = schema_version();
        let keys = FakeKeychain::default();
        let (store, opened) = Store::open_keyed(&tmp.0, &keys).unwrap();
        assert_eq!(opened, Opened::Converted);
        assert_eq!(keys.adds.get(), 1, "a key was made for it");
        // Everything came along, the search index too.
        assert_eq!(count(&store, "captures"), 31);
        assert_eq!(pragma(&store, "user_version"), version);
        let hits: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM captures_fts WHERE captures_fts MATCH 'quarterly'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
        assert_eq!(
            pragma(&store, "auto_vacuum"),
            2,
            "laid out for incremental vacuum"
        );
        assert_eq!(pragma(&store, "temp_store"), 2);
        drop(store);
        // No plaintext left in any file Lax keeps, and no copy left over.
        assert_eq!(on_disk(&tmp.0, MARKER), Vec::<String>::new());
        assert_eq!(file_kind(&tmp.database()).unwrap(), FileKind::Encrypted);
        assert!(!path_with(&tmp.database(), ENCRYPTING_SUFFIX).exists());
        // The next start just opens it.
        let (_, opened) = Store::open_keyed(&tmp.0, &keys).unwrap();
        assert_eq!(opened, Opened::Encrypted);
        assert_eq!(keys.adds.get(), 1);
    }

    #[test]
    fn a_copy_left_by_a_stop_halfway_is_made_again() {
        let tmp = TempDir::new();
        plaintext_database(&tmp.0);
        // As if Lax stopped during the export: a half-written copy and its
        // journal next to the untouched database.
        let copy = path_with(&tmp.database(), ENCRYPTING_SUFFIX);
        fs::write(&copy, [9u8; 4096]).unwrap();
        fs::write(path_with(&copy, "-journal"), [9u8; 512]).unwrap();
        let (store, opened) = Store::open_keyed(&tmp.0, &FakeKeychain::holding(KEY)).unwrap();
        assert_eq!(opened, Opened::Converted);
        assert_eq!(count(&store, "captures"), 31);
        assert!(!copy.exists());
        assert!(!path_with(&copy, "-journal").exists());
    }

    #[test]
    fn too_little_space_changes_nothing() {
        let tmp = TempDir::new();
        plaintext_database(&tmp.0);
        let path = tmp.database();
        let key = keychain::read_key(&FakeKeychain::holding(KEY)).unwrap();
        assert_eq!(convert(&path, &key, Some(1024)), Err(Postponed::LowSpace));
        assert_eq!(file_kind(&path).unwrap(), FileKind::Plaintext);
        assert!(!path_with(&path, ENCRYPTING_SUFFIX).exists());
        // Unknown free space doesn't stop it: the copy is checked anyway.
        assert_eq!(convert(&path, &key, None), Ok(()));
        assert_eq!(file_kind(&path).unwrap(), FileKind::Encrypted);
    }

    #[test]
    fn a_copy_that_differs_is_never_put_in_place() {
        let tmp = TempDir::new();
        plaintext_database(&tmp.0);
        let path = tmp.database();
        let copy = path_with(&path, ENCRYPTING_SUFFIX);
        let key = keychain::read_key(&FakeKeychain::holding(KEY)).unwrap();
        export(&path, &copy, &key).unwrap();
        assert!(check(&path, &copy, &key).is_ok());
        // A row missing, a trigger missing: each is refused.
        let encrypted = Connection::open(&copy).unwrap();
        unlock(&encrypted, &key).unwrap();
        encrypted
            .execute(
                "DELETE FROM captures WHERE rowid = (SELECT max(rowid) FROM captures)",
                [],
            )
            .unwrap();
        drop(encrypted);
        assert!(check(&path, &copy, &key).is_err());
        export_again(&path, &copy, &key);
        let encrypted = Connection::open(&copy).unwrap();
        unlock(&encrypted, &key).unwrap();
        let trigger: String = encrypted
            .query_row(
                "SELECT name FROM sqlite_schema WHERE type = 'trigger' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        encrypted
            .execute_batch(&format!("DROP TRIGGER \"{trigger}\""))
            .unwrap();
        drop(encrypted);
        assert!(check(&path, &copy, &key).is_err());
        // And `convert` with such a copy keeps the plaintext database.
        assert_eq!(file_kind(&path).unwrap(), FileKind::Plaintext);
    }

    fn export_again(path: &Path, copy: &Path, key: &DatabaseKey) {
        remove_copy(copy);
        export(path, copy, key).unwrap();
    }

    #[test]
    fn a_postponed_conversion_opens_the_plaintext_database() {
        let tmp = TempDir::new();
        plaintext_database(&tmp.0);
        // A folder where the copy should go makes the export fail.
        fs::create_dir(path_with(&tmp.database(), ENCRYPTING_SUFFIX)).unwrap();
        let (store, opened) = Store::open_keyed(&tmp.0, &FakeKeychain::holding(KEY)).unwrap();
        assert_eq!(opened, Opened::Postponed(Postponed::Failed));
        assert_eq!(count(&store, "captures"), 31);
        assert_eq!(pragma(&store, "temp_store"), 1, "still plaintext");
        drop(store);
        assert_eq!(file_kind(&tmp.database()).unwrap(), FileKind::Plaintext);
    }

    #[test]
    fn forgotten_text_is_gone_from_an_encrypted_database_too() {
        let tmp = TempDir::new();
        let keys = FakeKeychain::holding(KEY);
        let (store, _) = Store::open_keyed(&tmp.0, &keys).unwrap();
        for _ in 0..200 {
            store.insert(&capture(MARKER)).unwrap();
        }
        let before = fs::metadata(tmp.database()).unwrap().len();
        store.forget(&Forget::Everything).unwrap();
        // The free pages went back without VACUUM, which would have
        // copied the database into memory.
        let after = fs::metadata(tmp.database()).unwrap().len();
        assert!(after < before, "{after} < {before}");
        let count: i64 = store
            .conn
            .query_row("SELECT count(*) FROM captures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
