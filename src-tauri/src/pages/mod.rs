//! Memory pages (Stage 4 step 3, ADR 0008): one Markdown file per person,
//! organisation and project in the memory folder, `~/Lax/`.
//!
//! - The folder is private (`0700`, files `0600`), and refused if it is
//!   inside iCloud Drive or another synced folder: syncing would send the
//!   memory off the Mac (GUARDRAILS §1).
//! - Lax writes only between its markers, and checks before every write that
//!   the user's text is unchanged (`block::update`). If the user edited the
//!   block, the edit becomes theirs and Lax starts a new block.
//! - Pages are found by the id in their marker, so the user can rename and
//!   move them within the folder. A page Lax can't find is shown as missing,
//!   never recreated or forgotten without asking.
//! - Files are written whole to a temporary file and renamed over the page,
//!   only if the page hasn't changed since it was read.
//! - Every block Lax writes is kept as a version, with the episodes it drew
//!   on; forgetting one of them deletes the version.

pub mod block;
pub mod facts;

use std::collections::HashMap;
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Local, TimeZone, Utc};
use tauri::{AppHandle, Manager};

use crate::logging::{self, ErrorKind, Field};
use crate::pause::Pause;
use crate::store::{Entity, EntityKind, Episode, PageState, Store};

/// The memory folder in the user's home folder.
pub fn folder(home: &Path) -> PathBuf {
    home.join("Lax")
}

/// Latest entries listed on a page.
const LATEST: usize = 10;
/// Pages bigger than this aren't read: not a page Lax wrote.
const MAX_PAGE_BYTES: u64 = 1 << 20;
/// How deep below the folder pages are looked for.
const MAX_DEPTH: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderError {
    /// Inside a folder that is synced off the Mac.
    Synced(&'static str),
    Io,
}

impl std::fmt::Display for FolderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FolderError::Synced(service) => write!(
                f,
                "The memory folder is inside {service}, which would copy it off this Mac, so \
                 Lax doesn't write pages there."
            ),
            FolderError::Io => f.write_str("Lax couldn't open the memory folder."),
        }
    }
}

/// Creates the folder (`0700`) and checks that it isn't synced.
pub fn prepare_folder(dir: &Path) -> Result<(), FolderError> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|_| FolderError::Io)?;
    fs::set_permissions(dir, Permissions::from_mode(0o700)).map_err(|_| FolderError::Io)?;
    let real = dir.canonicalize().map_err(|_| FolderError::Io)?;
    match synced(&real) {
        Some(service) => Err(FolderError::Synced(service)),
        None => Ok(()),
    }
}

/// The sync service a path is inside, if any. Dropbox, Google Drive,
/// OneDrive and Box live in `~/Library/CloudStorage` on current macOS, or in
/// their own folder on older setups.
fn synced(path: &Path) -> Option<&'static str> {
    let path = path.to_string_lossy();
    if path.contains("/Library/Mobile Documents/") || path.ends_with("/Library/Mobile Documents") {
        return Some("iCloud Drive");
    }
    if path.contains("/Library/CloudStorage/") {
        return Some("a cloud storage folder");
    }
    for (folder, service) in [
        ("/Dropbox", "Dropbox"),
        ("/Google Drive", "Google Drive"),
        ("/OneDrive", "OneDrive"),
    ] {
        if path.contains(&format!("{folder}/")) || path.ends_with(folder) {
            return Some(service);
        }
    }
    None
}

/// What a scan last read of each file: its modification time, size and
/// page id, so unchanged files aren't read again every round.
pub type ScanCache = HashMap<PathBuf, (std::time::SystemTime, u64, Option<String>)>;

/// Every page in the folder, by the id in its marker, with every file that
/// carries it (a page the user copied has two). Hidden folders, symbolic
/// links and big files are skipped; files unchanged since the last scan
/// aren't read again.
pub fn scan(dir: &Path, cache: &mut ScanCache) -> HashMap<String, Vec<PathBuf>> {
    let mut found = HashMap::new();
    let mut seen = ScanCache::new();
    scan_into(dir, dir, 0, cache, &mut seen, &mut found);
    *cache = seen;
    found
}

fn scan_into(
    root: &Path,
    dir: &Path,
    depth: usize,
    cache: &ScanCache,
    seen: &mut ScanCache,
    found: &mut HashMap<String, Vec<PathBuf>>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if depth < MAX_DEPTH {
                scan_into(root, &path, depth + 1, cache, seen, found);
            }
            continue;
        }
        if !meta.is_file()
            || meta.len() > MAX_PAGE_BYTES
            || path.extension().is_none_or(|e| e != "md")
        {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let id = match cache.get(&path) {
            Some((at, len, id)) if *at == modified && *len == meta.len() => id.clone(),
            _ => fs::read_to_string(&path)
                .ok()
                .and_then(|text| block::page_id(&text).map(String::from)),
        };
        seen.insert(path.clone(), (modified, meta.len(), id.clone()));
        if let (Some(id), Ok(relative)) = (id, path.strip_prefix(root)) {
            found.entry(id).or_default().push(relative.to_path_buf());
        }
    }
}

/// Writes `text` to `path` if the file still holds `expected` (or, for
/// `None`, doesn't exist yet): to a private temporary file first, then
/// renamed over it. `false` if the file changed meanwhile.
pub fn write(path: &Path, expected: Option<&str>, text: &str) -> std::io::Result<bool> {
    let current = match fs::read_to_string(path) {
        Ok(current) => Some(current),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if current.as_deref() != expected {
        return Ok(false);
    }
    let dir = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let name = path
        .file_name()
        .ok_or(std::io::ErrorKind::InvalidInput)?
        .to_string_lossy();
    let temp = dir.join(format!(".{name}.lax-tmp"));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)?;
    Ok(true)
}

/// A path inside the folder that is only plain names (no `..`, no root).
fn inside(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    relative
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then(|| root.join(relative))
}

/// Writes a page only if its folder is really inside the memory folder and
/// not synced: a subfolder the user replaced with a symbolic link to iCloud
/// Drive is refused like the memory folder itself.
fn write_inside(
    root: &Path,
    path: &Path,
    expected: Option<&str>,
    text: &str,
) -> std::io::Result<bool> {
    let parent = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let (root, parent) = (root.canonicalize()?, parent.canonicalize()?);
    if !parent.starts_with(&root) || synced(&parent).is_some() {
        return Ok(false);
    }
    write(path, expected, text)
}

fn kind_folder(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Person => "people",
        EntityKind::Organisation => "organisations",
        EntityKind::Project => "projects",
    }
}

/// A path for a new page that no file has yet.
fn free_path(root: &Path, kind: EntityKind, name: &str) -> String {
    let base = block::slug(name);
    let mut candidate = format!("{}/{base}.md", kind_folder(kind));
    let mut n = 2;
    while root.join(&candidate).exists() {
        candidate = format!("{}/{base}-{n}.md", kind_folder(kind));
        n += 1;
    }
    candidate
}

/// What Lax writes on a page, before the model writes facts (step 4): the
/// names, how often and when it was mentioned, and the latest entries.
/// Defused, since summaries come from captured text.
fn content<Tz: TimeZone>(facts: &[String], entity: &Entity, latest: &[Episode], tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let date = |t: DateTime<Utc>| t.with_timezone(tz).format("%Y-%m-%d").to_string();
    let mut text = String::new();
    if !facts.is_empty() {
        for fact in facts {
            text.push_str(&format!("- {}\n", fact.replace(['\n', '\r'], " ")));
        }
        text.push('\n');
    }
    if !entity.aliases.is_empty() {
        text.push_str(&format!("Also known as: {}.\n", entity.aliases.join(", ")));
    }
    let (mentions, days) = (entity.mentions, entity.days);
    let entries = if mentions == 1 { "entry" } else { "entries" };
    let on = if days == 1 { "day" } else { "days" };
    text.push_str(&format!(
        "Mentioned in {mentions} journal {entries} on {days} {on}, from {} to {}.\n",
        date(entity.first_seen),
        date(entity.last_seen)
    ));
    if !latest.is_empty() {
        text.push_str("\nLatest entries:\n");
        for episode in latest {
            let start = episode.started_at.with_timezone(tz);
            let apps = if episode.app_names.is_empty() {
                String::new()
            } else {
                format!(" ({})", episode.app_names.join(", "))
            };
            let summary = episode.summary.replace(['\n', '\r'], " ");
            text.push_str(&format!(
                "- {}{apps}: {summary}\n",
                start.format("%Y-%m-%d %H:%M")
            ));
        }
    }
    block::defuse(&text)
}

/// What a sync did, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Synced {
    pub created: usize,
    pub written: usize,
    pub adopted: usize,
    pub removed: usize,
    pub missing: usize,
    pub rejected: usize,
}

/// Brings the pages in `dir` up to date with the memory: new pages for
/// entities Lax keeps, blocks rewritten when what they say changed, blocks
/// taken out for forgotten entities. Never deletes the user's text.
pub fn sync<Tz: TimeZone>(
    store: &mut Store,
    dir: &Path,
    cache: &mut ScanCache,
    now: DateTime<Utc>,
    tz: &Tz,
) -> crate::store::Result<Synced>
where
    Tz::Offset: std::fmt::Display,
{
    let mut done = Synced::default();
    let on_disk = scan(dir, cache);
    let entities = store.entities()?;
    // Off, the memory writes nothing new; forgotten entities still leave.
    let enabled = store.memory_enabled()?;

    for page in store.pages()? {
        // The file where the page was last seen, if it still carries the id;
        // otherwise the first other file that does.
        let found = on_disk.get(&page.id).and_then(|paths| {
            paths
                .iter()
                .find(|p| p.to_string_lossy() == page.path)
                .or_else(|| paths.first())
                .cloned()
        });
        let entity = page
            .entity_id
            .and_then(|id| entities.iter().find(|e| e.id == id));

        // Forgotten: take Lax's block out, and the file if nothing of the
        // user's is left in it.
        // The page is forgotten only once Lax's block is out of the file
        // (or there is no file); otherwise it is tried again next round, so
        // forgotten text can't stay behind in a file Lax no longer knows.
        let Some(entity) = entity else {
            let cleared = match &found {
                None => true,
                Some(relative) => {
                    let path = dir.join(relative);
                    match fs::read_to_string(&path) {
                        Err(_) => false,
                        Ok(text) => match block::without_block(&text, &page.id) {
                            None => true,
                            Some(rest) if block::only_a_title(&rest) => {
                                fs::read_to_string(&path).ok().as_deref() == Some(text.as_str())
                                    && fs::remove_file(&path).is_ok()
                            }
                            Some(rest) => {
                                write_inside(dir, &path, Some(&text), &rest).unwrap_or(false)
                            }
                        },
                    }
                }
            };
            if cleared {
                store.delete_page(&page.id)?;
                done.removed += 1;
            }
            continue;
        };

        if !enabled {
            continue;
        }
        // Facts the user deleted from the page are rejected: gone, and never
        // written there again.
        let mut facts = page.facts.clone().unwrap_or_default();
        if let Some(relative) = &found
            && let Ok(text) = fs::read_to_string(dir.join(relative))
        {
            for line in block::removed_lines(&text, &page.id, page.last_block.as_deref()) {
                let Some(fact) = line.strip_prefix("- ") else {
                    continue;
                };
                let key = crate::store::fact_key(fact);
                if facts
                    .iter()
                    .any(|f| crate::store::fact_key(&block::defuse(f)) == key)
                {
                    store.reject_fact(&page.id, fact, now)?;
                    facts.retain(|f| crate::store::fact_key(&block::defuse(f)) != key);
                    done.rejected += 1;
                }
            }
        }
        let latest = store.episodes_mentioning(&[entity.id], None, LATEST)?;
        let content = content(&facts, entity, &latest, tz);
        let sources: Vec<i64> = latest.iter().map(|e| e.id).collect();

        match (page.state, found) {
            (PageState::Recreate, _) => {
                let path = free_path(dir, entity.kind, &entity.name);
                let Some(full) = inside(dir, &path) else {
                    continue;
                };
                if write_inside(
                    dir,
                    &full,
                    None,
                    &block::new_page(&entity.name, &page.id, &content),
                )
                .unwrap_or(false)
                {
                    store.page_written(
                        &page.id,
                        &path,
                        &block::normalize(&content),
                        &sources,
                        now,
                    )?;
                    done.created += 1;
                }
            }
            (_, None) => {
                // Still there, but without Lax's markers: the user made it
                // theirs. Gone: missing.
                let detached = inside(dir, &page.path)
                    .and_then(|path| fs::read_to_string(path).ok())
                    .is_some_and(|text| block::page_id(&text) != Some(page.id.as_str()));
                let state = if detached {
                    PageState::Detached
                } else {
                    PageState::Missing
                };
                if page.state != state {
                    store.set_page_state(&page.id, state, None)?;
                    done.missing += 1;
                }
            }
            (_, Some(relative)) => {
                let path = dir.join(&relative);
                let relative = relative.to_string_lossy().to_string();
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                match block::update(&text, &page.id, page.last_block.as_deref(), &content) {
                    block::Update::Same => {
                        if page.state != PageState::Ok || page.path != relative {
                            store.set_page_state(&page.id, PageState::Ok, Some(&relative))?;
                        }
                    }
                    block::Update::Write { text: new, adopted } => {
                        if write_inside(dir, &path, Some(&text), &new).unwrap_or(false) {
                            if let Some(edit) = &adopted {
                                store.page_edited_by_user(&page.id, edit, now)?;
                                done.adopted += 1;
                            }
                            store.page_written(
                                &page.id,
                                &relative,
                                &block::normalize(&content),
                                &sources,
                                now,
                            )?;
                            done.written += 1;
                        }
                    }
                    block::Update::NotOurs => {
                        if page.state != PageState::Detached {
                            store.set_page_state(&page.id, PageState::Detached, Some(&relative))?;
                        }
                    }
                }
            }
        }
    }

    // New pages for entities Lax keeps and hasn't written yet.
    if !enabled {
        return Ok(done);
    }
    let has_page: Vec<Option<i64>> = store.pages()?.iter().map(|p| p.entity_id).collect();
    for entity in entities.iter().filter(|e| !e.is_new()) {
        if has_page.contains(&Some(entity.id)) {
            continue;
        }
        let Ok(id) = block::new_id() else {
            continue;
        };
        let latest = store.episodes_mentioning(&[entity.id], None, LATEST)?;
        let content = content(&[], entity, &latest, tz);
        let path = free_path(dir, entity.kind, &entity.name);
        let Some(full) = inside(dir, &path) else {
            continue;
        };
        if write_inside(
            dir,
            &full,
            None,
            &block::new_page(&entity.name, &id, &content),
        )
        .unwrap_or(false)
        {
            store.add_page(&id, entity.id, &path, now)?;
            let sources: Vec<i64> = latest.iter().map(|e| e.id).collect();
            store.page_written(&id, &path, &block::normalize(&content), &sources, now)?;
            done.created += 1;
        }
    }

    if done != Synced::default() {
        logging::info(
            "pages.synced",
            &[
                Field::Count("created", done.created as u64),
                Field::Count("written", done.written as u64),
                Field::Count("adopted", done.adopted as u64),
                Field::Count("removed", done.removed as u64),
                Field::Count("missing", done.missing as u64),
                Field::Count("rejected", done.rejected as u64),
            ],
        );
    }
    Ok(done)
}

/// One round of the background loop, after the memory's. Skipped while
/// paused.
pub fn round(app: &AppHandle) {
    if !app.state::<Pause>().is_paused() {
        sync_now(app);
    }
}

/// Brings the pages up to date now, also while paused: after a forget, what
/// was forgotten must leave the files at once (GUARDRAILS §4).
pub fn sync_now(app: &AppHandle) {
    let Ok(home) = app.path().home_dir() else {
        return;
    };
    let dir = folder(&home);
    let Some(store) = app.try_state::<Mutex<Store>>() else {
        return;
    };
    let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
    let enabled = store.memory_enabled().unwrap_or(false);
    let has_pages = store.pages().is_ok_and(|pages| !pages.is_empty());
    // Off and nothing written: not even the folder is created.
    if !enabled && !has_pages {
        return;
    }
    if let Err(error) = prepare_folder(&dir) {
        logging::warn(
            "pages.folder_refused",
            &[Field::Flag(
                "synced",
                matches!(error, FolderError::Synced(_)),
            )],
        );
        return;
    }
    static CACHE: std::sync::OnceLock<Mutex<ScanCache>> = std::sync::OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if sync(&mut store, &dir, &mut cache, Utc::now(), &Local).is_err() {
        logging::warn("pages.failed", &[Field::Error(ErrorKind::Database)]);
    }
}

/// Why pages can't be written, if they can't, for the Memory tab.
pub fn folder_problem(app: &AppHandle) -> Option<String> {
    let home = app.path().home_dir().ok()?;
    prepare_folder(&folder(&home)).err().map(|e| e.to_string())
}

/// The memory folder, for showing and opening.
pub fn folder_of(app: &AppHandle) -> Option<PathBuf> {
    app.path().home_dir().ok().map(|home| folder(&home))
}

#[cfg(test)]
mod tests;
