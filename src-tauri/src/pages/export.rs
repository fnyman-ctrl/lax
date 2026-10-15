//! Exporting the memory pages as Markdown files (ADR 0011, "Export"): the
//! only way a page leaves the encrypted database.
//!
//! - The folder is chosen with macOS's own folder picker, in Rust, so the
//!   web UI never hands Lax a path to write to.
//! - The window shows a notice first that the copy is not encrypted, and
//!   asks again for a folder inside iCloud Drive or another synced folder.
//! - Lax writes a new folder, `Lax pages <date>` (`0700`), with the layout
//!   the pages had in `~/Lax` and files `0600`, each page's whole text with
//!   its markers. It never overwrites anything.
//! - Logs hold the count, never names or paths.

#[cfg(test)]
use std::fs;
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::inside;
use crate::logging::{self, Field};
use crate::store::{Page, Store};

/// The folder the user picked for the next export, kept in Rust: the
/// window only ever learns its name.
#[derive(Default)]
pub struct ChosenFolder(std::sync::Mutex<Option<PathBuf>>);

impl ChosenFolder {
    pub fn set(&self, folder: Option<PathBuf>) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = folder;
    }

    /// The folder, once: an export uses up the pick, whatever happens.
    pub fn take(&self) -> Option<PathBuf> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// What an export wrote.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Exported {
    pub pages: usize,
    /// The new folder's name, `Lax pages 2026-10-15`, for the message.
    pub folder: String,
}

/// A page to export: its id, its path in the old layout, and its text.
pub struct ExportPage {
    pub id: String,
    pub path: String,
    /// `None` for a page without text (missing): skipped.
    pub text: Option<String>,
}

impl ExportPage {
    /// The page from `store`, with its text.
    pub fn of(store: &Store, page: &Page) -> crate::store::Result<Self> {
        Ok(ExportPage {
            id: page.id.clone(),
            path: page.path.clone(),
            text: store.page_text(&page.id)?,
        })
    }
}

/// Writes `pages` into a new folder inside `into`, named for `date`.
/// Nothing that exists is overwritten: the folder gets a number if one of
/// that name is there, and each file is created new.
pub fn write(pages: &[ExportPage], into: &Path, date: &str) -> std::io::Result<Exported> {
    let (dir, name) = new_folder(into, date)?;
    let mut written = 0;
    for page in pages {
        let Some(text) = &page.text else {
            continue;
        };
        // The page's own path, if it is plain names; else a name of its id.
        let path = inside(&dir, &page.path)
            .filter(|path| path.extension().is_some_and(|e| e == "md"))
            .unwrap_or_else(|| dir.join(format!("page-{}.md", page.id)));
        if let Some(parent) = path.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        // Two pages with one path (a detached page and a new one): the
        // second gets its id in the name. Never over an existing file.
        let mut file = None;
        for candidate in [
            path.clone(),
            path.with_file_name(format!("page-{}.md", page.id)),
        ] {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)
            {
                Ok(opened) => {
                    file = Some(opened);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        let mut file = file.ok_or(std::io::ErrorKind::AlreadyExists)?;
        file.write_all(text.as_bytes())?;
        written += 1;
    }
    logging::info("pages.exported", &[Field::Count("pages", written as u64)]);
    Ok(Exported {
        pages: written,
        folder: name,
    })
}

/// A new, private folder `Lax pages <date>` in `into`, or `… 2`, `… 3` if
/// that name is taken. Created, not reused, so nothing in it is old.
fn new_folder(into: &Path, date: &str) -> std::io::Result<(PathBuf, String)> {
    for n in 1..1000 {
        let name = if n == 1 {
            format!("Lax pages {date}")
        } else {
            format!("Lax pages {date} {n}")
        };
        let dir = into.join(&name);
        match DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok((dir, name)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

/// The sync service a chosen folder is inside, if any, after following
/// links, so the window can ask again before the copy leaves this Mac.
pub fn synced_service(folder: &Path) -> Option<&'static str> {
    let real = folder.canonicalize().ok()?;
    super::synced(&real)
}

/// Asks for a folder with macOS's folder picker, on the main thread, and
/// waits for the answer. `None` if the user cancelled.
#[cfg(target_os = "macos")]
pub fn pick_folder(app: &tauri::AppHandle) -> Option<PathBuf> {
    let (send, receive) = std::sync::mpsc::channel();
    let shown = app.run_on_main_thread(move || {
        let _ = send.send(macos::pick());
    });
    if shown.is_err() {
        return None;
    }
    receive.recv().ok().flatten()
}

#[cfg(not(target_os = "macos"))]
pub fn pick_folder(_app: &tauri::AppHandle) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;

    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::NSString;

    /// The folder picker, run modally. Only on the main thread.
    pub fn pick() -> Option<PathBuf> {
        let mtm = MainThreadMarker::new()?;
        // Lax lives in the menu bar: come to the front, so the picker does.
        // `activate` is macOS 14 and later; this works on every version.
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseDirectories(true);
        panel.setCanChooseFiles(false);
        panel.setAllowsMultipleSelection(false);
        panel.setCanCreateDirectories(true);
        panel.setPrompt(Some(&NSString::from_str("Export here")));
        panel.setMessage(Some(&NSString::from_str(
            "Choose where Lax puts a folder with your pages, as plain Markdown files.",
        )));
        if panel.runModal() != NSModalResponseOK {
            return None;
        }
        let url = panel.URLs().firstObject()?;
        url.path().map(|path| PathBuf::from(path.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    struct Temp(PathBuf);

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp() -> Temp {
        let id = super::super::block::new_id().unwrap();
        let dir = std::env::temp_dir().join(format!("lax-export-test-{id}"));
        fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }

    fn page(id: &str, path: &str, text: Option<&str>) -> ExportPage {
        ExportPage {
            id: id.to_string(),
            path: path.to_string(),
            text: text.map(String::from),
        }
    }

    #[test]
    fn pages_go_into_a_new_private_folder_and_nothing_is_overwritten() {
        let into = temp();
        let pages = [
            page("a1", "about-me.md", Some("# About me\n\nMine.\n")),
            page("a2", "people/gone.md", None),
            page("a3", "../outside.md", Some("# Odd\n")),
            page("a4", "people/anna-example.md", Some("# Anna Example\n")),
            page("a5", "people/anna-example.md", Some("# Anna, detached\n")),
        ];

        let first = write(&pages, &into.0, "2026-10-15").unwrap();
        assert_eq!(first.folder, "Lax pages 2026-10-15");
        assert_eq!(first.pages, 4, "a page without text is skipped");
        let dir = into.0.join(&first.folder);
        assert_eq!(
            fs::read_to_string(dir.join("about-me.md")).unwrap(),
            "# About me\n\nMine.\n"
        );
        // A path that isn't plain names stays inside the new folder.
        assert!(dir.join("page-a3.md").exists());
        assert!(dir.join("people/anna-example.md").exists());
        // The second page with that path gets its id in the name.
        assert_eq!(
            fs::read_to_string(dir.join("people/page-a5.md")).unwrap(),
            "# Anna, detached\n"
        );
        assert!(!into.0.join("outside.md").exists());
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("about-me.md")), 0o600);

        // Again the same day: a new folder, the first one untouched.
        fs::write(dir.join("about-me.md"), "Changed by the user.\n").unwrap();
        let second = write(&pages, &into.0, "2026-10-15").unwrap();
        assert_eq!(second.folder, "Lax pages 2026-10-15 2");
        assert_eq!(
            fs::read_to_string(dir.join("about-me.md")).unwrap(),
            "Changed by the user.\n"
        );
    }

    #[test]
    fn a_synced_folder_is_named() {
        let into = temp();
        assert_eq!(synced_service(&into.0), None);
        assert_eq!(
            super::super::synced(Path::new(
                "/Users/anna/Library/Mobile Documents/com~apple~CloudDocs/Export"
            )),
            Some("iCloud Drive")
        );
    }
}
