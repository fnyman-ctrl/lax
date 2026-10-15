//! Moving the memory pages from `~/Lax` into the database, once (ADR 0011,
//! "Moving the existing pages in"), and deleting the old files if the user
//! agrees. Only on an encrypted database: pages never move into a
//! plaintext one. In a module of its own, to be removed once the move has
//! been in a release long enough.
//!
//! - Each page's file is found by the rules the page round uses (the file
//!   at its path if it carries the page's id, else the first that does),
//!   read whole, twice, until two reads agree, and kept as the page's text
//!   unchanged. Edits in Lax's block are adopted at the next round.
//! - All of it, and the setting that pages now live in the database, is
//!   written in one transaction: a stop before it changes nothing.
//! - Missing pages stay missing; a file at a page's path without any
//!   markers moves in as the user's, detached; of copies, the one chosen
//!   moves and the others stay; files Lax doesn't know are never moved or
//!   deleted.
//! - The old files are deleted only when the user agrees, and only those
//!   that still hold exactly what was moved (a fingerprint, not a copy:
//!   a copy would outlive a forget).

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use super::{MAX_PAGE_BYTES, ScanCache, block, inside, scan};
use crate::logging::{self, Field};
use crate::store::{MovedFile, MovedPage, PageState, Store};

/// What the move did, for the log and the tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Moved {
    pub pages: usize,
    pub detached: usize,
    pub missing: usize,
    /// Other files carrying a moved page's id, left in place.
    pub copies: usize,
}

/// FNV-1a over `bytes`: enough to tell whether an old file still holds
/// what was moved.
pub fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A page file's whole text: a regular file (no symbolic link), UTF-8, at
/// most `MAX_PAGE_BYTES`. Read again until two reads agree, so a file being
/// saved meanwhile is read as it ends up.
fn read_whole(path: &Path) -> Option<String> {
    let read = || -> Option<String> {
        let meta = fs::symlink_metadata(path).ok()?;
        if !meta.is_file() || meta.len() > MAX_PAGE_BYTES {
            return None;
        }
        fs::read_to_string(path).ok()
    };
    let mut last = read()?;
    for _ in 0..3 {
        let again = read()?;
        if again == last {
            return Some(last);
        }
        last = again;
    }
    None
}

/// Moves the pages in from `dir`, if they aren't in yet and the database is
/// encrypted. `None` if there was nothing to do.
pub fn move_in(store: &mut Store, dir: &Path) -> crate::store::Result<Option<Moved>> {
    if store.pages_in_database()? || !store.is_encrypted() {
        return Ok(None);
    }
    let on_disk = if dir.is_dir() {
        scan(dir, &mut ScanCache::new())
    } else {
        HashMap::new()
    };
    let mut moved = Moved::default();
    let mut pages = Vec::new();
    let mut files = Vec::new();
    let keep = |path: &str, text: &str, files: &mut Vec<MovedFile>| {
        files.push(MovedFile {
            path: path.to_string(),
            hash: fingerprint(text.as_bytes()),
            bytes: text.len() as u64,
        });
    };
    for page in store.pages()? {
        let carrying = on_disk.get(&page.id);
        let chosen = carrying.and_then(|paths| {
            paths
                .iter()
                .find(|p| p.to_string_lossy() == page.path)
                .or_else(|| paths.first())
                .cloned()
        });
        moved.copies += carrying.map_or(0, |paths| paths.len().saturating_sub(1));
        let found = chosen.and_then(|relative| {
            let relative = relative.to_string_lossy().to_string();
            read_whole(&dir.join(&relative)).map(|text| (relative, text))
        });
        let (text, state, path) = match found {
            Some((path, text)) => {
                keep(&path, &text, &mut files);
                moved.pages += 1;
                (Some(text), PageState::Ok, path)
            }
            None => match inside(dir, &page.path).and_then(|path| read_whole(&path)) {
                // The user took Lax's markers out: their text moves in, and
                // Lax doesn't write to it.
                Some(text) if block::page_id(&text).is_none() => {
                    keep(&page.path, &text, &mut files);
                    moved.detached += 1;
                    (Some(text), PageState::Detached, page.path.clone())
                }
                _ => {
                    moved.missing += 1;
                    let state = if page.state == PageState::Recreate {
                        PageState::Recreate
                    } else {
                        PageState::Missing
                    };
                    (None, state, page.path.clone())
                }
            },
        };
        pages.push(MovedPage {
            id: page.id.clone(),
            text,
            state,
            path,
        });
    }
    store.move_pages_in(&pages, &files)?;
    logging::info(
        "pages.moved_in",
        &[
            Field::Count("pages", moved.pages as u64),
            Field::Count("detached", moved.detached as u64),
            Field::Count("missing", moved.missing as u64),
            Field::Count("copies", moved.copies as u64),
        ],
    );
    Ok(Some(moved))
}

/// What deleting the old files did.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Deleted {
    pub deleted: usize,
    /// Files changed since the move, relative to `~/Lax`: kept.
    pub changed: Vec<String>,
}

/// Deletes the old page files that still hold exactly what was moved, then
/// the folders left empty, and `dir` itself if it is empty. A file changed
/// since the move is kept, and listed. Files Lax didn't move are never
/// touched.
pub fn delete_old_files(store: &Store, dir: &Path) -> crate::store::Result<Deleted> {
    let mut done = Deleted::default();
    let mut handled = Vec::new();
    let root = dir.canonicalize().ok();
    for file in store.moved_page_files()? {
        let Some(path) = inside(dir, &file.path) else {
            handled.push(file.path);
            continue;
        };
        // Only inside the folder, also if a subfolder became a link.
        let real_parent = path.parent().and_then(|parent| parent.canonicalize().ok());
        if root.is_none()
            || real_parent
                .zip(root.as_ref())
                .is_none_or(|(p, r)| !p.starts_with(r))
        {
            handled.push(file.path);
            continue;
        }
        match fs::read(&path) {
            Ok(bytes) if bytes.len() as u64 == file.bytes && fingerprint(&bytes) == file.hash => {
                if fs::remove_file(&path).is_ok() {
                    done.deleted += 1;
                    handled.push(file.path);
                }
            }
            Ok(_) => {
                done.changed.push(file.path.clone());
                handled.push(file.path);
            }
            Err(error) if error.kind() == ErrorKind::NotFound => handled.push(file.path),
            Err(_) => {}
        }
    }
    store.forget_moved_page_files(&handled)?;
    for folder in ["people", "organisations", "projects"] {
        remove_if_empty(&dir.join(folder));
    }
    remove_if_empty(dir);
    logging::info(
        "pages.old_files_deleted",
        &[
            Field::Count("deleted", done.deleted as u64),
            Field::Count("changed", done.changed.len() as u64),
        ],
    );
    Ok(done)
}

/// Removes `dir` if nothing is in it but Finder's `.DS_Store`.
fn remove_if_empty(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let names: Vec<_> = entries.flatten().map(|entry| entry.file_name()).collect();
    if names.iter().all(|name| name == ".DS_Store") {
        let _ = fs::remove_file(dir.join(".DS_Store"));
        let _ = fs::remove_dir(dir);
    }
}
