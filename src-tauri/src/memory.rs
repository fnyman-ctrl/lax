//! The memory (Stage 4, ADR 0008), first step: recognizing the people,
//! organisations and projects that journal episodes name.
//!
//! - Off until the user turns it on, and only with a chat model chosen.
//! - It reads episodes, never raw captures, and runs in the journal's loop
//!   right after it, so the two never ask the model for two things at once.
//! - The episode text reaches the model only as `Captured` data (it was
//!   written from captured text), and the answer must be the `Entities`
//!   shape (GUARDRAILS §5).
//! - A pause drops a call in progress, and what an answer names is stored
//!   only if its episode still exists, so nothing is learned from forgotten
//!   episodes (GUARDRAILS §4).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use chrono::Utc;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::journal::{When, may_use_model};
use crate::logging::{self, ErrorKind, Field};
use crate::models::structured::{self, Captured, Entities, StructuredError};
use crate::models::{Endpoint, ModelClient, ModelTurn};
use crate::pages;
use crate::pause::Pause;
use crate::store::{
    EntityKind, Merged, ModelTask, Page, PageState, Renamed, Store, UnrecognizedEpisode, name_key,
};

/// At most this many episodes are read per round, so the journal gets its
/// turn again soon.
const EPISODES_PER_ROUND: usize = 5;
/// Bad answers about one episode before it is left unread.
const ATTEMPTS_PER_EPISODE: u32 = 3;
/// Room for a model that reasons before answering, as for the journal.
const MAX_TOKENS: u32 = 2000;

/// Episodes the model has answered badly about, and how often. Only in
/// memory.
#[derive(Default)]
pub struct Setbacks {
    failures: HashMap<i64, u32>,
}

impl Setbacks {
    /// Counts a bad answer about `episode`. `true` once it has had
    /// `ATTEMPTS_PER_EPISODE`: then it should be left unread, and is no
    /// longer counted.
    fn bad_answer(&mut self, episode: i64) -> bool {
        let count = self.failures.entry(episode).or_insert(0);
        *count += 1;
        if *count >= ATTEMPTS_PER_EPISODE {
            self.failures.remove(&episode);
            return true;
        }
        false
    }

    /// The episode was read after all: stop counting it.
    fn done(&mut self, episode: i64) {
        self.failures.remove(&episode);
    }
}

/// An episode to read, and the model to do it.
struct Work {
    base_url: String,
    model: String,
    episode: UnrecognizedEpisode,
    /// The names the user goes by: from the macOS account and the ones they
    /// gave.
    own_names: Vec<String>,
}

/// One round: read up to `EPISODES_PER_ROUND` new episodes. `Err` means the
/// model server is unavailable and the loop should wait a while.
pub async fn round(app: &AppHandle, setbacks: &mut Setbacks) -> Result<(), ()> {
    for _ in 0..EPISODES_PER_ROUND {
        if app.state::<Pause>().is_paused() || !may_use_model(When::Later) {
            return Ok(());
        }
        let Some(work) = next_work(app) else {
            return Ok(());
        };
        let started = std::time::Instant::now();
        let Some(answer) = app
            .state::<ModelTurn>()
            .background(&app.state::<Pause>(), recognize(&work))
            .await
        else {
            return Ok(());
        };
        if app.state::<Pause>().is_paused() {
            return Ok(());
        }
        match answer {
            Ok(entities) => {
                setbacks.done(work.episode.id);
                let names = names_in(entities, &work.episode.app_names, &work.own_names);
                let stored = with_store(app, |store| {
                    store.record_mentions(work.episode.id, &names, Utc::now())
                });
                logging::info(
                    "memory.episode_read",
                    &[
                        Field::Count("names", names.len() as u64),
                        Field::Flag("stored", matches!(stored, Some(Ok(true)))),
                        Field::Duration("elapsed", started.elapsed()),
                    ],
                );
            }
            Err(error) => {
                let unavailable = error.model_unavailable();
                logging::warn(
                    "memory.failed",
                    &[
                        Field::Flag("model_unavailable", unavailable),
                        Field::Error(ErrorKind::Other),
                    ],
                );
                if unavailable {
                    return Err(());
                }
                if setbacks.bad_answer(work.episode.id) {
                    // Marked read with nothing in it, so it doesn't hold up
                    // the rest.
                    let _ = with_store(app, |store| {
                        store.record_mentions(work.episode.id, &[], Utc::now())
                    });
                    logging::warn("memory.episode_set_aside", &[]);
                }
                // Next round rather than at once, as in the journal.
                return Ok(());
            }
        }
    }
    Ok(())
}

/// The next unread episode, if the memory is on and has a model.
fn next_work(app: &AppHandle) -> Option<Work> {
    with_store(app, |store| -> crate::store::Result<Option<Work>> {
        if !store.memory_enabled()? {
            return Ok(None);
        }
        let Some(choice) = store.model_choice(ModelTask::Chat)? else {
            return Ok(None);
        };
        let Some(episode) = store.unrecognized_episodes(1)?.into_iter().next() else {
            return Ok(None);
        };
        let mut own_names = own_names();
        own_names.extend(store.own_names()?);
        Ok(Some(Work {
            base_url: choice.base_url,
            model: choice.model,
            episode,
            own_names,
        }))
    })
    .and_then(Result::ok)
    .flatten()
}

async fn recognize(work: &Work) -> Result<Entities, StructuredError> {
    let endpoint =
        Endpoint::parse(&work.base_url).map_err(|_| StructuredError::NotTheRequestedJson)?;
    let client = ModelClient::new(endpoint).map_err(StructuredError::Model)?;
    structured::ask(
        &client,
        &work.model,
        Entities::TASK,
        Entities::SHAPE,
        &[Captured(&work.episode.summary)],
        MAX_TOKENS,
    )
    .await
}

/// The names in `entities`, by kind, without the episode's own apps (the
/// model is told apps aren't organisations, and this makes sure) and without
/// the user, who is not an entity of their own memory (ADR 0008).
fn names_in(
    entities: Entities,
    app_names: &[String],
    own_names: &[String],
) -> Vec<(EntityKind, String)> {
    let apps: Vec<String> = app_names.iter().map(|n| name_key(n)).collect();
    let user: Vec<String> = own_names.iter().map(|n| name_key(n)).collect();
    let mut names = Vec::new();
    for (kind, list) in [
        (EntityKind::Person, entities.people),
        (EntityKind::Organisation, entities.organisations),
        (EntityKind::Project, entities.projects),
    ] {
        for name in list {
            let key = name_key(&name);
            if apps.contains(&key) || (kind == EntityKind::Person && user.contains(&key)) {
                continue;
            }
            names.push((kind, name));
        }
    }
    names
}

/// The names the user goes by, from their macOS account: the full name and
/// its first word ("Anna Example" and "Anna"). Another person with the same
/// first name is left out too; asking the user for their names comes with
/// `about-me.md` (ADR 0008).
fn own_names() -> Vec<String> {
    #[cfg(target_os = "macos")]
    let full = objc2_foundation::NSFullUserName().to_string();
    #[cfg(not(target_os = "macos"))]
    let full = String::new();
    names_of(&full)
}

fn names_of(full: &str) -> Vec<String> {
    let full = full.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut names = Vec::new();
    if let Some(first) = full.split(' ').next().filter(|f| *f != full) {
        names.push(first.to_string());
    }
    if !full.is_empty() {
        names.push(full);
    }
    names
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStatus {
    pub enabled: bool,
    /// The chat model that reads the episodes, if one is chosen.
    pub model: Option<String>,
    /// Episodes not read yet.
    pub waiting: usize,
    /// Whether the journal is on; without it no new episodes arrive.
    pub journal: bool,
    /// The memory folder, with `~` for the home folder.
    pub folder: Option<String>,
    /// Why pages can't be written there, if they can't.
    pub folder_problem: Option<String>,
    /// Other names the user goes by, as they gave them.
    pub own_names: Vec<String>,
    /// `about-me.md`, once written.
    pub about_me: Option<PageItem>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EntityItem {
    pub id: i64,
    pub kind: &'static str,
    pub name: String,
    pub aliases: Vec<String>,
    pub mentions: usize,
    pub days: usize,
    pub last_seen: i64,
    /// Still new: seen on fewer than two days, and not kept by the user.
    pub is_new: bool,
    pub page: Option<PageItem>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PageItem {
    /// Relative to the memory folder.
    pub path: String,
    /// "ok", "missing", "detached" or "recreate".
    pub state: &'static str,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DontLearnItem {
    pub kind: &'static str,
    pub name: String,
}

fn page_item(page: &Page) -> PageItem {
    PageItem {
        path: page.path.clone(),
        state: match page.state {
            PageState::Ok => "ok",
            PageState::Missing => "missing",
            PageState::Detached => "detached",
            PageState::Recreate => "recreate",
        },
    }
}

fn status(store: &Store) -> crate::store::Result<MemoryStatus> {
    Ok(MemoryStatus {
        enabled: store.memory_enabled()?,
        model: store.model_choice(ModelTask::Chat)?.map(|c| c.model),
        waiting: store.unrecognized_count()?,
        journal: store.journal_enabled()?,
        folder: None,
        folder_problem: None,
        own_names: store.own_names()?,
        about_me: store.about_me_page()?.map(|p| page_item(&p)),
    })
}

#[tauri::command(async)]
pub fn memory_status(app: AppHandle) -> Result<MemoryStatus, String> {
    let mut status = command(&app, |store| {
        status(store).map_err(|_| "Lax couldn't read the memory.".to_string())
    })?;
    with_folder(&app, &mut status);
    Ok(status)
}

/// Adds where the pages are, and, with the memory on, whether they can be
/// written there.
fn with_folder(app: &AppHandle, status: &mut MemoryStatus) {
    let folder = pages::folder_of(app);
    let home = app.path().home_dir().ok();
    status.folder = folder.map(
        |f| match home.as_ref().and_then(|h| f.strip_prefix(h).ok()) {
            Some(relative) => format!("~/{}", relative.display()),
            None => f.display().to_string(),
        },
    );
    if status.enabled {
        status.folder_problem = pages::folder_problem(app);
    }
}

/// Turns the memory on or off. It can only be turned on with a chat model
/// chosen. Turning it off stops it learning; what it learned stays until
/// the user forgets it.
#[tauri::command(async)]
pub fn set_memory_enabled(app: AppHandle, enabled: bool) -> Result<MemoryStatus, String> {
    command(&app, |store| {
        let failed = |_| "Lax couldn't change the memory setting.".to_string();
        if enabled
            && store
                .model_choice(ModelTask::Chat)
                .map_err(failed)?
                .is_none()
        {
            return Err("Choose a chat model under Models first.".to_string());
        }
        store.set_memory_enabled(enabled).map_err(failed)?;
        status(store).map_err(failed)
    })
    .map(|mut status| {
        with_folder(&app, &mut status);
        pages::sync_now(&app);
        status
    })
}

#[tauri::command(async)]
pub fn memory_entities(app: AppHandle) -> Result<Vec<EntityItem>, String> {
    command(&app, |store| {
        let pages: HashMap<i64, Page> = store
            .pages()
            .map_err(|_| "Lax couldn't read the memory.".to_string())?
            .into_iter()
            .filter_map(|p| Some((p.entity_id?, p)))
            .collect();
        store
            .entities()
            .map(|entities| {
                entities
                    .into_iter()
                    .map(|e| EntityItem {
                        page: pages.get(&e.id).map(page_item),
                        is_new: e.is_new(),
                        id: e.id,
                        kind: e.kind.as_str(),
                        name: e.name,
                        aliases: e.aliases,
                        mentions: e.mentions,
                        days: e.days,
                        last_seen: e.last_seen.timestamp_millis(),
                    })
                    .collect()
            })
            .map_err(|_| "Lax couldn't read the memory.".to_string())
    })
}

/// Forgets everything the memory noted and turns it off.
#[tauri::command(async)]
pub fn forget_memory(app: AppHandle) -> Result<usize, String> {
    let forgotten = command(&app, |store| {
        store
            .forget_memory()
            .map_err(|_| "Lax couldn't forget what the memory noted.".to_string())
    })?;
    pages::sync_now(&app);
    Ok(forgotten)
}

/// Forgets one entity: its names and mentions. The episodes stay. With
/// `dont_learn`, its names go on the "don't learn" list so later episodes
/// can't bring it back; without, a later episode that names it starts anew.
#[tauri::command(async)]
pub fn forget_entity(app: AppHandle, id: i64, dont_learn: bool) -> Result<bool, String> {
    let existed = command(&app, |store| {
        store
            .forget_entity(id, dont_learn, Utc::now())
            .map_err(|_| "Lax couldn't forget it.".to_string())
    })?;
    pages::sync_now(&app);
    Ok(existed)
}

/// Keeps a new entity, as if it had been seen on two days.
#[tauri::command(async)]
pub fn keep_entity(app: AppHandle, id: i64) -> Result<bool, String> {
    let kept = command(&app, |store| {
        store
            .keep_entity(id)
            .map_err(|_| "Lax couldn't keep it.".to_string())
    })?;
    pages::sync_now(&app);
    Ok(kept)
}

/// Sets the other names the user goes by (nicknames, a maiden name): kept
/// out of the memory's people, and part of `about-me.md`.
#[tauri::command(async)]
pub fn set_own_names(app: AppHandle, names: Vec<String>) -> Result<MemoryStatus, String> {
    let mut clean: Vec<String> = Vec::new();
    for name in names
        .iter()
        .filter(|n| !n.trim().is_empty())
        .take(MAX_OWN_NAMES)
    {
        let name = valid_name(name)?;
        if !clean.iter().any(|n| name_key(n) == name_key(&name)) {
            clean.push(name);
        }
    }
    let mut status = command(&app, |store| {
        let failed = |_| "Lax couldn't save your names.".to_string();
        store.set_own_names(&clean).map_err(failed)?;
        // Noted before the user named them as theirs: forgotten now.
        store
            .forget_people_named(&clean, Utc::now())
            .map_err(failed)?;
        status(store).map_err(failed)
    })?;
    with_folder(&app, &mut status);
    pages::sync_now(&app);
    Ok(status)
}

/// Most other names the user can give.
const MAX_OWN_NAMES: usize = 10;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionItem {
    pub id: i64,
    pub written_at: i64,
    pub by_lax: bool,
    pub block: String,
    /// The facts this version would restore; empty if none.
    pub facts: Vec<String>,
}

/// How many versions of a page are shown.
const HISTORY: usize = 30;

/// The versions of an entity's page, newest first.
#[tauri::command(async)]
pub fn page_history(app: AppHandle, id: i64) -> Result<Vec<VersionItem>, String> {
    command(&app, |store| {
        let failed = |_| "Lax couldn't read the page's history.".to_string();
        let Some(page) = store.page_of(id).map_err(failed)? else {
            return Ok(Vec::new());
        };
        Ok(store
            .page_history(&page.id, HISTORY)
            .map_err(failed)?
            .into_iter()
            .map(|v| VersionItem {
                facts: if v.by_lax {
                    crate::store::facts_in(&v.block)
                } else {
                    Vec::new()
                },
                id: v.id,
                written_at: v.written_at.timestamp_millis(),
                by_lax: v.by_lax,
                block: v.block,
            })
            .collect())
    })
}

/// Puts back the facts of an earlier version of Lax's; the page is written
/// at once, as a new version.
#[tauri::command(async)]
pub fn restore_page_version(app: AppHandle, version: i64) -> Result<(), String> {
    command(&app, |store| {
        store
            .restore_facts(version, Utc::now())
            .map_err(|_| "Lax couldn't restore it.".to_string())?
            .map(|_| ())
            .ok_or_else(|| "That version is no longer there.".to_string())
    })?;
    pages::sync_now(&app);
    Ok(())
}

/// Writes `about-me.md` again after it went missing or the user removed
/// Lax's part.
#[tauri::command(async)]
pub fn recreate_about_me(app: AppHandle) -> Result<(), String> {
    command(&app, |store| {
        let failed = |_| "Lax couldn't recreate the page.".to_string();
        let page = store
            .about_me_page()
            .map_err(failed)?
            .ok_or("There is no about-me page yet.")?;
        store
            .set_page_state(&page.id, PageState::Recreate, None)
            .map_err(failed)
    })?;
    pages::sync_now(&app);
    Ok(())
}

/// Writes a missing page again, or a new one for a page whose markers the
/// user removed (that file is left as it is).
#[tauri::command(async)]
pub fn recreate_page(app: AppHandle, id: i64) -> Result<(), String> {
    command(&app, |store| {
        let failed = |_| "Lax couldn't recreate the page.".to_string();
        let page = store
            .page_of(id)
            .map_err(failed)?
            .ok_or("It has no page.")?;
        store
            .set_page_state(&page.id, PageState::Recreate, None)
            .map_err(failed)
    })?;
    pages::sync_now(&app);
    Ok(())
}

/// Opens the memory folder in Finder.
#[tauri::command(async)]
pub fn open_memory_folder(app: AppHandle) -> Result<(), String> {
    let folder = pages::folder_of(&app).ok_or("Lax couldn't find the memory folder.")?;
    if !folder.is_dir() {
        return Err("There are no pages yet.".to_string());
    }
    std::process::Command::new("/usr/bin/open")
        .arg(&folder)
        .status()
        .ok()
        .filter(|s| s.success())
        .map(|_| ())
        .ok_or_else(|| "Lax couldn't open the folder.".to_string())
}

#[tauri::command(async)]
pub fn rename_entity(app: AppHandle, id: i64, name: String) -> Result<(), String> {
    let name = valid_name(&name)?;
    command(&app, |store| {
        match store
            .rename_entity(id, &name)
            .map_err(|_| "Lax couldn't rename it.".to_string())?
        {
            Renamed::Done => Ok(()),
            Renamed::NotFound => Err("It's no longer in the memory.".to_string()),
            Renamed::Taken { by } => Err(format!(
                "{by} already goes by that name. To make them one, merge them."
            )),
            Renamed::Empty => Err("Type a name.".to_string()),
        }
    })?;
    pages::sync_now(&app);
    Ok(())
}

/// Merges `from` into `into`, which keeps its name.
#[tauri::command(async)]
pub fn merge_entities(app: AppHandle, from: i64, into: i64) -> Result<(), String> {
    command(&app, |store| {
        match store
            .merge_entities(from, into)
            .map_err(|_| "Lax couldn't merge them.".to_string())?
        {
            Merged::Done => Ok(()),
            Merged::NotFound => Err("One of them is no longer in the memory.".to_string()),
            Merged::DifferentKinds => {
                Err("Only two people, two organisations or two projects can be merged.".to_string())
            }
        }
    })?;
    pages::sync_now(&app);
    Ok(())
}

#[tauri::command(async)]
pub fn dont_learn_list(app: AppHandle) -> Result<Vec<DontLearnItem>, String> {
    command(&app, |store| {
        store
            .dont_learn()
            .map(|list| {
                list.into_iter()
                    .map(|d| DontLearnItem {
                        kind: d.kind.as_str(),
                        name: d.name,
                    })
                    .collect()
            })
            .map_err(|_| "Lax couldn't read the list.".to_string())
    })
}

/// Takes a name off the "don't learn" list.
#[tauri::command(async)]
pub fn allow_learning(app: AppHandle, kind: String, name: String) -> Result<bool, String> {
    let kind = EntityKind::parse(&kind).ok_or("Unknown kind.")?;
    command(&app, |store| {
        store
            .allow_learning(kind, &name)
            .map_err(|_| "Lax couldn't change the list.".to_string())
    })
}

/// A name the user typed, with whitespace collapsed; refused if empty, too
/// long, or with control or invisible characters.
fn valid_name(name: &str) -> Result<String, String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err("Type a name.".to_string());
    }
    if name.chars().count() > structured::MAX_NAME_CHARS {
        return Err(format!(
            "A name can be at most {} characters.",
            structured::MAX_NAME_CHARS
        ));
    }
    if name
        .chars()
        .any(|c| c.is_control() || structured::is_invisible(c))
    {
        return Err("The name has characters that can't be shown.".to_string());
    }
    Ok(name)
}

fn command<T>(
    app: &AppHandle,
    work: impl FnOnce(&mut Store) -> Result<T, String>,
) -> Result<T, String> {
    with_store(app, work).unwrap_or_else(|| Err("The database isn't available.".to_string()))
}

fn with_store<T>(app: &AppHandle, work: impl FnOnce(&mut Store) -> T) -> Option<T> {
    let store = app.try_state::<Mutex<Store>>()?;
    let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
    Some(work(&mut store))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entities(people: &[&str], organisations: &[&str], projects: &[&str]) -> Entities {
        let list = |names: &[&str]| names.iter().map(|n| n.to_string()).collect();
        Entities {
            people: list(people),
            organisations: list(organisations),
            projects: list(projects),
        }
    }

    #[test]
    fn names_keep_their_kind() {
        let found = names_in(
            entities(&["Anna Example"], &["Exempelbolaget AB"], &["Q3 lansering"]),
            &[],
            &[],
        );
        assert_eq!(
            found,
            [
                (EntityKind::Person, "Anna Example".to_string()),
                (EntityKind::Organisation, "Exempelbolaget AB".to_string()),
                (EntityKind::Project, "Q3 lansering".to_string()),
            ]
        );
    }

    #[test]
    fn the_episodes_own_apps_are_not_entities() {
        let found = names_in(
            entities(
                &["Anna Example"],
                &["slack", "Exempelbolaget AB"],
                &["Mail"],
            ),
            &["Slack".to_string(), "Mail".to_string()],
            &[],
        );
        assert_eq!(
            found,
            [
                (EntityKind::Person, "Anna Example".to_string()),
                (EntityKind::Organisation, "Exempelbolaget AB".to_string()),
            ]
        );
    }

    #[test]
    fn the_user_is_not_a_person_in_their_own_memory() {
        let own = names_of("  Bo   Exempel ");
        assert_eq!(own, ["Bo", "Bo Exempel"]);
        let found = names_in(
            entities(&["bo exempel", "Bo", "Anna Example"], &[], &["Bo"]),
            &[],
            &own,
        );
        assert_eq!(
            found,
            [
                (EntityKind::Person, "Anna Example".to_string()),
                // Only people: a project may share the name.
                (EntityKind::Project, "Bo".to_string()),
            ]
        );
        assert_eq!(names_of("Cher"), ["Cher"]);
        assert!(names_of("").is_empty());
    }

    #[test]
    fn typed_names_are_tidied_and_checked() {
        assert_eq!(valid_name("  Anna   Example ").unwrap(), "Anna Example");
        assert!(valid_name("   ").is_err());
        assert!(valid_name(&"a".repeat(101)).is_err());
        assert!(valid_name(&"å".repeat(100)).is_ok());
        assert!(valid_name("Anna\u{202E}elpmaxE").is_err());
    }

    #[test]
    fn an_episode_is_left_after_three_bad_answers() {
        let mut setbacks = Setbacks::default();
        assert!(!setbacks.bad_answer(7));
        assert!(!setbacks.bad_answer(7));
        assert!(setbacks.bad_answer(7));
        // Counting starts over, and a good answer clears the count.
        assert!(!setbacks.bad_answer(7));
        setbacks.done(7);
        assert!(setbacks.failures.is_empty());
    }

    /// Against a real model server, like `journal::tests::live_journal_episode`.
    #[tokio::test]
    #[ignore = "needs a local model server: LAX_LIVE_MODEL_URL, LAX_LIVE_CHAT_MODEL"]
    async fn live_memory_recognition() {
        let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let work = Work {
            base_url: env("LAX_LIVE_MODEL_URL"),
            model: env("LAX_LIVE_CHAT_MODEL"),
            episode: UnrecognizedEpisode {
                id: 1,
                summary: "Läste Anna Examples mejl om offerten för Q3 lansering från \
                          Exempelbolaget AB och svarade i Slack."
                    .to_string(),
                app_names: vec!["Mail".to_string(), "Slack".to_string()],
            },
            own_names: Vec::new(),
        };
        let answer = recognize(&work).await.expect("an answer");
        let names = names_in(answer, &work.episode.app_names, &[]);
        assert!(
            names
                .iter()
                .any(|(kind, name)| *kind == EntityKind::Person && name.contains("Anna"))
        );
    }
}
