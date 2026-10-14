//! The encrypted database (ADR 0010).
//!
//! A new database is encrypted with SQLCipher under the key in the login
//! keychain ([`crate::keychain`]). A plaintext one opens as before until it is
//! converted (rollout step 4). An encrypted one opens only with its own key:
//! a missing key is an error, never a reason to make a new one.
//!
//! The key never appears in SQL: it is set with `sqlite3_key_v2`, and every
//! failure on the way is one of the fixed errors in [`StoreError`], with no
//! SQLite text, which can quote the statement it failed on.

// One call into SQLite's C API, to set the key; see SAFETY below.
#![allow(unsafe_code)]

use std::ffi::c_int;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

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

impl Store {
    /// Opens (or creates) the database in `data_dir`, encrypted under the
    /// key from `keys`:
    ///
    /// - **Empty:** the stored key, or a new one if there is none, stored
    ///   and read back first; then a new encrypted database.
    /// - **Plaintext:** opened as before, without reading the key, until it
    ///   is converted.
    /// - **Encrypted:** the stored key only. Without it, [`StoreError::
    ///   KeyMissing`]; nothing new is made.
    ///
    /// Reading an existing key may make macOS ask for the login password
    /// (after an update); the caller says so first (ADR 0010, "After an
    /// update").
    pub fn open_keyed(data_dir: &Path, keys: &impl KeySource) -> Result<Self> {
        let path = database_file(data_dir)?;
        match file_kind(&path)? {
            FileKind::Plaintext => Self::open(data_dir),
            FileKind::Empty => {
                let key = key(keys, DatabaseFile::NotEncrypted)?;
                let conn = Connection::open(&path)?;
                unlock(&conn, &key)?;
                // Before WAL: set after it, it silently stays off.
                conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
                conn.pragma_update(None, "journal_mode", "WAL")?;
                Self::init(conn, Layout::Encrypted)
            }
            FileKind::Encrypted => {
                let key = key(keys, DatabaseFile::Encrypted)?;
                let conn = Connection::open(&path)?;
                unlock(&conn, &key)?;
                conn.pragma_update(None, "journal_mode", "WAL")?;
                Self::init(conn, Layout::Encrypted)
            }
        }
    }
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
    use crate::store::{DATABASE_FILE, Forget, NewCapture};

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
        let store = Store::open_keyed(&tmp.0, &keys).unwrap();
        assert_eq!(keys.adds.get(), 1, "a key was made");
        assert!(keys.item.borrow().is_some());
        for _ in 0..20 {
            store.insert(&capture(MARKER)).unwrap();
        }
        store.set_setting("note", MARKER).unwrap();
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
        let store = Store::open_keyed(&tmp.0, &keys).unwrap();
        store.insert(&capture("hello")).unwrap();
        drop(store);
        let store = Store::open_keyed(&tmp.0, &keys).unwrap();
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

    #[test]
    fn a_plaintext_database_opens_as_before_without_the_key() {
        let tmp = TempDir::new();
        let store = Store::open(&tmp.0).unwrap();
        store.insert(&capture("hello")).unwrap();
        drop(store);
        let keys = FakeKeychain::holding(KEY);
        let store = Store::open_keyed(&tmp.0, &keys).unwrap();
        assert_eq!(
            keys.reads.get(),
            0,
            "the key isn't read for a plaintext file"
        );
        assert_eq!(pragma(&store, "temp_store"), 1, "temp_store stays FILE");
        drop(store);
        assert_eq!(file_kind(&tmp.database()).unwrap(), FileKind::Plaintext);
    }

    #[test]
    fn forgotten_text_is_gone_from_an_encrypted_database_too() {
        let tmp = TempDir::new();
        let keys = FakeKeychain::holding(KEY);
        let store = Store::open_keyed(&tmp.0, &keys).unwrap();
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
