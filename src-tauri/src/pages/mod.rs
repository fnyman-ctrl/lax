//! Memory pages (Stage 4 step 3, ADR 0008): one Markdown text per person,
//! organisation and project, and the About me page, each in the `text` of
//! its row in the encrypted database (ADR 0011).
//!
//! - Lax writes only between its markers, and checks before every write that
//!   the user's text is unchanged (`block::update`). If the user edited the
//!   block, the edit becomes theirs and Lax starts a new block.
//! - A page is written only if it still holds what Lax read, so an edit
//!   saved meanwhile is never overwritten, and not at all while it is open
//!   in the Memory view's editor.
//! - Every block Lax writes is kept as a version, with the episodes it drew
//!   on; forgetting one of them deletes the version.
//! - A page leaves the database only through the export (`export`).

pub mod block;
pub mod export;
pub mod facts;

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Local, TimeZone, Utc};
use tauri::{AppHandle, Manager};

use crate::logging::{self, ErrorKind, Field};
use crate::pause::Pause;
use crate::store::{Entity, EntityKind, Episode, Page, PageKind, PageState, Store};

/// Latest entries listed on a page.
const LATEST: usize = 10;
/// The editor saves no page bigger than this.
pub const MAX_PAGE_BYTES: u64 = 1 << 20;

/// The pages open in the Memory view's editors, by id, with how many
/// editors have each open. Lax doesn't write a page while it is open, so a
/// save rarely finds it changed (ADR 0011).
#[derive(Default)]
pub struct OpenPages(Mutex<HashMap<String, usize>>);

impl OpenPages {
    pub fn open(&self, id: &str) {
        *self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id.to_string())
            .or_default() += 1;
    }

    pub fn close(&self, id: &str) {
        let mut open = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = open.get_mut(id) {
            *count -= 1;
            if *count == 0 {
                open.remove(id);
            }
        }
    }

    fn ids(&self) -> HashSet<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect()
    }
}

/// The sync service a path is inside, if any, for the export. Dropbox,
/// Google Drive, OneDrive and Box live in `~/Library/CloudStorage` on
/// current macOS, or in their own folder on older setups.
pub(crate) fn synced(path: &Path) -> Option<&'static str> {
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

/// A path inside `root` that is only plain names (no `..`, no root), for
/// the export.
fn inside(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    relative
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then(|| root.join(relative))
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
fn content<Tz: TimeZone>(
    facts: &[String],
    entity: &Entity,
    all: &[Entity],
    latest: &[Episode],
    tz: &Tz,
) -> String
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
    // Projects in parts (ADR 0013).
    let mut parts: Vec<&str> = all
        .iter()
        .filter(|e| e.part_of == Some(entity.id))
        .map(|e| e.name.as_str())
        .collect();
    parts.sort_by_key(|name| name.to_lowercase());
    if !parts.is_empty() {
        text.push_str(&format!("Parts: {}.\n", parts.join(", ")));
    }
    if let Some(whole) = entity
        .part_of
        .and_then(|id| all.iter().find(|e| e.id == id))
    {
        text.push_str(&format!("Part of: {}.\n", whole.name));
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

/// A path no other page has, for a new page: `people/anna-example.md`, or
/// `about-me.md` for `None`. It only names the file an export writes.
fn new_path(store: &Store, kind: Option<EntityKind>, name: &str) -> crate::store::Result<String> {
    let taken = store.page_paths()?;
    let (folder, base) = match kind {
        Some(kind) => (format!("{}/", kind_folder(kind)), block::slug(name)),
        None => (String::new(), "about-me".to_string()),
    };
    let mut candidate = format!("{folder}{base}.md");
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = format!("{folder}{base}-{n}.md");
        n += 1;
    }
    Ok(candidate)
}

/// Brings the pages up to date with the memory, except the pages `open` in
/// an editor: new pages for entities Lax keeps, blocks rewritten when what
/// they say changed, forgotten entities' pages taken away. Never changes
/// the user's text.
pub fn sync<Tz: TimeZone>(
    store: &mut Store,
    open: &HashSet<String>,
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
        // About me is done below; the page open in the editor waits until
        // it is closed. A forgotten entity's page waits too.
        if page.kind == PageKind::AboutMe || open.contains(&page.id) {
            continue;
        }
        let entity = page
            .entity_id
            .and_then(|id| entities.iter().find(|e| e.id == id));

        // Forgotten: the page goes, with the user's text on it, which the
        // forget showed them first (ADR 0011). A forget usually took it
        // already.
        let Some(entity) = entity else {
            store.delete_page(&page.id)?;
            done.removed += 1;
            continue;
        };

        if !enabled {
            continue;
        }
        let found = store.page_text(&page.id)?;
        let latest = store.episodes_mentioning(&[entity.id], None, LATEST)?;
        refresh(
            store,
            &page,
            found,
            Refresh {
                title: &entity.name,
                content: &|facts| content(facts, entity, &entities, &latest, tz),
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
        let content = content(&[], entity, &entities, &latest, tz);
        let path = new_path(store, Some(entity.kind), &entity.name)?;
        let text = block::new_page(&entity.name, &id, &content);
        store.add_page(&id, entity.id, &path, &text, now)?;
        let sources: Vec<i64> = latest.iter().map(|e| e.id).collect();
        store.page_written(&id, &path, &block::normalize(&content), &sources, now)?;
        done.created += 1;
    }

    about_me(store, open, now, &mut done)?;

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
    /// The block, given the facts.
    content: &'a dyn Fn(&[String]) -> String,
    /// The episodes the block lists besides its facts.
    sources: Vec<i64>,
}

/// Brings one page, whose text is `found` (`None`: it has none), up to
/// date: facts the user deleted are rejected, then the block is rewritten
/// if what it says changed, the page is recreated if the user asked, or
/// marked missing or detached.
fn refresh(
    store: &mut Store,
    page: &Page,
    found: Option<String>,
    what: Refresh,
    now: DateTime<Utc>,
    done: &mut Synced,
) -> crate::store::Result<()> {
    // Facts the user deleted from the page are rejected: gone, and never
    // written there again.
    let mut facts = page.facts.clone().unwrap_or_default();
    if let Some(text) = &found {
        for line in block::removed_lines(text, &page.id, page.last_block.as_deref()) {
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
        // A new block of Lax's below the user's text on the same page, or
        // a new page if it had none.
        (PageState::Recreate, found) => {
            let text = match &found {
                Some(current) => format!(
                    "{}\n\n{}\n",
                    current.trim_end(),
                    block::block(&page.id, &content)
                ),
                None => block::new_page(what.title, &page.id, &content),
            };
            if store.set_page_text(&page.id, found.as_deref(), &text)? {
                store.page_written(
                    &page.id,
                    &page.path,
                    &block::normalize(&content),
                    &sources,
                    now,
                )?;
                done.created += 1;
            }
        }
        // No text: missing until the user asks for it back.
        (_, None) => {
            if page.state != PageState::Missing {
                store.set_page_state(&page.id, PageState::Missing, None)?;
                done.missing += 1;
            }
        }
        (_, Some(text)) => {
            match block::update(&text, &page.id, page.last_block.as_deref(), &content) {
                block::Update::Same => {
                    if page.state != PageState::Ok {
                        store.set_page_state(&page.id, PageState::Ok, None)?;
                    }
                }
                block::Update::Write { text: new, adopted } => {
                    // Only over what was read: an edit saved meanwhile wins.
                    if store
                        .set_page_text(&page.id, Some(&text), &new)
                        .unwrap_or(false)
                    {
                        if let Some(edit) = &adopted {
                            store.page_edited_by_user(&page.id, edit, now)?;
                            done.adopted += 1;
                        }
                        store.page_written(
                            &page.id,
                            &page.path,
                            &block::normalize(&content),
                            &sources,
                            now,
                        )?;
                        done.written += 1;
                    }
                }
                // Without Lax's markers: the user made it theirs.
                block::Update::NotOurs => {
                    if page.state != PageState::Detached {
                        store.set_page_state(&page.id, PageState::Detached, None)?;
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
    open: &HashSet<String>,
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
            let path = new_path(store, None, ABOUT_ME_TITLE)?;
            let text = block::new_page(ABOUT_ME_TITLE, &id, &content);
            store.add_about_me_page(&id, &path, &text, now)?;
            store.page_written(&id, &path, &block::normalize(&content), &[], now)?;
            done.created += 1;
        }
        Some(page) if open.contains(&page.id) => {}
        Some(page) => {
            let found = store.page_text(&page.id)?;
            refresh(
                store,
                &page,
                found,
                Refresh {
                    title: ABOUT_ME_TITLE,
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
/// was forgotten must leave the pages at once (GUARDRAILS §4).
pub fn sync_now(app: &AppHandle) {
    let Some(store) = app.try_state::<Mutex<Store>>() else {
        return;
    };
    let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
    let open = app
        .try_state::<OpenPages>()
        .map(|open| open.ids())
        .unwrap_or_default();
    if sync(&mut store, &open, Utc::now(), &Local).is_err() {
        logging::warn("pages.failed", &[Field::Error(ErrorKind::Database)]);
    }
}

#[cfg(test)]
mod tests;
