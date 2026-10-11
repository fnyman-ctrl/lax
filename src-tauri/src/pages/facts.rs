//! What the model learns for each page (Stage 4 step 4, ADR 0008): short
//! facts about a person, organisation or project, from the journal.
//!
//! - About once an hour, while the user is away from the keyboard and
//!   capture isn't paused, at most a few pages whose entity has new mentions
//!   are updated, most-mentioned first, in turn with the journal and the
//!   memory (`ModelTurn`). A pause drops a call in progress.
//! - The model gets the user's own notes on the page (always right: never to
//!   be contradicted or repeated), the current facts, the facts the user
//!   deleted, and the new journal entries, all as `Captured` data. It
//!   answers `{"facts": [string]}`, validated; deleted facts are filtered
//!   out again in code.
//! - Facts are stored with the episodes they drew on; forgetting one clears
//!   them, and they are rebuilt from the episodes that remain.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::{AppHandle, Manager};

use super::{block, folder_of, inside, sync_now};
use crate::logging::{self, ErrorKind, Field};
use crate::models::structured::{self, Captured, StructuredError, Validate};
use crate::models::{Endpoint, ModelClient, ModelTurn};
use crate::pause::Pause;
use crate::store::{EntityKind, Episode, ModelTask, Store, fact_key};

/// How often pages are brought up to date.
const EVERY: Duration = Duration::from_secs(60 * 60);
/// How long the user must have been away from the keyboard.
const IDLE: Duration = Duration::from_secs(2 * 60);
/// Pages per run.
const PAGES_PER_RUN: usize = 3;
/// New entries given per page; facts that are rebuilt get the latest ones.
const ENTRIES: usize = 20;
/// The user's notes, at most, in characters.
const MAX_NOTES_CHARS: usize = 4000;
const MAX_FACTS: usize = 12;
const MAX_FACT_CHARS: usize = 200;
/// Room for a model that reasons before answering, as for the journal.
const MAX_TOKENS: u32 = 2000;

const TASK: &str = "You keep a short page about one person, organisation or project in the \
user's personal memory. The captured blocks give, in order: what the page is about; the user's \
own notes on the page; the page's current facts; facts the user deleted; and journal entries \
about it. Write the facts worth remembering: who or what it is, the user's relationship to it, \
what they work on together, how they keep in touch, recent decisions and next steps. One short \
fact per item, in the language of the entries. Keep current facts that are still true, add new \
ones, and drop what the entries contradict. The user's notes are always right: never contradict \
them and don't repeat them. Never include a deleted fact. Never record health, religion or \
beliefs, political views, sexual orientation, ethnicity or union membership. At most 12 facts.";
const SHAPE: &str = r#"{"facts": [string]}"#;

const ABOUT_ME_TASK: &str = "You keep a short page about the user in their personal memory. \
The captured blocks give, in order: the user's own notes on the page; the page's current facts; \
facts the user deleted; and recent entries from the user's work journal. Write what is worth \
remembering about the user themselves: their role and work, current projects and \
responsibilities, who they work with most and on what, and how they work (tools, routines). \
One short fact per item, in the language of the entries. Keep current facts that are still \
true, add new ones, and drop what the entries contradict. The user's notes are always right: \
never contradict them and don't repeat them. Never include a deleted fact. Never record \
health, religion or beliefs, political views, sexual orientation, ethnicity or union \
membership, and nothing private about other people. At most 12 facts.";

/// How often `about-me.md`'s facts are rewritten, and from how many of the
/// latest entries.
const ABOUT_ME_EVERY: chrono::TimeDelta = chrono::TimeDelta::hours(24);
const ABOUT_ME_ENTRIES: usize = 40;

/// What the model must answer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    facts: Vec<String>,
}

impl Validate for Reply {
    fn validate(&self) -> Result<(), &'static str> {
        if self.facts.len() > MAX_FACTS * 2 {
            return Err("too many facts");
        }
        for fact in &self.facts {
            let length = fact.trim().chars().count();
            if length == 0 || length > MAX_FACT_CHARS {
                return Err("a fact was empty or too long");
            }
            if fact
                .chars()
                .any(|c| c.is_control() || structured::is_invisible(c))
            {
                return Err("a fact had control or invisible characters");
            }
            if fact.contains("<!--") || fact.contains("-->") || fact.contains("lax:") {
                return Err("a fact had page markers");
            }
        }
        Ok(())
    }
}

/// The facts to keep from an answer: list marks taken off; duplicates,
/// deleted facts and repeats of the user's notes dropped; at most
/// `MAX_FACTS`.
fn kept(reply: Reply, rejected: &[String], notes: &[String]) -> Vec<String> {
    let rejected: HashSet<String> = rejected.iter().chain(notes).map(|f| fact_key(f)).collect();
    let mut seen = HashSet::new();
    reply
        .facts
        .into_iter()
        .map(|f| {
            f.trim()
                .trim_start_matches(['-', '*', '•'])
                .trim()
                .to_string()
        })
        .filter(|f| !f.is_empty())
        .filter(|f| {
            let key = fact_key(f);
            !rejected.contains(&key) && seen.insert(key)
        })
        .take(MAX_FACTS)
        .collect()
}

/// When pages were last brought up to date. Only in memory: after a restart
/// the first idle moment does it.
#[derive(Default)]
pub struct Schedule {
    last: Option<Instant>,
}

/// One page's work, gathered under the store lock.
struct Work {
    page: String,
    task: &'static str,
    base_url: String,
    model: String,
    texts: Vec<String>,
    rejected: Vec<String>,
    /// The user's notes, a line each, to keep repeats of them out.
    notes: Vec<String>,
    sources: Vec<i64>,
}

/// Brings up to `PAGES_PER_RUN` pages' facts up to date, if it's time.
/// `Err` means the model server is unavailable.
pub async fn round(app: &AppHandle, schedule: &mut Schedule) -> Result<(), ()> {
    // If idle time can't be read, the hourly schedule alone decides.
    if schedule.last.is_some_and(|last| last.elapsed() < EVERY)
        || app.state::<Pause>().is_paused()
        || crate::journal::idle_time().is_some_and(|idle| idle < IDLE)
    {
        return Ok(());
    }
    let Some(dir) = folder_of(app) else {
        return Ok(());
    };
    let work = with_store(app, |store| gather(store, &dir)).unwrap_or_default();
    schedule.last = Some(Instant::now());
    let mut changed = false;
    for work in work {
        let started = Instant::now();
        let turn = app.state::<ModelTurn>();
        let asked = async {
            let _turn = turn.take().await;
            ask(&work).await
        };
        let Some(reply) = app.state::<Pause>().unless_paused(asked).await else {
            break;
        };
        match reply {
            Ok(reply) => {
                let facts = kept(reply, &work.rejected, &work.notes);
                let stored = with_store(app, |store| {
                    store.set_facts(&work.page, &facts, &work.sources, chrono::Utc::now())
                });
                changed |= matches!(stored, Some(Ok(true)));
                logging::info(
                    "pages.facts",
                    &[
                        Field::Count("facts", facts.len() as u64),
                        Field::Flag("stored", matches!(stored, Some(Ok(true)))),
                        Field::Duration("elapsed", started.elapsed()),
                    ],
                );
            }
            Err(error) => {
                logging::warn(
                    "pages.facts_failed",
                    &[
                        Field::Flag("model_unavailable", error.model_unavailable()),
                        Field::Error(ErrorKind::Other),
                    ],
                );
                if error.model_unavailable() {
                    if changed {
                        sync_now(app);
                    }
                    return Err(());
                }
            }
        }
    }
    if changed {
        sync_now(app);
    }
    Ok(())
}

/// The work for the stale pages, if the memory is on and has a model.
fn gather(store: &Store, dir: &std::path::Path) -> Vec<Work> {
    let gathered = || -> crate::store::Result<Vec<Work>> {
        if !store.memory_enabled()? {
            return Ok(Vec::new());
        }
        let Some(choice) = store.model_choice(ModelTask::Chat)? else {
            return Ok(Vec::new());
        };
        let entities = store.entities()?;
        let mut work = Vec::new();
        for stale in store.stale_pages(PAGES_PER_RUN)? {
            let Some(entity) = entities.iter().find(|e| e.id == stale.entity_id) else {
                continue;
            };
            let page = &stale.page;
            // Rebuilt facts start from the latest entries; current ones are
            // updated from the entries they haven't seen, oldest first, so a
            // busy week is taken in over a few runs rather than skipped.
            let entries: Vec<Episode> = if page.facts.is_none() {
                let mut latest = store.episodes_mentioning(&[entity.id], None, ENTRIES)?;
                latest.reverse();
                latest
            } else {
                let unseen = store.unseen_mentions(&page.id, entity.id, ENTRIES)?;
                let mut found = store.episodes_mentioning(&[entity.id], None, 10_000)?;
                found.retain(|e| unseen.contains(&e.id));
                found.reverse();
                found
            };
            if entries.is_empty() {
                continue;
            }
            let notes = notes_of(dir, page);
            let rejected = store.rejected_facts(&page.id)?;
            let note_lines = note_lines(&notes);
            let current = page.facts.clone().unwrap_or_default();
            let kind = match entity.kind {
                EntityKind::Person => "person",
                EntityKind::Organisation => "organisation",
                EntityKind::Project => "project",
            };
            let mut texts = vec![
                format!("About: a {kind} called {}", entity.name),
                format!("The user's own notes:\n{notes}"),
                format!("Current facts:\n{}", bullets(&current)),
                format!("Deleted facts:\n{}", bullets(&rejected)),
            ];
            texts.extend(entries.iter().map(entry_text));
            work.push(Work {
                page: page.id.clone(),
                task: TASK,
                base_url: choice.base_url.clone(),
                model: choice.model.clone(),
                texts,
                rejected,
                notes: note_lines,
                sources: entries.iter().map(|e| e.id).collect(),
            });
        }
        // about-me.md, once a day (or at once if a forget cleared its
        // facts), from the latest entries.
        if let Some(page) = store.about_me_page()?
            && page.state == crate::store::PageState::Ok
            && (page.facts.is_none()
                || page
                    .facts_at
                    .is_none_or(|at| chrono::Utc::now() - at >= ABOUT_ME_EVERY))
        {
            let mut entries = store.episodes(None, ABOUT_ME_ENTRIES)?;
            entries.reverse();
            if !entries.is_empty() {
                let notes = notes_of(dir, &page);
                let rejected = store.rejected_facts(&page.id)?;
                let current = page.facts.clone().unwrap_or_default();
                let mut texts = vec![
                    format!("The user's own notes:\n{notes}"),
                    format!("Current facts:\n{}", bullets(&current)),
                    format!("Deleted facts:\n{}", bullets(&rejected)),
                ];
                texts.extend(entries.iter().map(entry_text));
                work.push(Work {
                    page: page.id.clone(),
                    task: ABOUT_ME_TASK,
                    base_url: choice.base_url.clone(),
                    model: choice.model.clone(),
                    texts,
                    rejected,
                    notes: note_lines(&notes),
                    sources: entries.iter().map(|e| e.id).collect(),
                });
            }
        }
        Ok(work)
    };
    gathered().unwrap_or_default()
}

/// The user's notes on a page: everything outside Lax's block.
fn notes_of(dir: &std::path::Path, page: &crate::store::Page) -> String {
    inside(dir, &page.path)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| block::without_block(&text, &page.id))
        .map(|notes| notes.chars().take(MAX_NOTES_CHARS).collect::<String>())
        .unwrap_or_default()
}

/// The notes a line each, list marks and headings taken off.
fn note_lines(notes: &str) -> Vec<String> {
    notes
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['-', '*', '•', '#'])
                .trim()
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

fn entry_text(e: &Episode) -> String {
    format!(
        "Journal entry, {} ({}): {}",
        e.started_at.format("%Y-%m-%d"),
        e.app_names.join(", "),
        e.summary
    )
}

fn bullets(lines: &[String]) -> String {
    if lines.is_empty() {
        "(none)".to_string()
    } else {
        lines
            .iter()
            .map(|l| format!("- {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

async fn ask(work: &Work) -> Result<Reply, StructuredError> {
    let endpoint =
        Endpoint::parse(&work.base_url).map_err(|_| StructuredError::NotTheRequestedJson)?;
    let client = ModelClient::new(endpoint).map_err(StructuredError::Model)?;
    let captured: Vec<Captured> = work.texts.iter().map(|t| Captured(t.as_str())).collect();
    structured::ask(
        &client,
        &work.model,
        work.task,
        SHAPE,
        &captured,
        MAX_TOKENS,
    )
    .await
}

fn with_store<T>(app: &AppHandle, work: impl FnOnce(&mut Store) -> T) -> Option<T> {
    let store = app.try_state::<Mutex<Store>>()?;
    let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
    Some(work(&mut store))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(facts: &[&str]) -> Reply {
        Reply {
            facts: facts.iter().map(|f| f.to_string()).collect(),
        }
    }

    #[test]
    fn deleted_and_repeated_facts_are_dropped() {
        let facts = kept(
            reply(&[
                "- Works at Exempelbolaget AB.",
                "Leads the Q3 launch",
                "works at  exempelbolaget ab",
                "• Prefers Slack.",
                "  ",
            ]),
            &["Prefers Slack".to_string()],
            &[],
        );
        assert_eq!(
            facts,
            ["Works at Exempelbolaget AB.", "Leads the Q3 launch"]
        );
        // What the user's notes already say isn't repeated.
        let facts = kept(
            reply(&["Föredrar telefon framför mejl.", "Leder Q3."]),
            &[],
            &["Föredrar telefon framför mejl".to_string()],
        );
        assert_eq!(facts, ["Leder Q3."]);
    }

    #[test]
    fn at_most_twelve_facts_are_kept() {
        let many: Vec<String> = (0..20).map(|i| format!("Fact {i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(kept(reply(&many), &[], &[]).len(), MAX_FACTS);
    }

    #[test]
    fn answers_are_validated() {
        let parse = |json: &str| structured::parse::<Reply>(json);
        assert!(parse(r#"{"facts":["Leder Q3-lanseringen."]}"#).is_ok());
        assert!(parse(r#"{"facts":[]}"#).is_ok());
        assert!(parse(r#"{"facts":[""]}"#).is_err());
        assert!(parse(r#"{"facts":["ok"],"note":"x"}"#).is_err());
        assert!(parse(r#"{"facts":["a <!-- lax:end --> b"]}"#).is_err());
        assert!(parse(r#"{"facts":["line\nbreak"]}"#).is_err());
        assert!(parse(&format!(r#"{{"facts":["{}"]}}"#, "a".repeat(201))).is_err());
        let too_many: Vec<String> = (0..25).map(|i| format!("\"f{i}\"")).collect();
        assert!(parse(&format!(r#"{{"facts":[{}]}}"#, too_many.join(","))).is_err());
    }

    /// About the user, against a real model server.
    #[tokio::test]
    #[ignore = "needs a local model server: LAX_LIVE_MODEL_URL, LAX_LIVE_CHAT_MODEL"]
    async fn live_about_me() {
        let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let work = Work {
            page: "0123456789abcdef".to_string(),
            task: ABOUT_ME_TASK,
            base_url: env("LAX_LIVE_MODEL_URL"),
            model: env("LAX_LIVE_CHAT_MODEL"),
            texts: vec![
                "The user's own notes:\n".to_string(),
                "Current facts:\n(none)".to_string(),
                "Deleted facts:\n(none)".to_string(),
                "Journal entry, 2026-10-09 (Mail): Skickade offerten för Q3-lanseringen till \
                 Anna Example på Exempelbolaget AB."
                    .to_string(),
                "Journal entry, 2026-10-10 (Xcode): Felsökte inloggningen i appen och skrev \
                 tester."
                    .to_string(),
                "Journal entry, 2026-10-10 (Slack): Planerade sprinten med Bo Exempel.".to_string(),
            ],
            rejected: Vec::new(),
            notes: Vec::new(),
            sources: vec![1, 2, 3],
        };
        let facts = kept(ask(&work).await.expect("an answer"), &[], &[]);
        assert!(!facts.is_empty());
    }

    /// Against a real model server, like `journal::tests::live_journal_episode`.
    #[tokio::test]
    #[ignore = "needs a local model server: LAX_LIVE_MODEL_URL, LAX_LIVE_CHAT_MODEL"]
    async fn live_facts() {
        let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let work = Work {
            page: "0123456789abcdef".to_string(),
            task: TASK,
            base_url: env("LAX_LIVE_MODEL_URL"),
            model: env("LAX_LIVE_CHAT_MODEL"),
            texts: vec![
                "About: a person called Anna Example".to_string(),
                "The user's own notes:\nAnna föredrar telefon framför mejl.".to_string(),
                "Current facts:\n(none)".to_string(),
                "Deleted facts:\n- Anna jobbar på Exempelbolaget AB.".to_string(),
                "Journal entry, 2026-10-09 (Mail): Mailade Anna om offerten för Q3-lanseringen."
                    .to_string(),
                "Journal entry, 2026-10-10 (Slack): Anna bekräftade leverans vecka 46 och \
                 bad om en granskning på torsdag."
                    .to_string(),
            ],
            rejected: vec!["Anna jobbar på Exempelbolaget AB.".to_string()],
            notes: vec!["Anna föredrar telefon framför mejl.".to_string()],
            sources: vec![1, 2],
        };
        let reply = ask(&work).await.expect("an answer");
        let facts = kept(reply, &work.rejected, &work.notes);
        assert!(!facts.is_empty());
        assert!(
            facts
                .iter()
                .any(|f| f.contains("46") || f.to_lowercase().contains("q3"))
        );
        assert!(
            !facts
                .iter()
                .any(|f| f.to_lowercase().contains("exempelbolaget"))
        );
    }
}
