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

use std::collections::{HashMap, HashSet};
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Local, TimeZone, Utc};
use tauri::{AppHandle, Manager};

use crate::logging::{self, ErrorKind, Field};
use crate::pause::Pause;
use crate::store::{Entity, EntityKind, Episode, Page, PageKind, PageState, Store};

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

/// Where the pages are kept: files in the memory folder until they have
/// moved into the database (ADR 0011), then the pages' rows. The same rules
/// hold for both: Lax changes a page only through `block::update`, and only
/// if the page still holds what Lax read.
pub enum Shelf<'a> {
    /// `~/Lax`, its files found by the page ids in their markers.
    Folder {
        dir: &'a Path,
        on_disk: HashMap<String, Vec<PathBuf>>,
    },
    /// The `text` of each page's row.
    Rows,
}

/// Where one page's text was found.
#[derive(Debug, Clone)]
enum Spot {
    /// A file, relative to the memory folder.
    File(PathBuf),
    /// The page's row, with the text it held when found, so a round reads
    /// each page once.
    Row(String),
}

impl Shelf<'_> {
    /// Where the page's text is: for files, the one at the page's path if
    /// it still carries the page's id, otherwise the first other that does.
    fn find(&self, store: &Store, page: &Page) -> crate::store::Result<Option<Spot>> {
        Ok(match self {
            Shelf::Folder { on_disk, .. } => on_disk
                .get(&page.id)
                .and_then(|paths| {
                    paths
                        .iter()
                        .find(|p| p.to_string_lossy() == page.path)
                        .or_else(|| paths.first())
                        .cloned()
                })
                .map(Spot::File),
            Shelf::Rows => store.page_text(&page.id)?.map(Spot::Row),
        })
    }

    /// The page's text where it was found. A file is read now; a row's
    /// text was read when it was found.
    fn read(&self, spot: &Spot) -> Option<String> {
        match spot {
            Spot::File(relative) => match self {
                Shelf::Folder { dir, .. } => fs::read_to_string(dir.join(relative)).ok(),
                Shelf::Rows => unreachable!("a row shelf finds no files"),
            },
            Spot::Row(text) => Some(text.clone()),
        }
    }

    /// Replaces the page's text with `text` if it still is `expected`.
    fn write(&self, store: &Store, id: &str, spot: &Spot, expected: &str, text: &str) -> bool {
        match spot {
            Spot::File(relative) => match self {
                Shelf::Folder { dir, .. } => {
                    write_inside(dir, &dir.join(relative), Some(expected), text).unwrap_or(false)
                }
                Shelf::Rows => unreachable!("a row shelf finds no files"),
            },
            Spot::Row(_) => store
                .set_page_text(id, Some(expected), text)
                .unwrap_or(false),
        }
    }

    /// The page's path, where its text was found.
    fn path(page: &Page, spot: &Spot) -> String {
        match spot {
            Spot::File(relative) => relative.to_string_lossy().to_string(),
            Spot::Row(_) => page.path.clone(),
        }
    }

    /// A path no other page has, for a new page: `people/anna-example.md`,
    /// or `about-me.md` for `None`. In the database it only names the file
    /// an export writes.
    fn new_path(
        &self,
        store: &Store,
        kind: Option<EntityKind>,
        name: &str,
    ) -> crate::store::Result<String> {
        let taken = match self {
            Shelf::Folder { .. } => HashSet::new(),
            Shelf::Rows => store.page_paths()?,
        };
        let free = |candidate: &str| match self {
            Shelf::Folder { dir, .. } => !dir.join(candidate).exists(),
            Shelf::Rows => !taken.contains(candidate),
        };
        let (folder, base) = match kind {
            Some(kind) => (format!("{}/", kind_folder(kind)), block::slug(name)),
            None => (String::new(), "about-me".to_string()),
        };
        let mut candidate = format!("{folder}{base}.md");
        let mut n = 2;
        while !free(&candidate) {
            candidate = format!("{folder}{base}-{n}.md");
            n += 1;
        }
        Ok(candidate)
    }

    /// Puts a new page's `text` in place for the page `add` records, given
    /// the text to keep on its row: the file first and then the row
    /// without text, so no row points at a file that wasn't written; or the
    /// row with its text, in one statement.
    fn put_new(
        &self,
        store: &mut Store,
        path: &str,
        text: &str,
        add: impl FnOnce(&mut Store, Option<&str>) -> crate::store::Result<()>,
    ) -> crate::store::Result<bool> {
        match self {
            Shelf::Folder { dir, .. } => {
                let Some(full) = inside(dir, path) else {
                    return Ok(false);
                };
                if !write_inside(dir, &full, None, text).unwrap_or(false) {
                    return Ok(false);
                }
                add(store, None)?;
                Ok(true)
            }
            Shelf::Rows => {
                add(store, Some(text))?;
                Ok(true)
            }
        }
    }

    /// Takes a forgotten entity's page away; `true` once it is gone. A file
    /// loses Lax's block, and goes if nothing of the user's is left in it;
    /// it is tried again next round if that fails, so forgotten text can't
    /// stay behind in a file Lax no longer knows. A row goes whole, with
    /// the user's text on it, which the forget showed them first (ADR 0011).
    fn take_away(&self, page: &Page, found: Option<&Spot>) -> bool {
        match (self, found) {
            (_, None) | (_, Some(Spot::Row(_))) => true,
            (Shelf::Rows, Some(Spot::File(_))) => unreachable!("a row shelf finds no files"),
            (Shelf::Folder { dir, .. }, Some(Spot::File(relative))) => {
                let path = dir.join(relative);
                match fs::read_to_string(&path) {
                    Err(_) => false,
                    Ok(text) => match block::without_block(&text, &page.id) {
                        None => true,
                        Some(rest) if block::only_a_title(&rest) => {
                            fs::read_to_string(&path).ok().as_deref() == Some(text.as_str())
                                && fs::remove_file(&path).is_ok()
                        }
                        Some(rest) => write_inside(dir, &path, Some(&text), &rest).unwrap_or(false),
                    },
                }
            }
        }
    }

    /// For a page whose text wasn't found: whether it is still there but
    /// without Lax's markers (detached), rather than gone (missing).
    fn detached(&self, page: &Page) -> bool {
        match self {
            Shelf::Folder { dir, .. } => inside(dir, &page.path)
                .and_then(|path| fs::read_to_string(path).ok())
                .is_some_and(|text| block::page_id(&text) != Some(page.id.as_str())),
            Shelf::Rows => false,
        }
    }
}

/// Brings the pages in the memory folder `dir` up to date. See
/// [`sync_shelf`].
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
    let shelf = Shelf::Folder {
        dir,
        on_disk: scan(dir, cache),
    };
    sync_shelf(store, &shelf, now, tz)
}

/// Brings the pages in the database up to date. See [`sync_shelf`].
pub fn sync_rows<Tz: TimeZone>(
    store: &mut Store,
    now: DateTime<Utc>,
    tz: &Tz,
) -> crate::store::Result<Synced>
where
    Tz::Offset: std::fmt::Display,
{
    sync_shelf(store, &Shelf::Rows, now, tz)
}

/// Brings the pages up to date with the memory: new pages for entities Lax
/// keeps, blocks rewritten when what they say changed, forgotten entities'
/// pages taken away. Never changes the user's text.
fn sync_shelf<Tz: TimeZone>(
    store: &mut Store,
    shelf: &Shelf,
    now: DateTime<Utc>,
    tz: &Tz,
) -> crate::store::Result<Synced>
where
    Tz::Offset: std::fmt::Display,
{
    let mut done = Synced::default();
    let entities = store.entities()?;
    // Off, the memory writes nothing new; forgotten entities still leave.
    let enabled = store.memory_enabled()?;

    for page in store.pages()? {
        if page.kind == PageKind::AboutMe {
            continue;
        }
        let found = shelf.find(store, &page)?;
        let entity = page
            .entity_id
            .and_then(|id| entities.iter().find(|e| e.id == id));

        // Forgotten: the page goes, once its text is out of the way.
        let Some(entity) = entity else {
            if shelf.take_away(&page, found.as_ref()) {
                store.delete_page(&page.id)?;
                done.removed += 1;
            }
            continue;
        };

        if !enabled {
            continue;
        }
        let latest = store.episodes_mentioning(&[entity.id], None, LATEST)?;
        refresh(
            store,
            shelf,
            &page,
            found,
            Refresh {
                title: &entity.name,
                kind: Some(entity.kind),
                content: &|facts| content(facts, entity, &latest, tz),
                sources: latest.iter().map(|e| e.id).collect(),
            },
            now,
            &mut done,
        )?;
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
        let path = shelf.new_path(store, Some(entity.kind), &entity.name)?;
        let text = block::new_page(&entity.name, &id, &content);
        if shelf.put_new(store, &path, &text, |store, text| {
            store.add_page(&id, entity.id, &path, text, now)
        })? {
            let sources: Vec<i64> = latest.iter().map(|e| e.id).collect();
            store.page_written(&id, &path, &block::normalize(&content), &sources, now)?;
            done.created += 1;
        }
    }

    about_me(store, shelf, now, &mut done)?;

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

/// What a page says, for [`refresh`].
struct Refresh<'a> {
    /// The title of a page written anew.
    title: &'a str,
    /// What the page is about, for naming one written anew; `None` for the
    /// About me page.
    kind: Option<EntityKind>,
    /// The block, given the facts.
    content: &'a dyn Fn(&[String]) -> String,
    /// The episodes the block lists besides its facts.
    sources: Vec<i64>,
}

/// Brings one page up to date: facts the user deleted are rejected, then
/// the block is rewritten if what it says changed, the page is recreated if
/// the user asked, or marked missing or detached.
fn refresh(
    store: &mut Store,
    shelf: &Shelf,
    page: &Page,
    found: Option<Spot>,
    what: Refresh,
    now: DateTime<Utc>,
    done: &mut Synced,
) -> crate::store::Result<()> {
    // Facts the user deleted from the page are rejected: gone, and never
    // written there again.
    let mut facts = page.facts.clone().unwrap_or_default();
    if let Some(spot) = &found
        && let Some(text) = shelf.read(spot)
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
    let content = (what.content)(&facts);
    // A version holds the facts too, so it goes when an episode they drew
    // on is forgotten, not only one of the listed entries.
    let mut sources = what.sources;
    for source in store.fact_sources(&page.id)? {
        if !sources.contains(&source) {
            sources.push(source);
        }
    }

    match (page.state, found) {
        (PageState::Recreate, found) => {
            let written = match shelf {
                // A new file next to the old one.
                Shelf::Folder { dir, .. } => {
                    let path = shelf.new_path(store, what.kind, what.title)?;
                    inside(dir, &path)
                        .filter(|full| {
                            write_inside(
                                dir,
                                full,
                                None,
                                &block::new_page(what.title, &page.id, &content),
                            )
                            .unwrap_or(false)
                        })
                        .map(|_| path)
                }
                // A new block of Lax's below the user's text on the same
                // page, or a new page if it had none.
                Shelf::Rows => {
                    let current = found.and_then(|spot| shelf.read(&spot));
                    let text = match &current {
                        Some(current) => format!(
                            "{}\n\n{}\n",
                            current.trim_end(),
                            block::block(&page.id, &content)
                        ),
                        None => block::new_page(what.title, &page.id, &content),
                    };
                    store
                        .set_page_text(&page.id, current.as_deref(), &text)?
                        .then(|| page.path.clone())
                }
            };
            if let Some(path) = written {
                store.page_written(&page.id, &path, &block::normalize(&content), &sources, now)?;
                done.created += 1;
            }
        }
        (_, None) => {
            // Still there, but without Lax's markers: the user made it
            // theirs. Gone: missing.
            let state = if shelf.detached(page) {
                PageState::Detached
            } else {
                PageState::Missing
            };
            if page.state != state {
                store.set_page_state(&page.id, state, None)?;
                done.missing += 1;
            }
        }
        (_, Some(spot)) => {
            let path = Shelf::path(page, &spot);
            let Some(text) = shelf.read(&spot) else {
                return Ok(());
            };
            match block::update(&text, &page.id, page.last_block.as_deref(), &content) {
                block::Update::Same => {
                    if page.state != PageState::Ok || page.path != path {
                        store.set_page_state(&page.id, PageState::Ok, Some(&path))?;
                    }
                }
                block::Update::Write { text: new, adopted } => {
                    if shelf.write(store, &page.id, &spot, &text, &new) {
                        if let Some(edit) = &adopted {
                            store.page_edited_by_user(&page.id, edit, now)?;
                            done.adopted += 1;
                        }
                        store.page_written(
                            &page.id,
                            &path,
                            &block::normalize(&content),
                            &sources,
                            now,
                        )?;
                        done.written += 1;
                    }
                }
                block::Update::NotOurs => {
                    if page.state != PageState::Detached {
                        store.set_page_state(&page.id, PageState::Detached, Some(&path))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// How far back `about-me.md` looks for who and what the user works with.
const ABOUT_ME_DAYS: i64 = 30;
const ABOUT_ME_TITLE: &str = "About me";

/// `about-me.md`: what the model learned about the user, then who and what
/// they worked with most lately. Defused, like every block.
fn about_me_content(facts: &[String], lately: &AboutMe) -> String {
    let mut text = String::new();
    for fact in facts {
        text.push_str(&format!("- {}\n", fact.replace(['\n', '\r'], " ")));
    }
    if !facts.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!("In the last {ABOUT_ME_DAYS} days:\n"));
    for (label, names) in [
        ("Most in touch with", &lately.people),
        ("Projects", &lately.projects),
        ("Organisations", &lately.organisations),
        ("Apps", &lately.apps),
    ] {
        if !names.is_empty() {
            text.push_str(&format!("{label}: {}.\n", names.join(", ")));
        }
    }
    block::defuse(&text)
}

/// Who and what the user worked with most in the last `ABOUT_ME_DAYS`.
struct AboutMe {
    people: Vec<String>,
    projects: Vec<String>,
    organisations: Vec<String>,
    apps: Vec<String>,
}

fn lately(store: &Store, now: DateTime<Utc>) -> crate::store::Result<AboutMe> {
    let since = now - chrono::TimeDelta::days(ABOUT_ME_DAYS);
    let names = |kind| -> crate::store::Result<Vec<String>> {
        Ok(store
            .most_mentioned_since(kind, since, 5)?
            .into_iter()
            .map(|(name, _)| name)
            .collect())
    };
    Ok(AboutMe {
        people: names(EntityKind::Person)?,
        projects: names(EntityKind::Project)?,
        organisations: names(EntityKind::Organisation)?,
        apps: store.most_used_apps_since(since, 5)?,
    })
}

/// Writes or updates the About me page, with the memory on.
fn about_me(
    store: &mut Store,
    shelf: &Shelf,
    now: DateTime<Utc>,
    done: &mut Synced,
) -> crate::store::Result<()> {
    if !store.memory_enabled()? {
        return Ok(());
    }
    let lately = lately(store, now)?;
    match store.about_me_page()? {
        None => {
            let Ok(id) = block::new_id() else {
                return Ok(());
            };
            let content = about_me_content(&[], &lately);
            let path = shelf.new_path(store, None, ABOUT_ME_TITLE)?;
            let text = block::new_page(ABOUT_ME_TITLE, &id, &content);
            if shelf.put_new(store, &path, &text, |store, text| {
                store.add_about_me_page(&id, &path, text, now)
            })? {
                store.page_written(&id, &path, &block::normalize(&content), &[], now)?;
                done.created += 1;
            }
        }
        Some(page) => {
            let found = shelf.find(store, &page)?;
            refresh(
                store,
                shelf,
                &page,
                found,
                Refresh {
                    title: ABOUT_ME_TITLE,
                    kind: None,
                    content: &|facts| about_me_content(facts, &lately),
                    sources: Vec::new(),
                },
                now,
                done,
            )?;
            // It names other entities: a forgotten one mustn't stay in its
            // history, so only the latest version is kept.
            store.keep_only_latest_version(&page.id)?;
        }
    }
    Ok(())
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
    // Moved into the database: no folder to prepare.
    if store.pages_in_database().unwrap_or(false) {
        if sync_rows(&mut store, Utc::now(), &Local).is_err() {
            logging::warn("pages.failed", &[Field::Error(ErrorKind::Database)]);
        }
        return;
    }
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
