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

use crate::logging::{self, ErrorKind, Field};
use crate::models::structured::{self, Captured, Entities, StructuredError};
use crate::models::{Endpoint, ModelClient};
use crate::pause::Pause;
use crate::store::{EntityKind, ModelTask, Store, UnrecognizedEpisode, name_key};

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
}

/// One round: read up to `EPISODES_PER_ROUND` new episodes. `Err` means the
/// model server is unavailable and the loop should wait a while.
pub async fn round(app: &AppHandle, setbacks: &mut Setbacks) -> Result<(), ()> {
    for _ in 0..EPISODES_PER_ROUND {
        if app.state::<Pause>().is_paused() {
            return Ok(());
        }
        let Some(work) = next_work(app) else {
            return Ok(());
        };
        let started = std::time::Instant::now();
        let Some(answer) = app.state::<Pause>().unless_paused(recognize(&work)).await else {
            return Ok(());
        };
        if app.state::<Pause>().is_paused() {
            return Ok(());
        }
        match answer {
            Ok(entities) => {
                setbacks.done(work.episode.id);
                let names = names_in(entities, &work.episode.app_names, &own_names());
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
        Ok(Some(Work {
            base_url: choice.base_url,
            model: choice.model,
            episode,
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
}

fn status(store: &Store) -> crate::store::Result<MemoryStatus> {
    Ok(MemoryStatus {
        enabled: store.memory_enabled()?,
        model: store.model_choice(ModelTask::Chat)?.map(|c| c.model),
        waiting: store.unrecognized_count()?,
        journal: store.journal_enabled()?,
    })
}

#[tauri::command(async)]
pub fn memory_status(app: AppHandle) -> Result<MemoryStatus, String> {
    command(&app, |store| {
        status(store).map_err(|_| "Lax couldn't read the memory.".to_string())
    })
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
}

#[tauri::command(async)]
pub fn memory_entities(app: AppHandle) -> Result<Vec<EntityItem>, String> {
    command(&app, |store| {
        store
            .entities()
            .map(|entities| {
                entities
                    .into_iter()
                    .map(|e| EntityItem {
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
    command(&app, |store| {
        store
            .forget_memory()
            .map_err(|_| "Lax couldn't forget what the memory noted.".to_string())
    })
}

/// Forgets one entity: its names and mentions. The episodes stay, and a
/// later episode that names it again starts it anew.
#[tauri::command(async)]
pub fn forget_entity(app: AppHandle, id: i64) -> Result<bool, String> {
    command(&app, |store| {
        store
            .forget_entity(id)
            .map_err(|_| "Lax couldn't forget it.".to_string())
    })
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
