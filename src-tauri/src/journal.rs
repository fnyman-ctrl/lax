//! The journal (Stage 3): every few minutes, the chosen chat model turns
//! recent captures into short episodes, the readable timeline of a day.
//!
//! - Off until the user turns it on, and only with a chat model chosen.
//!   Only models on this Mac can be chosen for now (GUARDRAILS §3).
//! - While paused it does nothing, and a pause during a run drops what the
//!   model wrote (GUARDRAILS §4: pausing stops pending processing).
//! - Captured text reaches the model only as `Captured` data, and the answer
//!   is schema-validated before it is stored (GUARDRAILS §5).
//! - An episode about captures the user forgot meanwhile is refused by the
//!   store, so nothing derived from forgotten captures is kept.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Local, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::logging::{self, ErrorKind, Field};
use crate::memory;
use crate::models::structured::{self, Captured, StructuredError, Validate};
use crate::models::{Endpoint, ModelClient, ModelError, ModelHealth, ModelTurn};
use crate::pause::Pause;
use crate::store::{Capture, ModelTask, NewEpisode, Store};

/// How often the journal looks for captures to summarize.
const ROUND: Duration = Duration::from_secs(60);
/// After a failed summary, wait this long before trying again.
const AFTER_FAILURE: Duration = Duration::from_secs(10 * 60);
/// An episode covers at most this much time.
const BLOCK: TimeDelta = TimeDelta::minutes(15);
/// Captures younger than this are left for the next round: the window may
/// still be changing.
const SETTLE: TimeDelta = TimeDelta::minutes(2);
/// With no keyboard or mouse input for this long, a block that hasn't run
/// its full length is summarized anyway.
const IDLE_FLUSH: Duration = Duration::from_secs(5 * 60);
/// Most blocks summarized in one round, so a backlog doesn't hog the Mac.
const BLOCKS_PER_ROUND: usize = 3;
/// A block the model answered badly this many times is set aside until Lax
/// restarts, so one hard block can't stop the journal.
const ATTEMPTS_PER_BLOCK: u32 = 3;
/// Most captures read per block, and characters per capture and per block.
const MAX_CAPTURES: usize = 200;
const MAX_CHARS_PER_CAPTURE: usize = 1500;
const MAX_CHARS_PER_BLOCK: usize = 12_000;
/// Room for the answer. Models like Gemma 4 reason before answering, and with
/// a full block that can take well over a thousand tokens; too little room
/// leaves no answer at all (checked on a real Mac). Kept well inside the
/// client's chat timeout at local speeds (about 25 tokens a second).
const MAX_TOKENS: u32 = 2000;

const TASK: &str = "Write a short work-journal entry, one to three sentences, about what the \
user did in these windows: what they worked on, in which apps, and with whom. Write in the \
language most of the captured text is in. Write notes in the past tense without \"I\", for \
example: \"Read Anna's mail about the Q3 offer and updated the roadmap.\" Only use what is in \
the captured text; don't guess.";
const SHAPE: &str = r#"{"summary": string}"#;

/// What the model must answer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EpisodeSummary {
    summary: String,
}

const MAX_SUMMARY_CHARS: usize = 600;

impl Validate for EpisodeSummary {
    fn validate(&self) -> Result<(), &'static str> {
        let length = self.summary.trim().chars().count();
        if length == 0 || length > MAX_SUMMARY_CHARS {
            return Err("the summary was empty or too long");
        }
        if self
            .summary
            .chars()
            .any(|c| (c.is_control() && c != '\n') || structured::is_invisible(c))
        {
            return Err("the summary had control or invisible characters");
        }
        Ok(())
    }
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut setbacks = Setbacks::default();
        let mut memory_setbacks = memory::Setbacks::default();
        let mut merge_setbacks = crate::merges::Setbacks::default();
        let mut facts_schedule = crate::pages::facts::Schedule::default();
        loop {
            // The memory reads the journal's episodes, right after it, so the
            // two never ask the model for two things at once (ADR 0008).
            let mut result = round(&app, &mut setbacks).await;
            if result.is_ok() {
                result = memory::round(&app, &mut memory_setbacks).await;
            }
            if result.is_ok() {
                result = crate::merges::round(&app, &mut merge_setbacks).await;
            }
            // No model: brings the memory's pages up to date.
            crate::pages::round(&app);
            if result.is_ok() {
                result = crate::pages::facts::round(&app, &mut facts_schedule).await;
            }
            if result.is_ok() {
                result = crate::vectors::round(&app).await;
            }
            let wait = match result {
                Ok(()) => ROUND,
                Err(()) => AFTER_FAILURE,
            };
            tokio::time::sleep(wait).await;
        }
    });
}

/// Blocks the model has answered badly, by their first capture, and the
/// captures set aside after too many tries. Only in memory.
#[derive(Default)]
struct Setbacks {
    failures: HashMap<i64, u32>,
    set_aside: HashSet<i64>,
}

impl Setbacks {
    /// Counts a bad answer for the block; after `ATTEMPTS_PER_BLOCK` its
    /// captures are set aside.
    fn bad_answer(&mut self, block: &[i64]) {
        let Some(first) = block.first() else { return };
        let count = self.failures.entry(*first).or_insert(0);
        *count += 1;
        if *count >= ATTEMPTS_PER_BLOCK {
            self.failures.remove(first);
            self.set_aside.extend(block);
            logging::warn(
                "journal.block_set_aside",
                &[Field::Count("captures", block.len() as u64)],
            );
        }
    }
}

/// One round: summarize up to `BLOCKS_PER_ROUND` complete blocks.
async fn round(app: &AppHandle, setbacks: &mut Setbacks) -> Result<(), ()> {
    for _ in 0..BLOCKS_PER_ROUND {
        if app.state::<Pause>().is_paused() || !may_use_model(When::Soon) {
            return Ok(());
        }
        let Some(work) = next_work(app, &setbacks.set_aside) else {
            return Ok(());
        };
        let started = std::time::Instant::now();
        // Paused while the model was writing: the request is dropped, and
        // anything it wrote anyway is not stored.
        let Some(summary) = app
            .state::<ModelTurn>()
            .background(&app.state::<Pause>(), summarize(&work))
            .await
        else {
            return Ok(());
        };
        if app.state::<Pause>().is_paused() {
            return Ok(());
        }
        match summary {
            Ok(summary) => {
                app.state::<ModelHealth>().up();
                let stored = with_store(app, |store| {
                    store.record_episode(
                        &NewEpisode {
                            summary: summary.trim().to_string(),
                            model: work.model.clone(),
                            created_at: Utc::now(),
                        },
                        &work.capture_ids,
                    )
                });
                logging::info(
                    "journal.episode",
                    &[
                        Field::Count("captures", work.capture_ids.len() as u64),
                        Field::Flag("stored", matches!(stored, Some(Ok(Some(_))))),
                        Field::Duration("elapsed", started.elapsed()),
                    ],
                );
            }
            Err(error) => {
                // A server that's down or busy isn't the block's fault: wait
                // a while. Anything else (a malformed or invalid answer, or
                // running out of room) may be, so the block is counted and
                // the journal goes on.
                let unavailable = error.model_unavailable();
                logging::warn(
                    "journal.failed",
                    &[
                        Field::Flag("model_unavailable", unavailable),
                        Field::Flag(
                            "out_of_room",
                            matches!(error, StructuredError::Model(ModelError::OutOfRoom)),
                        ),
                        Field::Error(ErrorKind::Other),
                    ],
                );
                if unavailable {
                    app.state::<ModelHealth>()
                        .down(Utc::now().timestamp_millis());
                    return Err(());
                }
                // It answered, if badly.
                app.state::<ModelHealth>().up();
                setbacks.bad_answer(&work.block_ids);
                // Try again next round rather than at once, so a hard block
                // doesn't keep the Mac busy with back-to-back attempts.
                return Ok(());
            }
        }
    }
    Ok(())
}

/// A block of captures ready to summarize, and the model to do it.
struct Work {
    base_url: String,
    model: String,
    /// What the model is shown, one entry per window.
    texts: Vec<String>,
    /// The captures those entries stand for; only these are marked
    /// summarized.
    capture_ids: Vec<i64>,
    /// Every capture of the block, for setting it aside.
    block_ids: Vec<i64>,
}

/// The next complete block, if the journal is on and has a model.
fn next_work(app: &AppHandle, set_aside: &HashSet<i64>) -> Option<Work> {
    let idle = idle_time();
    with_store(app, |store| -> crate::store::Result<Option<Work>> {
        if !store.journal_enabled()? {
            return Ok(None);
        }
        let Some(choice) = store.model_choice(ModelTask::Chat)? else {
            return Ok(None);
        };
        let now = Utc::now();
        let waiting: Vec<Capture> = store
            .unsummarized(now - SETTLE, MAX_CAPTURES)?
            .into_iter()
            .filter(|c| !set_aside.contains(&c.id))
            .collect();
        let Some(block) = block(&waiting, now, idle.is_some_and(|i| i >= IDLE_FLUSH)) else {
            return Ok(None);
        };
        let (texts, capture_ids) = prompt_texts(&block);
        Ok(Some(Work {
            base_url: choice.base_url,
            model: choice.model,
            texts,
            capture_ids,
            block_ids: block.iter().map(|c| c.id).collect(),
        }))
    })
    .and_then(Result::ok)
    .flatten()
}

/// The captures of the first block in `waiting` (oldest first): those within
/// `BLOCK` of the first. Only complete blocks are returned, unless `flush`
/// (the user is away) lets a shorter one through.
fn block(waiting: &[Capture], now: DateTime<Utc>, flush: bool) -> Option<Vec<Capture>> {
    let first = waiting.first()?;
    let end = first.captured_at + BLOCK;
    let complete = end <= now - SETTLE || waiting.iter().any(|c| c.captured_at >= end);
    if !complete && !flush {
        return None;
    }
    Some(
        waiting
            .iter()
            .take_while(|c| c.captured_at < end)
            .cloned()
            .collect(),
    )
}

/// The block as `Captured` data: one entry per window, its latest content,
/// within the character limits. Also the ids of the captures the entries
/// stand for; windows left out for room stay for a later block.
fn prompt_texts(captures: &[Capture]) -> (Vec<String>, Vec<i64>) {
    // Windows in order of first appearance, each with its latest capture and
    // all its captures' ids.
    let mut windows: Vec<(&Capture, Vec<i64>)> = Vec::new();
    for capture in captures {
        match windows.iter_mut().find(|(c, _)| {
            c.bundle_id.eq_ignore_ascii_case(&capture.bundle_id)
                && c.window_title == capture.window_title
        }) {
            Some((latest, ids)) => {
                *latest = capture;
                ids.push(capture.id);
            }
            None => windows.push((capture, vec![capture.id])),
        }
    }
    let mut texts = Vec::new();
    let mut included = Vec::new();
    let mut total = 0;
    for (capture, ids) in windows {
        let time = capture.captured_at.with_timezone(&Local).format("%H:%M");
        let site = capture
            .site
            .as_deref()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        let body: String = capture.text.chars().take(MAX_CHARS_PER_CAPTURE).collect();
        let text = format!(
            "{time} {} — {}{site}\n{body}",
            capture.app_name, capture.window_title
        );
        let length = text.chars().count();
        if total + length > MAX_CHARS_PER_BLOCK && !texts.is_empty() {
            break;
        }
        total += length;
        texts.push(text);
        included.extend(ids);
    }
    (texts, included)
}

async fn summarize(work: &Work) -> Result<String, StructuredError> {
    let endpoint =
        Endpoint::parse(&work.base_url).map_err(|_| StructuredError::NotTheRequestedJson)?;
    let client = ModelClient::new(endpoint).map_err(StructuredError::Model)?;
    let captured: Vec<Captured> = work.texts.iter().map(|t| Captured(t.as_str())).collect();
    let answer: EpisodeSummary =
        structured::ask(&client, &work.model, TASK, SHAPE, &captured, MAX_TOKENS).await?;
    Ok(answer.summary)
}

/// Without keyboard or mouse input for this long, work that can wait may use
/// the model.
const AWAY: Duration = Duration::from_secs(2 * 60);

/// Whether background work may use the model now. Never on battery, and
/// work that can wait (`Later`) only while the user is away from the
/// keyboard, so the Mac stays quiet and cool while it is used. Questions
/// the user asks are not background work and always may.
pub(crate) fn may_use_model(when: When) -> bool {
    allowed(when, on_battery(), idle_time())
}

/// When background work wants the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum When {
    /// The journal: soon after the captures it summarizes.
    Soon,
    /// The memory's reading, facts and vectors.
    Later,
}

fn allowed(when: When, on_battery: bool, idle: Option<Duration>) -> bool {
    if on_battery {
        return false;
    }
    // If idle time can't be read, the user counts as away.
    when == When::Soon || !idle.is_some_and(|idle| idle < AWAY)
}

fn on_battery() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::ax::on_battery().unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

pub(crate) fn idle_time() -> Option<Duration> {
    #[cfg(target_os = "macos")]
    {
        crate::ax::idle_time()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct JournalStatus {
    pub enabled: bool,
    /// The chat model that writes it, if one is chosen.
    pub model: Option<String>,
    /// Captures waiting to be summarized.
    pub waiting: usize,
    /// Since when the model's server couldn't be reached, in milliseconds
    /// since 1970, if it can't.
    pub model_down_since: Option<i64>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeItem {
    pub id: i64,
    pub started_at: i64,
    pub ended_at: i64,
    pub summary: String,
    pub app_names: Vec<String>,
    pub captures: usize,
}

fn status(store: &Store) -> crate::store::Result<JournalStatus> {
    Ok(JournalStatus {
        enabled: store.journal_enabled()?,
        model: store.model_choice(ModelTask::Chat)?.map(|c| c.model),
        waiting: store.unsummarized_count()?,
        model_down_since: None,
    })
}

fn command<T>(
    app: &AppHandle,
    work: impl FnOnce(&mut Store) -> Result<T, String>,
) -> Result<T, String> {
    with_store(app, work).unwrap_or_else(|| Err("The database isn't available.".to_string()))
}

#[tauri::command(async)]
pub fn journal_status(app: AppHandle) -> Result<JournalStatus, String> {
    let mut status = command(&app, |store| {
        status(store).map_err(|_| "Lax couldn't read the journal.".to_string())
    })?;
    status.model_down_since = app.state::<ModelHealth>().down_since();
    Ok(status)
}

/// Turns the journal on or off. It can only be turned on with a chat model
/// chosen.
#[tauri::command(async)]
pub fn set_journal_enabled(app: AppHandle, enabled: bool) -> Result<JournalStatus, String> {
    command(&app, |store| {
        let failed = |_| "Lax couldn't change the journal setting.".to_string();
        if enabled
            && store
                .model_choice(ModelTask::Chat)
                .map_err(failed)?
                .is_none()
        {
            return Err("Choose a chat model under Models first.".to_string());
        }
        store.set_journal_enabled(enabled).map_err(failed)?;
        status(store).map_err(failed)
    })
}

#[tauri::command(async)]
pub fn journal_episodes(
    app: AppHandle,
    before_at: Option<i64>,
    before_id: Option<i64>,
    limit: u32,
) -> Result<Vec<EpisodeItem>, String> {
    let before = match (before_at, before_id) {
        (Some(at), Some(id)) => Some((
            DateTime::from_timestamp_millis(at).ok_or("Invalid time")?,
            id,
        )),
        _ => None,
    };
    command(&app, |store| {
        store
            .episodes(before, limit.clamp(1, 200) as usize)
            .map(|episodes| {
                episodes
                    .into_iter()
                    .map(|e| EpisodeItem {
                        id: e.id,
                        started_at: e.started_at.timestamp_millis(),
                        ended_at: e.ended_at.timestamp_millis(),
                        summary: e.summary,
                        app_names: e.app_names,
                        captures: e.captures,
                    })
                    .collect()
            })
            .map_err(|_| "Lax couldn't read the journal.".to_string())
    })
}

fn with_store<T>(app: &AppHandle, work: impl FnOnce(&mut Store) -> T) -> Option<T> {
    let store = app.try_state::<Mutex<Store>>()?;
    let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
    Some(work(&mut store))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(id: i64, minute: i64, bundle_id: &str, title: &str, text: &str) -> Capture {
        Capture {
            id,
            captured_at: DateTime::from_timestamp_millis(1_781_520_000_000 + minute * 60_000)
                .unwrap(),
            app_name: "Mail".to_string(),
            bundle_id: bundle_id.to_string(),
            window_title: title.to_string(),
            site: None,
            text: text.to_string(),
        }
    }

    fn minute(m: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_781_520_000_000 + m * 60_000).unwrap()
    }

    #[test]
    fn only_complete_blocks_are_summarized() {
        let waiting = vec![
            capture(1, 0, "com.apple.mail", "Inbox", "a"),
            capture(2, 10, "com.apple.mail", "Inbox", "b"),
            capture(3, 20, "com.apple.mail", "Inbox", "c"),
        ];
        // At minute 12 the first block (0-15) is still open.
        assert!(block(&waiting[..2], minute(12), false).is_none());
        // A capture after the block's end closes it.
        let ids: Vec<i64> = block(&waiting, minute(21), false)
            .unwrap()
            .iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, [1, 2]);
        // Or enough time passes.
        assert!(block(&waiting[..2], minute(18), false).is_some());
        // Or the user is away.
        assert!(block(&waiting[..2], minute(12), true).is_some());
        assert!(block(&[], minute(30), true).is_none());
    }

    #[test]
    fn each_window_appears_once_with_its_latest_content() {
        let captures = vec![
            capture(1, 0, "com.apple.mail", "Re: Offert", "first draft"),
            capture(2, 1, "com.apple.Notes", "Q3", "note"),
            capture(3, 2, "com.apple.mail", "Re: Offert", "final text"),
        ];
        let (texts, ids) = prompt_texts(&captures);
        assert_eq!(texts.len(), 2);
        assert!(texts[0].contains("final text") && !texts[0].contains("first draft"));
        assert!(texts[0].contains("Mail — Re: Offert"));
        // Both captures of the mail window are summarized by its entry.
        assert_eq!(ids, [1, 3, 2]);
    }

    #[test]
    fn prompts_stay_within_the_limits() {
        let long = "x".repeat(5000);
        let captures: Vec<Capture> = (0..20)
            .map(|i| capture(i, i, "com.example.app", &format!("Doc {i}"), &long))
            .collect();
        let (texts, ids) = prompt_texts(&captures);
        let total: usize = texts.iter().map(|t| t.chars().count()).sum();
        assert!(total <= MAX_CHARS_PER_BLOCK, "{total}");
        assert!(
            texts
                .iter()
                .all(|t| t.chars().count() <= MAX_CHARS_PER_CAPTURE + 40)
        );
        // Only the windows that fit are marked summarized; the rest wait.
        assert_eq!(ids.len(), texts.len());
        assert!(ids.len() < captures.len());
    }

    #[test]
    fn a_block_answered_badly_is_set_aside_after_three_tries() {
        let mut setbacks = Setbacks::default();
        setbacks.bad_answer(&[7, 8]);
        setbacks.bad_answer(&[7, 8]);
        assert!(setbacks.set_aside.is_empty());
        setbacks.bad_answer(&[7, 8]);
        assert!(setbacks.set_aside.contains(&7) && setbacks.set_aside.contains(&8));
    }

    #[test]
    fn background_work_waits_for_mains_power_and_the_user_to_be_away() {
        let s = Duration::from_secs;
        // On mains power the journal always may; the rest once the user is away.
        assert!(allowed(When::Soon, false, Some(s(0))));
        assert!(!allowed(When::Later, false, Some(s(0))));
        assert!(!allowed(When::Later, false, Some(s(119))));
        assert!(allowed(When::Later, false, Some(s(120))));
        assert!(allowed(When::Later, false, None));
        // On battery nothing does.
        assert!(!allowed(When::Soon, true, Some(s(3600))));
        assert!(!allowed(When::Later, true, Some(s(3600))));
        assert!(!allowed(When::Later, true, None));
    }

    #[test]
    fn summaries_are_validated() {
        let ok = |s: &str| {
            EpisodeSummary {
                summary: s.to_string(),
            }
            .validate()
            .is_ok()
        };
        assert!(ok("Läste Annas mejl om offerten för Q3."));
        assert!(!ok("   "));
        assert!(!ok(&"a".repeat(601)));
        assert!(!ok("Read mail\u{202e}"));
        assert!(structured::parse::<EpisodeSummary>(r#"{"summary":"ok","next":"x"}"#).is_err());
    }

    /// Writes a real episode with a local model from synthetic captures. Not
    /// run by default; run with `LAX_LIVE_MODEL_URL=http://localhost:1234/v1
    /// LAX_LIVE_CHAT_MODEL=google/gemma-4-e4b cargo test live_journal -- --ignored`.
    #[tokio::test]
    #[ignore = "needs a model server running on this Mac"]
    async fn live_journal_episode() {
        let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let mut mail = capture(
            1,
            0,
            "com.apple.mail",
            "Re: Offert för Q3",
            "Hej Erik! Vi kan leverera skärmarna vecka 46 om ni godkänner offerten före fredag. Mvh Anna Example",
        );
        mail.app_name = "Mail".to_string();
        let mut slack = capture(
            2,
            5,
            "com.tinyspeck.slackmacgap",
            "#q3-launch",
            "Erik Exempel: Anna says week 46 works. Can we move the launch review to Thursday?",
        );
        slack.app_name = "Slack".to_string();
        let (texts, capture_ids) = prompt_texts(&[mail, slack]);
        let work = Work {
            base_url: env("LAX_LIVE_MODEL_URL"),
            model: env("LAX_LIVE_CHAT_MODEL"),
            texts,
            block_ids: capture_ids.clone(),
            capture_ids,
        };
        let summary = summarize(&work).await.unwrap();
        assert!(!summary.trim().is_empty());
        assert!(
            summary.contains("46") || summary.to_lowercase().contains("offert"),
            "{summary}"
        );
    }
}
