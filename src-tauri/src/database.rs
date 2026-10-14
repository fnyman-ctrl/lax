//! Opening the database when Lax starts, off the main thread (ADR 0010,
//! "Opening the database").
//!
//! Reading the key can make macOS ask for the login password, and opening
//! can take a while, so the tray and the window come first, with nothing
//! captured, and the database opens on a thread of its own. The window
//! shows where it stands ([`database_status`]):
//!
//! - **Opening:** "Opening the database…".
//! - **After an update:** Lax's own note before macOS asks, and nothing is
//!   read until the user chooses **Continue** ([`continue_opening`]).
//! - **Failed:** what went wrong, with **Try again** ([`retry_opening`]) and,
//!   where the key can't open the file, **Start over** ([`start_over`]).
//!
//! Capture, expiry, the journal and the writing assistant start only once
//! the store is open, as before.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Instant;

use chrono::Local;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::keychain::{self, ACCOUNT, BuildHash, KeyError, KeySource, LoginKeychain};
use crate::logging::{self, ErrorKind, Field};
use crate::pause::{CaptureState, Pause};
use crate::store::{DATABASE_FILE, FileKind, Store, StoreError, file_kind};

/// Tells the window that the database's state changed.
pub const CHANGED_EVENT: &str = "database-changed";
/// The code hash of the last build that read the key (ADR 0010, "After an
/// update"). Not secret, and outside the database, which can't be read
/// before the key.
const KEY_BUILD_FILE: &str = "key-build";

/// Where opening the database stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Opening,
    /// A new build is about to read the key: Lax's note first.
    UpdateNote,
    Open,
    Failed(Failure),
}

/// Why the database didn't open, without any detail that could hold the
/// key or captured text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// Locked, denied or cancelled: the key is probably fine.
    KeyUnavailable,
    /// An encrypted database and no key for it.
    KeyMissing,
    /// The key doesn't open the file, or the file is damaged.
    CannotUnlock,
    /// Made by a newer Lax.
    NewerSchema,
    /// Anything else: the folder or the disk.
    Other,
}

#[derive(Default)]
pub struct Database(Mutex<Option<State>>);

impl Database {
    fn get(&self) -> Option<State> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Moves to `next` if the current state is one of `from`; returns
    /// whether it did, so two clicks can't open the database twice.
    fn change(&self, from: &[Option<State>], next: State) -> bool {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if from.contains(&*state) {
            *state = Some(next);
            true
        } else {
            false
        }
    }

    fn set(&self, next: State) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(next);
    }
}

/// What the window shows.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseStatus {
    /// "opening", "update", "open", "keyUnavailable", "keyMissing",
    /// "cannotUnlock", "newerSchema" or "failed".
    state: &'static str,
}

#[tauri::command]
pub fn database_status(app: AppHandle) -> DatabaseStatus {
    let state = match app.state::<Database>().get() {
        None | Some(State::Opening) => "opening",
        Some(State::UpdateNote) => "update",
        Some(State::Open) => "open",
        Some(State::Failed(Failure::KeyUnavailable)) => "keyUnavailable",
        Some(State::Failed(Failure::KeyMissing)) => "keyMissing",
        Some(State::Failed(Failure::CannotUnlock)) => "cannotUnlock",
        Some(State::Failed(Failure::NewerSchema)) => "newerSchema",
        Some(State::Failed(Failure::Other)) => "failed",
    };
    DatabaseStatus { state }
}

/// After Lax's note: reads the key, and macOS asks.
#[tauri::command]
pub fn continue_opening(app: AppHandle) {
    let database = app.state::<Database>();
    if database.change(&[Some(State::UpdateNote)], State::Opening) {
        open_in_background(app.clone(), true);
    }
}

/// After a failure: tries again. The note isn't shown again; the user has
/// seen it, or the failure came before any key was read.
#[tauri::command]
pub fn retry_opening(app: AppHandle) {
    if app.state::<Database>().change(&failed(), State::Opening) {
        open_in_background(app.clone(), true);
    }
}

/// Starts over with an empty database, after the user confirmed it: only
/// when the key is missing or doesn't open the file. The old file is moved
/// aside, still encrypted, and the old key item is renamed, never deleted,
/// so the old file can still be opened if its key turns up (ADR 0010,
/// "When the key is missing or wrong").
///
/// Async, so a Keychain prompt for the rename never holds the main thread.
#[tauri::command(async)]
pub fn start_over(app: AppHandle) -> Result<(), String> {
    let database = app.state::<Database>();
    let before = database.get();
    let from = [
        Some(State::Failed(Failure::KeyMissing)),
        Some(State::Failed(Failure::CannotUnlock)),
    ];
    if !database.change(&from, State::Opening) {
        return Err("Lax can only start over when its key doesn't open the database.".into());
    }
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|_| "Lax couldn't find its data folder.".to_string())?;
    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    if let Err(error) = move_aside(&dir, &LoginKeychain::lax(), &stamp) {
        logging::warn("store.start_over_failed", &[Field::Error(ErrorKind::Other)]);
        // As it was: the same explanation, and Start over still offered.
        database.set(before.unwrap_or(State::Failed(Failure::CannotUnlock)));
        changed(&app);
        return Err(error);
    }
    logging::info("store.started_over", &[]);
    open_in_background(app.clone(), true);
    Ok(())
}

/// Opens the database at start.
pub fn start(app: &AppHandle) {
    if app.state::<Database>().change(&[None], State::Opening) {
        open_in_background(app.clone(), false);
    }
}

fn failed() -> Vec<Option<State>> {
    [
        Failure::KeyUnavailable,
        Failure::KeyMissing,
        Failure::CannotUnlock,
        Failure::NewerSchema,
        Failure::Other,
    ]
    .into_iter()
    .map(|failure| Some(State::Failed(failure)))
    .collect()
}

fn changed(app: &AppHandle) {
    let _ = app.emit(CHANGED_EVENT, ());
}

/// Opens the database on its own thread. `noted` says the user has seen
/// Lax's note, or doesn't need to.
fn open_in_background(app: AppHandle, noted: bool) {
    changed(&app);
    thread::spawn(move || {
        let next = open(&app, noted);
        app.state::<Database>().set(next);
        changed(&app);
        // Lax waits for the user, or can't go on: show them why, also when
        // the window would otherwise stay hidden.
        if matches!(next, State::UpdateNote | State::Failed(_)) {
            crate::tray::open_window(&app);
        }
    });
}

fn open(app: &AppHandle, noted: bool) -> State {
    let Ok(dir) = app.path().app_data_dir() else {
        return State::Failed(Failure::Other);
    };
    let kind = match file_kind(&dir.join(DATABASE_FILE)) {
        Ok(kind) => kind,
        Err(_) => return State::Failed(Failure::Other),
    };
    let build = keychain::own_build_hash();
    if !noted && needs_note(kind, recorded_build(&dir).as_deref(), build.as_ref()) {
        logging::info("store.update_note", &[]);
        return State::UpdateNote;
    }
    let started = Instant::now();
    match Store::open_keyed(&dir, &LoginKeychain::lax()) {
        Ok(store) => {
            // This build read the key, or made it: it may read it again
            // without a prompt.
            if kind != FileKind::Plaintext
                && let Some(build) = build
            {
                let _ = record_build(&dir, &build);
            }
            logging::info(
                "store.opened",
                &[
                    Field::Flag("encrypted", kind != FileKind::Plaintext),
                    Field::Flag("new", kind == FileKind::Empty),
                    Field::Duration("elapsed", started.elapsed()),
                ],
            );
            opened(app, store);
            State::Open
        }
        Err(error) => {
            let failure = failure(&error);
            logging::warn(
                "store.open_failed",
                &[
                    Field::Flag("key", failure.is_key()),
                    Field::Error(failure.kind()),
                ],
            );
            State::Failed(failure)
        }
    }
}

impl Failure {
    /// Whether the key is the trouble, for the log.
    fn is_key(self) -> bool {
        matches!(self, Failure::KeyUnavailable | Failure::KeyMissing)
    }

    /// What kind of error it is, for the log.
    fn kind(self) -> ErrorKind {
        match self {
            Failure::KeyUnavailable | Failure::KeyMissing => ErrorKind::Permission,
            Failure::CannotUnlock | Failure::NewerSchema => ErrorKind::Database,
            Failure::Other => ErrorKind::Io,
        }
    }
}

fn failure(error: &StoreError) -> Failure {
    match error {
        StoreError::KeyUnavailable => Failure::KeyUnavailable,
        StoreError::KeyMissing => Failure::KeyMissing,
        StoreError::CannotUnlock => Failure::CannotUnlock,
        StoreError::NewerSchema { .. } => Failure::NewerSchema,
        _ => Failure::Other,
    }
}

/// Everything that waits for the store, as Lax did at start before.
fn opened(app: &AppHandle, store: Store) {
    // A pause set before Lax was quit still holds. If it can't be read,
    // capture stays paused: stopping by mistake is safer than capturing by
    // mistake.
    let pause = app.state::<Pause>();
    pause.restore(
        store
            .pause_state()
            .unwrap_or(CaptureState::Paused { until: None }),
    );
    // Capture only starts with the user's exclusions in place. If they
    // can't be read, it doesn't start at all: capturing an app the user
    // excluded is the worse mistake.
    let exclusions = store.exclusions();
    app.manage(Mutex::new(store));
    crate::tray::refresh(app);
    crate::expiry::start(app.clone());
    // Does nothing until the user turns the journal on. The memory runs in
    // the same loop, after it.
    crate::journal::start(app.clone());
    match exclusions {
        Ok(exclusions) => {
            app.state::<crate::exclusions::Current>().set(exclusions);
            // Capture checks the pause and the Accessibility permission
            // before every read, so it can start now.
            crate::capture::start(app.clone());
        }
        Err(_) => {
            app.manage(crate::viewer::StoreUnavailable(
                "your exclusions couldn't be read".to_string(),
            ));
        }
    }
    // Listens for ⌥ Option only if the user turned the writing assistant on.
    crate::summon::start(app);
}

/// Whether Lax shows its note before reading the key: when a key is in use
/// (an encrypted database, or an empty one after Lax made a key before) and
/// this build isn't the one that last read it. A build whose hash can't be
/// read counts as new.
fn needs_note(kind: FileKind, recorded: Option<&str>, build: Option<&BuildHash>) -> bool {
    let key_in_use = match kind {
        FileKind::Encrypted => true,
        FileKind::Empty => recorded.is_some(),
        FileKind::Plaintext => false,
    };
    key_in_use && (build.map(BuildHash::to_hex).as_deref() != recorded || build.is_none())
}

fn recorded_build(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(KEY_BUILD_FILE))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn record_build(dir: &Path, build: &BuildHash) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(dir.join(KEY_BUILD_FILE))?;
    file.write_all(build.to_hex().as_bytes())
}

/// "Start over": renames the key item first, then moves the database and
/// its write-ahead files aside, all with the same `stamp`. If the key can't
/// be renamed, nothing moves: a new database would otherwise get the old
/// key. No key item is fine: that is why the user starts over.
fn move_aside(dir: &Path, keys: &impl KeySource, stamp: &str) -> Result<(), String> {
    match keys.rename_item(&format!("{ACCOUNT}-unreadable-{stamp}")) {
        Ok(()) | Err(KeyError::Missing) => {}
        Err(_) => {
            return Err("Lax couldn't put its old key aside in the keychain. Try again.".into());
        }
    }
    for suffix in ["", "-wal", "-shm"] {
        let from = dir.join(format!("{DATABASE_FILE}{suffix}"));
        if from.exists() {
            let to = dir.join(format!("{DATABASE_FILE}.unreadable-{stamp}{suffix}"));
            fs::rename(&from, &to)
                .map_err(|_| "Lax couldn't move the old database aside.".to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::keychain::fake::FakeKeychain;

    const KEY: &[u8] = b"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "lax-database-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn hash(byte: u8) -> BuildHash {
        BuildHash::from_bytes(vec![byte; 20])
    }

    #[test]
    fn the_note_shows_only_when_a_new_build_reads_a_key_in_use() {
        let this = hash(1);
        let other = hash(2).to_hex();
        let same = this.to_hex();
        // An encrypted database: a new build, a missing record or an
        // unknown build shows the note; the same build doesn't.
        assert!(needs_note(FileKind::Encrypted, Some(&other), Some(&this)));
        assert!(needs_note(FileKind::Encrypted, None, Some(&this)));
        assert!(needs_note(FileKind::Encrypted, Some(&same), None));
        assert!(!needs_note(FileKind::Encrypted, Some(&same), Some(&this)));
        // A plaintext database reads no key; a new install has none yet.
        assert!(!needs_note(FileKind::Plaintext, Some(&other), Some(&this)));
        assert!(!needs_note(FileKind::Empty, None, Some(&this)));
        // An empty database after a key was made by another build.
        assert!(needs_note(FileKind::Empty, Some(&other), Some(&this)));
    }

    #[test]
    fn the_build_is_recorded_privately_and_read_back() {
        let tmp = TempDir::new();
        assert_eq!(recorded_build(&tmp.0), None);
        record_build(&tmp.0, &hash(7)).unwrap();
        assert_eq!(recorded_build(&tmp.0), Some(hash(7).to_hex()));
        record_build(&tmp.0, &hash(8)).unwrap();
        assert_eq!(recorded_build(&tmp.0), Some(hash(8).to_hex()));
        let mode = fs::metadata(tmp.0.join(KEY_BUILD_FILE))
            .unwrap()
            .permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o600
        );
    }

    #[test]
    fn starting_over_keeps_the_old_file_and_key() {
        let tmp = TempDir::new();
        let keys = FakeKeychain::holding(KEY);
        drop(Store::open_keyed(&tmp.0, &keys).unwrap());
        move_aside(&tmp.0, &keys, "20261015-010203").unwrap();
        // The old key is kept under another name, with its data.
        assert_eq!(
            *keys.renamed.borrow(),
            [(
                "database-key-unreadable-20261015-010203".to_string(),
                KEY.to_vec()
            )]
        );
        // The old file is kept, still encrypted, next to where it was.
        let old = tmp.0.join("lax.sqlite.unreadable-20261015-010203");
        assert_eq!(file_kind(&old).unwrap(), FileKind::Encrypted);
        assert!(!tmp.0.join(DATABASE_FILE).exists());
        // The next start makes a new key and an empty database.
        drop(Store::open_keyed(&tmp.0, &keys).unwrap());
        assert!(keys.item.borrow().is_some());
        assert_eq!(keys.renamed.borrow().len(), 1);
        assert_eq!(
            file_kind(&tmp.0.join(DATABASE_FILE)).unwrap(),
            FileKind::Encrypted
        );
    }

    #[test]
    fn starting_over_moves_nothing_if_the_key_cant_be_put_aside() {
        let tmp = TempDir::new();
        let keys = FakeKeychain::holding(KEY);
        drop(Store::open_keyed(&tmp.0, &keys).unwrap());
        struct Refusing;
        impl KeySource for Refusing {
            fn read_item(&self) -> Result<zeroize::Zeroizing<Vec<u8>>, KeyError> {
                Err(KeyError::Denied)
            }
            fn add_item(&self, _: &[u8]) -> Result<keychain::Added, KeyError> {
                Err(KeyError::Denied)
            }
            fn rename_item(&self, _: &str) -> Result<(), KeyError> {
                Err(KeyError::Denied)
            }
        }
        assert!(move_aside(&tmp.0, &Refusing, "x").is_err());
        assert_eq!(
            file_kind(&tmp.0.join(DATABASE_FILE)).unwrap(),
            FileKind::Encrypted
        );
        // With no key at all, the file still moves: that is why the user
        // starts over.
        let none = FakeKeychain::default();
        move_aside(&tmp.0, &none, "y").unwrap();
        assert!(!tmp.0.join(DATABASE_FILE).exists());
    }

    #[test]
    fn failures_have_fixed_names() {
        assert_eq!(failure(&StoreError::KeyMissing), Failure::KeyMissing);
        assert_eq!(failure(&StoreError::CannotUnlock), Failure::CannotUnlock);
        assert_eq!(
            failure(&StoreError::KeyUnavailable),
            Failure::KeyUnavailable
        );
        assert_eq!(
            failure(&StoreError::Io(std::io::Error::other("disk"))),
            Failure::Other
        );
        assert!(Failure::KeyMissing.is_key());
        assert!(!Failure::CannotUnlock.is_key());
    }
}
