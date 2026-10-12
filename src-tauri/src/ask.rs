//! Ask your memory, first version (Stage 5): the user asks a question in
//! Lax, and the chat model on this Mac answers from the journal.
//!
//! - Finding what the question is about takes no model call: a time named
//!   in the question ("i tisdags", "last week"), names the memory knows, and
//!   a full-text search of the journal, within a size budget.
//! - The question is the user's own request. The journal entries were
//!   written from captured text, so they go in only as `Captured` data, and
//!   the answer must be `{"answer": string, "sources": [number]}`, with
//!   sources among the entries given (GUARDRAILS §5).
//! - Nothing is stored: not the question, not the answer. Logs hold counts
//!   and timings only.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use chrono::{
    DateTime, Datelike, Local, Months, NaiveDate, NaiveTime, TimeDelta, TimeZone, Utc, Weekday,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::journal::EpisodeItem;
use crate::logging::{self, ErrorKind, Field};
use crate::models::structured::{self, Captured, StructuredError, Validate};
use crate::models::{Endpoint, ModelClient, ModelTurn};
use crate::pause::Pause;
use crate::store::{EntityKind, Episode, ModelTask, Store};

/// Most entries and characters of entries given to the model, so the prompt
/// fits a small local model's context and is answered within the timeout.
const MAX_ENTRIES: usize = 40;
/// Smaller than the journal's blocks: the user waits while the model reads
/// it, and finding by meaning picks fewer, better entries.
const MAX_CHARS: usize = 8_000;
/// Most entries taken from each way of finding them.
const PER_SOURCE: usize = 40;
/// Entries found by meaning.
const NEAREST: usize = 12;

/// The question as a vector, for finding entries by meaning.
struct Meaning {
    model: String,
    vector: Vec<f32>,
}

/// Most named entities whose facts are given.
const MAX_KNOWN_ENTITIES: usize = 5;
/// Entries given when nothing in the question narrows it down.
const RECENT: usize = 40;
/// Room for a model that reasons before answering, as for the journal.
const MAX_TOKENS: u32 = 2000;
const MAX_QUESTION_CHARS: usize = 1000;
const MAX_ANSWER_CHARS: usize = 4000;

const TASK: &str = "Answer the user's question using only the captured blocks: what the \
memory knows about the user and about people, organisations and projects, and journal entries. \
Each journal entry starts with its number in brackets, then when it happened and in which apps. \
Answer briefly and directly, in the language of the question. For several things, use short \
lines starting with \"- \". Put the numbers of the journal entries the answer is based on in \
sources, as a list of numbers such as [1, 4] (what the memory knows has no number). Don't put \
entry numbers in the answer. If the blocks don't answer the question, say so plainly; don't \
guess.";
const SHAPE: &str = r#"{"answer": string, "sources": [number]}"#;

/// What the model must answer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    answer: String,
    #[serde(deserialize_with = "sources")]
    sources: Vec<u32>,
}

/// Entry numbers as the model writes them: a list of numbers as asked, but
/// also, from small models answering without reasoning, numbers as text
/// ("4", "[4]"), ranges ("1-8"), comma-separated text ("1, 3") and lists
/// in lists ([[5]]). Anything else, such as words, is refused. Whether the
/// numbers were given is checked afterwards.
fn sources<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<u32>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    let mut out = Vec::new();
    if collect(&value, 0, &mut out).is_none() {
        return Err(serde::de::Error::custom("not entry numbers"));
    }
    // The same entry twice is one source.
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn collect(value: &serde_json::Value, depth: usize, out: &mut Vec<u32>) -> Option<()> {
    use serde_json::Value;
    match value {
        Value::Number(n) => {
            // Whole numbers, also written as 1.0.
            let whole = match n.as_u64() {
                Some(n) => n,
                None => {
                    let f = n.as_f64()?;
                    (f >= 0.0 && f.fract() == 0.0 && f <= f64::from(u32::MAX))
                        .then_some(f as u64)?
                }
            };
            out.push(u32::try_from(whole).ok()?);
        }
        Value::String(text) => numbers(text, out)?,
        Value::Array(items) if depth < 3 => {
            for item in items {
                collect(item, depth + 1, out)?;
            }
        }
        _ => return None,
    }
    Some(())
}

/// "4", "[4]", "1-8", "1, 3" or "1–3" as numbers.
fn numbers(text: &str, out: &mut Vec<u32>) -> Option<()> {
    let text = text.trim().trim_start_matches('[').trim_end_matches(']');
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let part = part.trim_start_matches('[').trim_end_matches(']').trim();
        match part.split_once(['-', '–']) {
            Some((from, to)) => {
                let (from, to): (u32, u32) = (from.trim().parse().ok()?, to.trim().parse().ok()?);
                if to < from || to - from > MAX_ENTRIES as u32 {
                    return None;
                }
                out.extend(from..=to);
            }
            None => out.push(part.parse().ok()?),
        }
    }
    Some(())
}

impl Validate for Reply {
    fn validate(&self) -> Result<(), &'static str> {
        let answer = self.answer.trim();
        if answer.is_empty() {
            return Err("an empty answer");
        }
        if answer.chars().count() > MAX_ANSWER_CHARS {
            return Err("an answer that was too long");
        }
        if answer
            .chars()
            .any(|c| (c.is_control() && c != '\n') || structured::is_invisible(c))
        {
            return Err("an answer with control or invisible characters");
        }
        if self.sources.len() > MAX_ENTRIES {
            return Err("too many sources");
        }
        Ok(())
    }
}

/// The entries a question is about, and how they were found.
#[derive(Debug)]
struct Found {
    episodes: Vec<Episode>,
    /// What the memory's pages say about the user and about the entities
    /// the question names, a block each.
    known: Vec<String>,
    /// Nothing in the question narrowed it down, so these are the latest.
    recent_only: bool,
    /// The question named a time.
    timed: bool,
}

/// Finds the entries `question` is about, oldest first, within the budget.
fn gather<Tz: TimeZone>(
    store: &Store,
    question: &str,
    meaning: Option<&Meaning>,
    now: &DateTime<Tz>,
) -> crate::store::Result<Found> {
    let range = time_range(question, now);
    let asked = words(question);
    let entities = named_entities(&asked, &store.entity_names()?);
    let search = search_words(&asked);

    // Within a named time, every match there counts, not only the newest.
    let limit = if range.is_some() { 500 } else { PER_SOURCE };
    let mentioning: Vec<Episode> = store.episodes_mentioning(&entities, range, limit)?;
    let matching: Vec<Episode> = store.search_episodes(&search, range, limit)?;
    // Closest in meaning, with vectors from the embedding model.
    let near: Vec<Episode> = match meaning {
        Some(meaning) => store.nearest_episodes(&meaning.model, &meaning.vector, range, NEAREST)?,
        None => Vec::new(),
    };
    let mut recent_only = false;

    // In order of priority; the budget cuts from the end.
    let candidates: Vec<Episode> = match range {
        Some((from, to)) => {
            let mut in_range = store.episodes_between(from, to, 500)?;
            in_range.reverse(); // Latest first, so the budget keeps the end of the time.
            if entities.is_empty() && search.is_empty() && near.is_empty() {
                in_range
            } else {
                let wanted: HashSet<i64> = mentioning
                    .iter()
                    .chain(&matching)
                    .chain(&near)
                    .map(|e| e.id)
                    .collect();
                let (first, rest): (Vec<Episode>, Vec<Episode>) =
                    in_range.into_iter().partition(|e| wanted.contains(&e.id));
                first.into_iter().chain(rest).collect()
            }
        }
        None => {
            let both: Vec<Episode> = mentioning.into_iter().chain(matching).chain(near).collect();
            if both.is_empty() {
                recent_only = true;
                store.episodes(None, RECENT)?
            } else {
                both
            }
        }
    };

    // What the memory knows comes first, and counts against the budget.
    let known = known(store, &entities)?;
    let mut seen = HashSet::new();
    let mut chars: usize = known.iter().map(|k| k.chars().count()).sum();
    let mut episodes = Vec::new();
    for episode in candidates {
        if episodes.len() >= MAX_ENTRIES {
            break;
        }
        if !seen.insert(episode.id) {
            continue;
        }
        let length = episode.summary.chars().count() + 60;
        if chars + length > MAX_CHARS {
            break;
        }
        chars += length;
        episodes.push(episode);
    }
    episodes.sort_by_key(|e| (e.started_at, e.id));
    Ok(Found {
        known,
        episodes,
        recent_only,
        timed: range.is_some(),
    })
}

/// What the memory knows that bears on `text` (the writing assistant's
/// instruction): `about-me.md`, and the pages of the entities named in it.
/// Also the names of those pages, "you" for about-me, for the user to see.
pub(crate) fn known_for(
    store: &Store,
    text: &str,
) -> crate::store::Result<(Vec<String>, Vec<String>)> {
    let named = named_entities(&words(text), &store.entity_names()?);
    let known = known(store, &named)?;
    let mut pages = Vec::new();
    if store
        .about_me_page()?
        .and_then(|p| p.facts)
        .is_some_and(|facts| !facts.is_empty())
    {
        pages.push("you".to_string());
    }
    let all = store.entities()?;
    for id in named.iter().take(MAX_KNOWN_ENTITIES) {
        let has_facts = store
            .page_of(*id)?
            .and_then(|p| p.facts)
            .is_some_and(|facts| !facts.is_empty());
        if let Some(entity) = all.iter().find(|e| e.id == *id).filter(|_| has_facts) {
            pages.push(entity.name.clone());
        }
    }
    Ok((known, pages))
}

/// The facts on the pages of the named entities, and on `about-me.md`.
fn known(store: &Store, entities: &[i64]) -> crate::store::Result<Vec<String>> {
    let mut known = Vec::new();
    if let Some(facts) = store.about_me_page()?.and_then(|p| p.facts)
        && !facts.is_empty()
    {
        known.push(format!(
            "What the memory knows about the user:\n{}",
            bullets(&facts)
        ));
    }
    let all = store.entities()?;
    for id in entities.iter().take(MAX_KNOWN_ENTITIES) {
        let (Some(entity), Some(facts)) = (
            all.iter().find(|e| e.id == *id),
            store.page_of(*id)?.and_then(|p| p.facts),
        ) else {
            continue;
        };
        if facts.is_empty() {
            continue;
        }
        let kind = match entity.kind {
            EntityKind::Person => "person",
            EntityKind::Organisation => "organisation",
            EntityKind::Project => "project",
        };
        known.push(format!(
            "What the memory knows about {} ({kind}):\n{}",
            entity.name,
            bullets(&facts)
        ));
    }
    Ok(known)
}

fn bullets(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| format!("- {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The words of `text`, lowercased, split at anything that isn't a letter
/// or digit.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// The entities whose names appear in the question as whole words.
fn named_entities(asked: &[String], names: &[(i64, String)]) -> Vec<i64> {
    let question = format!(" {} ", asked.join(" "));
    let mut found: Vec<i64> = names
        .iter()
        .filter(|(_, key)| {
            let name = words(key).join(" ");
            !name.is_empty() && question.contains(&format!(" {name} "))
        })
        .map(|(id, _)| *id)
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// Words worth searching the journal for: not the little words of a
/// question, not words about time, and at least three letters (or with a
/// digit, like "Q3").
fn search_words(asked: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    asked
        .iter()
        .filter(|w| w.chars().count() >= 3 || w.chars().any(|c| c.is_ascii_digit()))
        .filter(|w| !STOP_WORDS.contains(&w.as_str()) && !is_time_word(w))
        .filter(|w| seen.insert(w.as_str()))
        .take(12)
        .cloned()
        .collect()
}

const STOP_WORDS: &[&str] = &[
    // Swedish
    "vad",
    "vem",
    "vilka",
    "vilken",
    "vilket",
    "när",
    "var",
    "hur",
    "varför",
    "gjorde",
    "göra",
    "gör",
    "har",
    "hade",
    "jag",
    "mig",
    "min",
    "mitt",
    "mina",
    "med",
    "och",
    "att",
    "det",
    "den",
    "som",
    "för",
    "från",
    "till",
    "om",
    "på",
    "av",
    "en",
    "ett",
    "är",
    "var",
    "vara",
    "blev",
    "kan",
    "kunde",
    "ska",
    "skulle",
    "något",
    "några",
    "allt",
    "alla",
    "mest",
    "jobbade",
    "jobbat",
    "arbetade",
    "arbetat",
    "sysslade",
    "pratade",
    "pratat",
    "senast",
    "status",
    "läget",
    "hände",
    "hänt",
    "berätta",
    "sammanfatta",
    "lax",
    // English
    "what",
    "who",
    "which",
    "when",
    "where",
    "how",
    "why",
    "did",
    "does",
    "doing",
    "done",
    "have",
    "has",
    "had",
    "the",
    "and",
    "with",
    "for",
    "from",
    "about",
    "was",
    "were",
    "are",
    "this",
    "that",
    "those",
    "these",
    "any",
    "all",
    "most",
    "work",
    "worked",
    "working",
    "talk",
    "talked",
    "last",
    "latest",
    "happened",
    "tell",
    "summarize",
    "summarise",
    "can",
    "you",
];

/// Month names are left in: "Jan" and "Maj" are names too, and a month
/// name alone doesn't narrow the time.
fn is_time_word(word: &str) -> bool {
    TIME_WORDS.contains(&word) || weekday(word).is_some()
}

const TIME_WORDS: &[&str] = &[
    "idag",
    "dag",
    "igår",
    "går",
    "förrgår",
    "morse",
    "imorse",
    "eftermiddag",
    "kväll",
    "veckan",
    "vecka",
    "förra",
    "denna",
    "här",
    "månaden",
    "månad",
    "today",
    "yesterday",
    "morning",
    "afternoon",
    "evening",
    "week",
    "month",
    "before",
];

/// The time `question` names, as a range in UTC, using the local calendar
/// of `now`. Swedish and English. A question that names several times
/// ("igår och idag") gets the span from the first to the last.
fn time_range<Tz: TimeZone>(
    question: &str,
    now: &DateTime<Tz>,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let asked = words(question);
    let text = format!(" {} ", asked.join(" "));
    let has = |phrase: &str| text.contains(&format!(" {phrase} "));
    let today = now.date_naive();
    let tz = now.timezone();
    // Each named time as a span of days, end excluded.
    let mut found: Vec<(NaiveDate, NaiveDate)> = Vec::new();
    let span = |from: NaiveDate, count: i64| (from, from + TimeDelta::days(count));

    if has("förrgår") || has("day before yesterday") {
        found.push(span(today - TimeDelta::days(2), 1));
    }
    if has("igår") || has("i går") || (has("yesterday") && !has("day before yesterday")) {
        found.push(span(today - TimeDelta::days(1), 1));
    }
    if has("idag")
        || has("i dag")
        || has("today")
        || has("imorse")
        || has("i morse")
        || has("this morning")
        || has("this afternoon")
        || has("i eftermiddag")
    {
        found.push(span(today, 1));
    }
    let monday = today - TimeDelta::days(i64::from(today.weekday().num_days_from_monday()));
    if has("förra veckan") || has("last week") {
        found.push(span(monday - TimeDelta::days(7), 7));
    }
    if has("den här veckan") || has("denna vecka") || has("i veckan") || has("this week") {
        found.push(span(monday, 7));
    }
    if let Some(first) = today.with_day(1) {
        if (has("förra månaden") || has("last month"))
            && let Some(previous) = first.checked_sub_months(Months::new(1))
        {
            found.push((previous, first));
        }
        if (has("den här månaden") || has("denna månad") || has("this month"))
            && let Some(next) = first.checked_add_months(Months::new(1))
        {
            found.push((first, next));
        }
    }
    // Dates: 2026-10-07, "7 oktober", "7 okt", "October 7".
    for word in question.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_ascii_digit());
        if let Ok(date) = NaiveDate::parse_from_str(word, "%Y-%m-%d") {
            found.push(span(date, 1));
        }
    }
    for pair in asked.windows(2) {
        let (day, name) = match (pair[0].parse::<u32>(), pair[1].parse::<u32>()) {
            (Ok(day), Err(_)) => (day, &pair[1]),
            (Err(_), Ok(day)) => (day, &pair[0]),
            _ => continue,
        };
        let Some(month) = month(name) else { continue };
        // Not a date ("maj 2026", "31 februari"): look further. A date
        // later this year means last year's.
        let date = NaiveDate::from_ymd_opt(today.year(), month, day)
            .filter(|date| *date <= today)
            .or_else(|| NaiveDate::from_ymd_opt(today.year() - 1, month, day));
        if let Some(date) = date {
            found.push(span(date, 1));
        }
    }
    // Weekdays: "i tisdags" is the last one before today; "tisdag" or
    // "Tuesday" may be today.
    for word in &asked {
        if let Some((day, past)) = weekday(word) {
            let back =
                (7 + today.weekday().num_days_from_monday() - day.num_days_from_monday()) % 7;
            let back = if back == 0 && past { 7 } else { back };
            found.push(span(today - TimeDelta::days(i64::from(back)), 1));
        }
    }
    let from = found.iter().map(|(from, _)| *from).min()?;
    let to = found.iter().map(|(_, to)| *to).max()?;
    Some((local_midnight(&tz, from)?, local_midnight(&tz, to)?))
}

fn local_midnight<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&date.and_time(NaiveTime::MIN))
        .earliest()
        .map(|t| t.with_timezone(&Utc))
}

/// A weekday name, and whether it is the Swedish past form ("tisdags").
fn weekday(word: &str) -> Option<(Weekday, bool)> {
    const DAYS: &[(&str, Weekday)] = &[
        ("måndag", Weekday::Mon),
        ("tisdag", Weekday::Tue),
        ("onsdag", Weekday::Wed),
        ("torsdag", Weekday::Thu),
        ("fredag", Weekday::Fri),
        ("lördag", Weekday::Sat),
        ("söndag", Weekday::Sun),
        ("monday", Weekday::Mon),
        ("tuesday", Weekday::Tue),
        ("wednesday", Weekday::Wed),
        ("thursday", Weekday::Thu),
        ("friday", Weekday::Fri),
        ("saturday", Weekday::Sat),
        ("sunday", Weekday::Sun),
    ];
    DAYS.iter().find_map(|(name, day)| {
        if word == *name {
            Some((*day, false))
        } else if word.strip_suffix('s') == Some(name) {
            Some((*day, true))
        } else {
            None
        }
    })
}

/// A month name or its short form, Swedish or English, as 1–12.
fn month(word: &str) -> Option<u32> {
    const MONTHS: &[&[&str]] = &[
        &["januari", "january", "jan"],
        &["februari", "february", "feb"],
        &["mars", "march", "mar"],
        &["april", "apr"],
        &["maj", "may"],
        &["juni", "june", "jun"],
        &["juli", "july", "jul"],
        &["augusti", "august", "aug"],
        &["september", "sep", "sept"],
        &["oktober", "october", "okt", "oct"],
        &["november", "nov"],
        &["december", "dec"],
    ];
    MONTHS
        .iter()
        .position(|names| names.contains(&word))
        .and_then(|i| u32::try_from(i + 1).ok())
}

/// One entry as the model sees it: "[3] Tue 2026-10-07 14:05–14:20 (Mail,
/// Slack): …". All of it goes in as captured data.
fn entry_text<Tz: TimeZone>(number: usize, episode: &Episode, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let start = episode.started_at.with_timezone(tz);
    let end = episode.ended_at.with_timezone(tz);
    format!(
        "[{number}] {} {}–{} ({}): {}",
        start.format("%a %Y-%m-%d"),
        start.format("%H:%M"),
        end.format("%H:%M"),
        episode.app_names.join(", "),
        episode.summary
    )
}

/// The question, with today's date so the model can place "yesterday".
fn request<Tz: TimeZone>(question: &str, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    format!(
        "Today is {}.\n\nQuestion: {question}",
        now.format("%A %Y-%m-%d, %H:%M")
    )
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AskAnswer {
    pub answer: String,
    /// The entries the answer says it is based on.
    pub sources: Vec<EpisodeItem>,
    /// How many entries the model was given.
    pub considered: usize,
    /// Nothing in the question narrowed it down, so the model was given
    /// the latest entries.
    pub recent_only: bool,
}

/// Answers `question` from the journal with the chosen chat model.
#[tauri::command]
pub async fn ask_memory(app: AppHandle, question: String) -> Result<AskAnswer, String> {
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err("Type a question.".to_string());
    }
    if question.chars().count() > MAX_QUESTION_CHARS {
        return Err(format!(
            "A question can be at most {MAX_QUESTION_CHARS} characters."
        ));
    }
    if app.state::<Pause>().is_paused() {
        return Err("Lax is paused. Resume it from the menu bar to ask.".to_string());
    }
    let now = Local::now();
    // One turn at the model for the whole question (in turn with the journal
    // and the memory), so a background call can't slip in between the
    // embedding and the answer. A pause meanwhile drops the question
    // (GUARDRAILS §4).
    let turn = app.state::<ModelTurn>();
    let Some(_turn) = app
        .state::<Pause>()
        .unless_paused(turn.take_interactive())
        .await
    else {
        return Err("Lax was paused, so the question was dropped.".to_string());
    };
    // With an embedding model, the question as a vector. If the model can't
    // be reached, the question is answered without it.
    let embedding = with_store(&app, |store| store.model_choice(ModelTask::Embeddings))
        .and_then(Result::ok)
        .flatten();
    let meaning = match embedding {
        Some(choice) => {
            let embedded = crate::vectors::embed(
                &choice.base_url,
                &choice.model,
                std::slice::from_ref(&question),
            );
            app.state::<Pause>()
                .unless_paused(embedded)
                .await
                .and_then(Result::ok)
                .and_then(|mut v| v.pop())
                .map(|vector| Meaning {
                    model: choice.model,
                    vector,
                })
        }
        None => None,
    };
    let (choice, found) = {
        let store = app
            .try_state::<Mutex<Store>>()
            .ok_or("The database isn't available.")?;
        let store = store.lock().unwrap_or_else(PoisonError::into_inner);
        let failed = |_| "Lax couldn't read the journal.".to_string();
        let choice = store
            .model_choice(ModelTask::Chat)
            .map_err(failed)?
            .ok_or("Choose a chat model under Models first.")?;
        (
            choice,
            gather(&store, &question, meaning.as_ref(), &now).map_err(failed)?,
        )
    };
    // A time with nothing in it is answered as such: what the memory knows
    // in general says nothing about that day.
    if found.episodes.is_empty() && (found.timed || found.known.is_empty()) {
        return Ok(AskAnswer {
            answer: if found.timed {
                "There's nothing in the journal from that time.".to_string()
            } else {
                "There's nothing in the journal yet. Turn the journal on, and ask again once it \
                 has written a few entries."
                    .to_string()
            },
            sources: Vec::new(),
            considered: 0,
            recent_only: found.recent_only,
        });
    }

    let Ok(endpoint) = Endpoint::parse(&choice.base_url) else {
        return Err("The chat model's address isn't valid. Check it under Models.".to_string());
    };
    let started = std::time::Instant::now();
    let texts: Vec<String> = found
        .known
        .iter()
        .cloned()
        .chain(
            found
                .episodes
                .iter()
                .enumerate()
                .map(|(i, e)| entry_text(i + 1, e, &Local)),
        )
        .collect();
    let request = request(&question, &now);
    let asked = answer(endpoint, &choice.model, &request, &texts);
    let Some(reply) = app.state::<Pause>().unless_paused(asked).await else {
        return Err("Lax was paused, so the question was dropped.".to_string());
    };
    let reply = reply.and_then(|reply| {
        if reply
            .sources
            .iter()
            .any(|&n| n == 0 || n as usize > found.episodes.len())
        {
            return Err(StructuredError::Invalid("a source that wasn't given"));
        }
        Ok(reply)
    });
    match reply {
        Ok(reply) => {
            let mut cited: Vec<usize> = reply.sources.iter().map(|&n| n as usize - 1).collect();
            cited.sort_unstable();
            cited.dedup();
            logging::info(
                "ask.answered",
                &[
                    Field::Count("entries", found.episodes.len() as u64),
                    Field::Count("cited", cited.len() as u64),
                    Field::Flag("recent_only", found.recent_only),
                    Field::Duration("elapsed", started.elapsed()),
                ],
            );
            Ok(AskAnswer {
                answer: without_references(&reply.answer),
                sources: cited
                    .into_iter()
                    .map(|i| item(&found.episodes[i]))
                    .collect(),
                considered: found.episodes.len(),
                recent_only: found.recent_only,
            })
        }
        Err(error) => {
            logging::warn(
                "ask.failed",
                &[
                    Field::Flag("model_unavailable", error.model_unavailable()),
                    Field::Error(ErrorKind::Other),
                ],
            );
            Err(if error.model_unavailable() {
                "Lax couldn't reach the model. Is the model server running?".to_string()
            } else {
                "The model's answer couldn't be used. Try asking again, perhaps in other words."
                    .to_string()
            })
        }
    }
}

async fn answer(
    endpoint: Endpoint,
    model: &str,
    request: &str,
    texts: &[String],
) -> Result<Reply, StructuredError> {
    // The user waits for this: no reasoning, which makes a small local model
    // about ten times faster.
    let client = ModelClient::new(endpoint)
        .map_err(StructuredError::Model)?
        .without_reasoning();
    let captured: Vec<Captured> = texts.iter().map(|t| Captured(t.as_str())).collect();
    structured::ask_for(
        &client,
        model,
        TASK,
        SHAPE,
        Some(request),
        &captured,
        MAX_TOKENS,
    )
    .await
}

fn with_store<T>(app: &AppHandle, work: impl FnOnce(&Store) -> T) -> Option<T> {
    let store = app.try_state::<Mutex<Store>>()?;
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    Some(work(&store))
}

/// The answer without entry references such as "[3]" or "[1-8]", which
/// models put in although sources are shown on their own.
fn without_references(answer: &str) -> String {
    let mut out = String::with_capacity(answer.len());
    let mut rest = answer;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find(']') {
            Some(close)
                if close > 0
                    && after[..close]
                        .chars()
                        .all(|c| c.is_ascii_digit() || matches!(c, ',' | '-' | '–' | ' '))
                    && after[..close].chars().any(|c| c.is_ascii_digit()) =>
            {
                // Drop the space before it too.
                while out.ends_with(' ') {
                    out.pop();
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn item(e: &Episode) -> EpisodeItem {
    EpisodeItem {
        id: e.id,
        started_at: e.started_at.timestamp_millis(),
        ended_at: e.ended_at.timestamp_millis(),
        summary: e.summary.clone(),
        app_names: e.app_names.clone(),
        captures: e.captures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{EntityKind, NewCapture, NewEpisode};
    use chrono::FixedOffset;

    fn stockholm() -> FixedOffset {
        FixedOffset::east_opt(2 * 3600).unwrap()
    }

    /// Sunday 11 October 2026, 15:00 in Stockholm.
    fn now() -> DateTime<FixedOffset> {
        stockholm()
            .with_ymd_and_hms(2026, 10, 11, 15, 0, 0)
            .unwrap()
    }

    fn day(range: Option<(DateTime<Utc>, DateTime<Utc>)>) -> Option<(String, i64)> {
        range.map(|(from, to)| {
            (
                from.with_timezone(&stockholm())
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
                (to - from).num_days(),
            )
        })
    }

    fn on(date: &str, days: i64) -> Option<(String, i64)> {
        Some((format!("{date} 00:00"), days))
    }

    #[test]
    fn times_in_swedish_and_english() {
        let range = |q: &str| day(time_range(q, &now()));
        assert_eq!(range("Vad gjorde jag idag?"), on("2026-10-11", 1));
        assert_eq!(range("vad gjorde jag i dag"), on("2026-10-11", 1));
        assert_eq!(range("What did I do this morning?"), on("2026-10-11", 1));
        assert_eq!(range("Vad gjorde jag igår?"), on("2026-10-10", 1));
        assert_eq!(range("och i går då"), on("2026-10-10", 1));
        assert_eq!(range("yesterday"), on("2026-10-10", 1));
        assert_eq!(range("i förrgår"), on("2026-10-09", 1));
        // On a Sunday, "i tisdags" is this week's Tuesday.
        assert_eq!(range("Vad gjorde jag i tisdags?"), on("2026-10-06", 1));
        assert_eq!(range("on Tuesday"), on("2026-10-06", 1));
        assert_eq!(range("i lördags"), on("2026-10-10", 1));
        // "i söndags" on a Sunday is a week ago; "söndag" is today.
        assert_eq!(range("i söndags"), on("2026-10-04", 1));
        assert_eq!(range("söndag"), on("2026-10-11", 1));
        assert_eq!(range("förra veckan"), on("2026-09-28", 7));
        assert_eq!(range("What did I do last week?"), on("2026-09-28", 7));
        assert_eq!(range("i veckan"), on("2026-10-05", 7));
        assert_eq!(range("förra månaden"), on("2026-09-01", 30));
        assert_eq!(range("den 7 oktober"), on("2026-10-07", 1));
        assert_eq!(range("7 okt"), on("2026-10-07", 1));
        assert_eq!(range("on October 7"), on("2026-10-07", 1));
        assert_eq!(range("2026-10-07?"), on("2026-10-07", 1));
        // A date later this year is last year's.
        assert_eq!(range("24 december"), on("2025-12-24", 1));
        // Several times: the span from the first to the last.
        assert_eq!(range("Vad gjorde jag igår och idag?"), on("2026-10-10", 2));
        assert_eq!(range("i måndags och i tisdags"), on("2026-10-05", 2));
        // Not a date, but the weekday still counts.
        assert_eq!(range("i tisdags, typ 2026 maj"), on("2026-10-06", 1));
        assert_eq!(range("31 februari"), None);
        assert_eq!(range("Hur går det med Q3-offerten?"), None);
        assert_eq!(range("Vem är Anna Example?"), None);
    }

    #[test]
    fn search_words_leave_out_question_and_time_words() {
        assert_eq!(
            search_words(&words("Vad gjorde jag med Q3-offerten i tisdags?")),
            ["q3", "offerten"]
        );
        assert_eq!(
            search_words(&words("What did I work on with the launch last week?")),
            ["launch"]
        );
        assert!(search_words(&words("Vad gjorde jag igår?")).is_empty());
    }

    #[test]
    fn names_are_found_as_whole_words() {
        let names = vec![
            (1, "anna example".to_string()),
            (1, "anna".to_string()),
            (2, "bo".to_string()),
            (3, "q3 lansering".to_string()),
        ];
        assert_eq!(
            named_entities(&words("Vad pratade jag med Anna om?"), &names),
            [1]
        );
        // "bo" is not found inside "bokade".
        assert!(named_entities(&words("Vem bokade mötet?"), &names).is_empty());
        assert_eq!(
            named_entities(&words("Hur går Q3-lanseringen? Q3 lansering?"), &names),
            [3]
        );
    }

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        stockholm()
            .with_ymd_and_hms(2026, 10, day, hour, 0, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn episode(store: &mut Store, when: DateTime<Utc>, summary: &str) -> i64 {
        let id = store
            .insert(&NewCapture {
                captured_at: when,
                bundle_id: "com.example.mail",
                app_name: "Mail",
                window_title: "Synthetic window",
                site: None,
                text: "Synthetic text",
            })
            .unwrap()
            .unwrap();
        store
            .record_episode(
                &NewEpisode {
                    summary: summary.to_string(),
                    model: "example-model".to_string(),
                    created_at: when,
                },
                &[id],
            )
            .unwrap()
            .unwrap()
    }

    fn sample() -> Store {
        let mut store = Store::open_in_memory().unwrap();
        let tuesday = episode(&mut store, at(6, 10), "Mailade Anna om Q3-offerten.");
        episode(&mut store, at(6, 14), "Läste nyheter.");
        episode(&mut store, at(9, 9), "Planerade lanseringen med Bo.");
        let friday = episode(&mut store, at(10, 11), "Ringde Anna om leveransen.");
        for id in [tuesday, friday] {
            store
                .record_mentions(
                    id,
                    &[(EntityKind::Person, "Anna Example".to_string())],
                    at(10, 12),
                )
                .unwrap();
        }
        store
            .rename_entity(store.entity_names().unwrap()[0].0, "Anna")
            .unwrap();
        store
    }

    fn summaries(found: &Found) -> Vec<&str> {
        found.episodes.iter().map(|e| e.summary.as_str()).collect()
    }

    #[test]
    fn a_time_gives_that_time_and_puts_matches_first() {
        let store = sample();
        let found = gather(&store, "Vad gjorde jag i tisdags?", None, &now()).unwrap();
        assert_eq!(
            summaries(&found),
            ["Mailade Anna om Q3-offerten.", "Läste nyheter."]
        );
        assert!(found.timed && !found.recent_only);
        let found = gather(&store, "Vad gjorde jag igår?", None, &now()).unwrap();
        assert_eq!(summaries(&found), ["Ringde Anna om leveransen."]);
        let found = gather(&store, "Vad gjorde jag i torsdags?", None, &now()).unwrap();
        assert!(found.episodes.is_empty() && found.timed);
    }

    #[test]
    fn within_a_time_every_match_there_comes_first() {
        let mut store = sample();
        // A busy Tuesday, more than the budget holds…
        for hour in 11..23 {
            let when = at(6, hour) + TimeDelta::minutes(30);
            episode(&mut store, when, &"Synthetic. ".repeat(100));
        }
        // …and more newer entries about Anna than one search returns.
        for day in 7..=10 {
            for hour in 0..12 {
                let id = episode(&mut store, at(day, hour), "Anna igen.");
                store
                    .record_mentions(id, &[(EntityKind::Person, "Anna".to_string())], at(10, 21))
                    .unwrap();
            }
        }
        let found = gather(
            &store,
            "Vad pratade jag med Anna om i tisdags?",
            None,
            &now(),
        )
        .unwrap();
        assert!(
            found
                .episodes
                .iter()
                .any(|e| e.summary == "Mailade Anna om Q3-offerten.")
        );
    }

    #[test]
    fn what_the_memory_knows_comes_along() {
        let mut store = sample();
        let anna = store.entity_names().unwrap()[0].0;
        store.keep_entity(anna).unwrap();
        let folder = std::env::temp_dir().join(format!(
            "lax-ask-test-{}",
            crate::pages::block::new_id().unwrap()
        ));
        crate::pages::prepare_folder(&folder).unwrap();
        store.set_memory_enabled(true).unwrap();
        crate::pages::sync(
            &mut store,
            &folder,
            &mut crate::pages::ScanCache::new(),
            at(11, 12),
            &stockholm(),
        )
        .unwrap();
        let page = store.page_of(anna).unwrap().unwrap();
        let sources: Vec<i64> = store
            .episodes_mentioning(&[anna], None, 10)
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect();
        store
            .set_facts(
                &page.id,
                &["Leder Q3-lanseringen.".to_string()],
                &sources,
                at(11, 12),
            )
            .unwrap();
        let about = store.about_me_page().unwrap().unwrap();
        store
            .set_facts(
                &about.id,
                &["Jobbar med lanseringar.".to_string()],
                &sources,
                at(11, 12),
            )
            .unwrap();

        let found = gather(&store, "Vad gör Anna?", None, &now()).unwrap();
        assert_eq!(
            found.known,
            [
                "What the memory knows about the user:\n- Jobbar med lanseringar.",
                "What the memory knows about Anna (person):\n- Leder Q3-lanseringen.",
            ]
        );
        // A question about no one still gets what the memory knows about the user.
        let found = gather(&store, "Sammanfatta", None, &now()).unwrap();
        assert_eq!(found.known.len(), 1);
        // It counts against the budget: entries make room for it.
        let known: usize = found.known.iter().map(|k| k.chars().count()).sum();
        let entries: usize = found
            .episodes
            .iter()
            .map(|e| e.summary.chars().count() + 60)
            .sum();
        assert!(known + entries <= MAX_CHARS);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn entries_are_found_by_meaning_too() {
        let mut store = sample();
        // Vectors: the offer and the delivery point one way, the rest another.
        let all = store.episodes(None, 10).unwrap();
        let vectors: Vec<(i64, Vec<f32>)> = all
            .iter()
            .map(|e| {
                let about_the_deal = e.summary.contains("offert") || e.summary.contains("leverans");
                (
                    e.id,
                    if about_the_deal {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    },
                )
            })
            .collect();
        store.set_vectors("m", &vectors).unwrap();
        let meaning = Meaning {
            model: "m".to_string(),
            vector: vec![0.9, 0.1],
        };
        // No name, word or time matches "budgeten", but the meaning does.
        let found = gather(&store, "Hur gick det med budgeten?", Some(&meaning), &now()).unwrap();
        assert!(!found.recent_only);
        let summaries: Vec<&str> = found.episodes.iter().map(|e| e.summary.as_str()).collect();
        assert!(summaries.contains(&"Mailade Anna om Q3-offerten."));
        assert!(summaries.contains(&"Ringde Anna om leveransen."));
        // Without vectors it falls back to the latest entries.
        let found = gather(&store, "Hur gick det med budgeten?", None, &now()).unwrap();
        assert!(found.recent_only);
    }

    #[test]
    fn names_and_words_find_entries_at_any_time() {
        let store = sample();
        let found = gather(&store, "Vad har jag haft för mig med Anna?", None, &now()).unwrap();
        assert_eq!(
            summaries(&found),
            ["Mailade Anna om Q3-offerten.", "Ringde Anna om leveransen."]
        );
        let found = gather(&store, "Hur går lanseringen?", None, &now()).unwrap();
        assert_eq!(summaries(&found), ["Planerade lanseringen med Bo."]);
    }

    #[test]
    fn without_anything_to_go_on_the_latest_entries_are_given() {
        let store = sample();
        let found = gather(&store, "Sammanfatta", None, &now()).unwrap();
        assert!(found.recent_only);
        assert_eq!(found.episodes.len(), 4);
        // Oldest first.
        assert_eq!(found.episodes[0].summary, "Mailade Anna om Q3-offerten.");
    }

    #[test]
    fn the_budget_keeps_the_prompt_small() {
        let mut store = Store::open_in_memory().unwrap();
        let long = "Synthetic summary. ".repeat(30);
        for hour in 0..24 {
            episode(&mut store, at(10, hour), &long);
        }
        let found = gather(&store, "Vad gjorde jag igår?", None, &now()).unwrap();
        let chars: usize = found
            .episodes
            .iter()
            .map(|e| e.summary.chars().count())
            .sum();
        assert!(chars <= MAX_CHARS);
        assert!(found.episodes.len() < 24);
        // The end of the day is kept.
        assert_eq!(found.episodes.last().unwrap().started_at, at(10, 23));
    }

    #[test]
    fn entry_references_are_taken_out_of_answers() {
        assert_eq!(
            without_references("Du mailade Anna [1] och planerade mötet [2-3].\n[1-8]"),
            "Du mailade Anna och planerade mötet."
        );
        assert_eq!(without_references("Se [1, 4] nedan"), "Se nedan");
        // Brackets that aren't references stay.
        assert_eq!(
            without_references("Lista [a] och [ ] och [x1]"),
            "Lista [a] och [ ] och [x1]"
        );
        assert_eq!(
            without_references("- rad ett\n- rad två"),
            "- rad ett\n- rad två"
        );
    }

    #[test]
    fn answers_are_validated() {
        let parse = |json: &str| structured::parse::<Reply>(json);
        assert!(parse(r#"{"answer":"Du mailade Anna.","sources":[1]}"#).is_ok());
        assert!(parse(r#"{"answer":"Rad ett\n- rad två","sources":[]}"#).is_ok());
        assert!(parse(r#"{"answer":"  ","sources":[]}"#).is_err());
        assert!(parse(r#"{"answer":"ok","sources":[1],"action":"send"}"#).is_err());
        assert!(parse("{\"answer\":\"ok\u{202E}hidden\",\"sources\":[]}").is_err());
        // Numbers as small models write them without reasoning.
        let sources = |json: &str| parse(json).map(|r| r.sources).ok();
        assert_eq!(sources(r#"{"answer":"ok","sources":["1"]}"#), Some(vec![1]));
        assert_eq!(
            sources(r#"{"answer":"ok","sources":"1-3"}"#),
            Some(vec![1, 2, 3])
        );
        assert_eq!(
            sources(r#"{"answer":"ok","sources":"1, 4"}"#),
            Some(vec![1, 4])
        );
        assert_eq!(
            sources(r#"{"answer":"ok","sources":[2, "5-6"]}"#),
            Some(vec![2, 5, 6])
        );
        // Not numbers, or a range too long to be real, is refused.
        assert!(parse(r#"{"answer":"ok","sources":"all of them"}"#).is_err());
        assert!(parse(r#"{"answer":"ok","sources":"1-100000"}"#).is_err());
        assert!(parse(r#"{"answer":"ok","sources":"5-2"}"#).is_err());
        assert!(parse(r#"{"answer":"ok","sources":[true]}"#).is_err());
        assert!(parse(r#"{"answer":"ok","sources":[[[[1]]]]}"#).is_err());
        assert!(parse(r#"{"answer":"ok","sources":[-1]}"#).is_err());
        // More shapes from the live probe.
        assert_eq!(sources(r#"{"answer":"ok","sources":[[5]]}"#), Some(vec![5]));
        assert_eq!(
            sources(r#"{"answer":"ok","sources":["[1]", "[2]"]}"#),
            Some(vec![1, 2])
        );
        assert_eq!(
            sources(r#"{"answer":"ok","sources":"[3, 4]"}"#),
            Some(vec![3, 4])
        );
        assert_eq!(
            sources(r#"{"answer":"ok","sources":"2–3"}"#),
            Some(vec![2, 3])
        );
        assert_eq!(
            sources(r#"{"answer":"ok","sources":[1.0, 3.0]}"#),
            Some(vec![1, 3])
        );
        assert!(parse(r#"{"answer":"ok","sources":[1.5]}"#).is_err());
        // Repeats count once.
        assert_eq!(
            sources(r#"{"answer":"ok","sources":["1-30", 1, 2, 3, 30]}"#),
            Some((1..=30).collect())
        );
        assert!(
            parse(&format!(
                r#"{{"answer":"{}","sources":[]}}"#,
                "a".repeat(4001)
            ))
            .is_err()
        );
    }

    #[test]
    fn entries_show_when_and_where() {
        let episode = Episode {
            id: 1,
            started_at: at(6, 14),
            ended_at: at(6, 14) + TimeDelta::minutes(15),
            summary: "Mailade Anna.".to_string(),
            app_names: vec!["Mail".to_string(), "Slack".to_string()],
            captures: 3,
        };
        assert_eq!(
            entry_text(3, &episode, &stockholm()),
            "[3] Tue 2026-10-06 14:00–14:15 (Mail, Slack): Mailade Anna."
        );
        assert!(request("Vad gjorde jag?", &now()).starts_with("Today is Sunday 2026-10-11"));
    }

    /// Against a real model server, like `journal::tests::live_journal_episode`.
    #[tokio::test]
    #[ignore = "needs a local model server: LAX_LIVE_MODEL_URL, LAX_LIVE_CHAT_MODEL"]
    async fn live_answer() {
        let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let store = sample();
        let found = gather(
            &store,
            "Vad pratade jag med Anna om i tisdags?",
            None,
            &now(),
        )
        .unwrap();
        let texts: Vec<String> = found
            .episodes
            .iter()
            .enumerate()
            .map(|(i, e)| entry_text(i + 1, e, &stockholm()))
            .collect();
        let reply = answer(
            Endpoint::parse(&env("LAX_LIVE_MODEL_URL")).unwrap(),
            &env("LAX_LIVE_CHAT_MODEL"),
            &request("Vad pratade jag med Anna om i tisdags?", &now()),
            &texts,
        )
        .await
        .expect("an answer");
        assert!(reply.answer.to_lowercase().contains("offert"));
        assert!(reply.sources.contains(&1));
    }
}
