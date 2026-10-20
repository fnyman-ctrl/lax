//! Ask your memory, first version (Stage 5): the user asks a question in
//! Lax, and the chat model on this Mac answers from the journal.
//!
//! - Finding what the question is about takes no model call: a time named
//!   in the question ("i tisdags", "last week"), names the memory knows, and
//!   a full-text search of the journal, within a size budget. A question
//!   that names no time also gets a few captures found by word, cut to the
//!   part around the match (#193).
//! - The question is the user's own request. The journal entries were
//!   written from captured text, and the captures are captured text, so
//!   they go in only as `Captured` data, and the answer must be
//!   `{"answer": string, "sources": [number]}`, with sources among the
//!   entries and captures given (GUARDRAILS §5).
//! - The answer's text is shown in the window as the model writes it, as
//!   plain text, before the reply is validated (the one exception in
//!   GUARDRAILS §5). Only the window that asked gets it, through the
//!   command's own channel. When the reply is complete it is validated in
//!   full, and the validated answer replaces the streamed text; if it fails,
//!   the window shows the usual error instead and drops what was streamed.
//! - Nothing is stored: not the question, not the answer. Logs hold counts
//!   and timings only.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{
    DateTime, Datelike, Local, Months, NaiveDate, NaiveTime, TimeDelta, TimeZone, Utc, Weekday,
};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager};

use crate::journal::EpisodeItem;
use crate::logging::{self, ErrorKind, Field};
use crate::models::partial::FieldText;
use crate::models::structured::{self, Captured, StructuredError, Validate};
use crate::models::{Endpoint, ModelClient, ModelHealth, ModelTurn};
use crate::pause::Pause;
use crate::store::{Entity, EntityKind, Episode, ModelTask, Store};

/// Most entries and characters of entries given to the model, so the prompt
/// fits a small local model's context and is answered within the timeout.
const MAX_ENTRIES: usize = 40;
/// Smaller than the journal's blocks: the user waits while the model reads
/// it, and finding by meaning picks fewer, better entries. From 8,000 to
/// 5,000 took about 1,000 prompt tokens, and 2 seconds before the first
/// word, off a question on an M1 Pro (#124).
const MAX_CHARS: usize = 5_000;
/// Most entries taken from each way of finding them. A count of candidates,
/// not a size: the budget above decides how many of them are given.
const PER_SOURCE: usize = 40;
/// Entries found by meaning. They take turns with those found by name and
/// by word, so some of them are given even when the others are many.
const NEAREST: usize = 12;

/// The question as a vector, for finding entries by meaning.
struct Meaning {
    model: String,
    vector: Vec<f32>,
}

/// Most captures given with a question that names no time (#193): text as
/// it was on the screen, found by word, for what the journal only sums up.
const MAX_CAPTURES: usize = 8;
/// About this many characters of each capture, around its first match.
const CAPTURE_CHARS: usize = 600;
/// Captures found by word, before those with the same text are left out.
const CAPTURE_CANDIDATES: usize = 3 * MAX_CAPTURES;
/// Most characters of a capture's window title.
const MAX_TITLE_CHARS: usize = 120;
/// Every source the model can be given: the entries, then the captures.
const MAX_SOURCES: usize = MAX_ENTRIES + MAX_CAPTURES;

/// Most named entities whose facts are given.
const MAX_KNOWN_ENTITIES: usize = 5;
/// Most parts whose facts a named project brings along (ADR 0013).
const PARTS_PER_PROJECT: usize = 3;
/// Entries given when nothing in the question narrows it down.
const RECENT: usize = 40;
/// Room for a model that reasons before answering, as for the journal.
const MAX_TOKENS: u32 = 2000;
const MAX_QUESTION_CHARS: usize = 1000;
const MAX_ANSWER_CHARS: usize = 4000;

const TASK: &str = "Answer the user's question using only the captured blocks: what the \
memory knows about the user and about people, organisations and projects, and journal entries. \
Each journal entry starts with its number in brackets, then when it happened and in which apps. \
Answer in the language of the question, even when the entries are in another language, and \
only about what the question asks: leave out entries about other things, and when an entry \
covers several things, use only the part about the question's subject. Make it easy to read: \
for several things, a short sentence that sums up, then each thing as a point of a few words on a \
line of its own starting with \"- \" (\\n before each in the JSON string), at most five, \
putting together what belongs together and leaving out repeats. For one thing, or a length the \
question asks for such as one sentence, only that. Asked what \
is left to do, answer from what the entries say is planned, promised or not done yet. Put the numbers of the journal entries the answer is based on in \
sources, as a list of numbers such as [1, 4] (what the memory knows has no number). Don't put \
entry numbers in the answer. If the blocks don't answer the question, say so plainly; don't \
guess. A block may hold the conversation so far: use it only to understand what a follow-up \
question refers to, and answer from the memory and the journal entries. A numbered block that \
says \"Captured text\" is not a journal entry but part of what was on the screen then, in that \
app and window: something the user saw or read, not necessarily did or wrote. Use it, and its \
number, like an entry.";
/// The answer's form from TASK, again right before the question, where a
/// small model follows it.
const FORM: &str =
    "for several things, a sentence that sums up and then \"- \" points of a few words.";
const SHAPE: &str = r#"{"answer": string, "sources": [number]}"#;
/// Low, so an answer keeps the form asked for every time, as a draft does
/// (`write::TEMPERATURE`).
const TEMPERATURE: f32 = 0.1;

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
/// in lists ([[5]]). Anything else, such as a sentence about the entries,
/// counts as no sources: the answer is still good, and its own "[1]"
/// references are used instead (`references`). Whether the numbers were
/// given is checked afterwards.
fn sources<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<u32>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    let mut out = Vec::new();
    if collect(&value, 0, &mut out).is_none() {
        return Ok(Vec::new());
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
                if to < from || to - from > MAX_SOURCES as u32 {
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
        if self.sources.len() > MAX_SOURCES {
            return Err("too many sources");
        }
        Ok(())
    }
}

/// The entries a question is about, and how they were found.
#[derive(Debug)]
struct Found {
    episodes: Vec<Episode>,
    /// Captured text the question's words find, oldest first; only for a
    /// question that names no time. Numbered after the entries.
    captures: Vec<CapturedText>,
    /// What the memory's pages say about the user and about the entities
    /// the question names, a block each.
    known: Vec<String>,
    /// Nothing in the question narrowed it down, so these are the latest.
    recent_only: bool,
    /// The question named a time.
    timed: bool,
}

/// Finds the entries `question` is about, oldest first, within the budget.
#[cfg(test)]
fn gather<Tz: TimeZone>(
    store: &Store,
    question: &str,
    meaning: Option<&Meaning>,
    now: &DateTime<Tz>,
) -> crate::store::Result<Found>
where
    Tz::Offset: std::fmt::Display,
{
    gather_after(store, question, &[], meaning, now)
}

/// Like [`gather`], for a question that may follow up on `earlier` ones
/// (latest last). A question with no names or words of its own ("och vad
/// mer?", "vad sa hon?") follows up: it takes the latest earlier time, and
/// the names and words of the question before it. One with its own (a new
/// subject) keeps only its own time, but still adds the names before it.
fn gather_after<Tz: TimeZone>(
    store: &Store,
    question: &str,
    earlier: &[&str],
    meaning: Option<&Meaning>,
    now: &DateTime<Tz>,
) -> crate::store::Result<Found>
where
    Tz::Offset: std::fmt::Display,
{
    let asked = words(question);
    let before = earlier.last().map(|q| words(q)).unwrap_or_default();
    let names = store.entity_names()?;
    let mut entities = named_entities(&asked, &names);
    let mut search = search_words(&asked);
    let follows_up = entities.is_empty() && search.is_empty();
    let range = time_range(question, now).or_else(|| {
        if !follows_up {
            return None;
        }
        earlier
            .iter()
            .rev()
            .find_map(|earlier| time_range(earlier, now))
    });
    entities.extend(named_entities(&before, &names));
    entities.sort_unstable();
    entities.dedup();
    if follows_up {
        search = search_words(&before);
    }

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
            let direct = in_turns([mentioning, matching]);
            if (!entities.is_empty() || !search.is_empty()) && !direct.is_empty() {
                // A time and a subject that was found ("Vitas the last two
                // days"): only the entries about it, not everything else from
                // that time, and not the nearest by meaning either.
                direct
            } else {
                // Only a time, or a subject the words didn't find: everything
                // from that time, the nearest by meaning first.
                let mut in_range = store.episodes_between(from, to, 500)?;
                in_range.reverse(); // Latest first, so the budget keeps the end of the time.
                if near.is_empty() {
                    in_range
                } else {
                    let wanted: HashSet<i64> = near.iter().map(|e| e.id).collect();
                    let (first, rest): (Vec<Episode>, Vec<Episode>) =
                        in_range.into_iter().partition(|e| wanted.contains(&e.id));
                    first.into_iter().chain(rest).collect()
                }
            }
        }
        None => {
            let both = in_turns([mentioning, matching, near]);
            if both.is_empty() {
                recent_only = true;
                store.episodes(None, RECENT)?
            } else {
                both
            }
        }
    };

    // What the memory knows comes first, and counts against the budget, but
    // for at most half of it: entries always have room.
    let known = known(store, &entities)?;
    let mut seen = HashSet::new();
    let known_chars: usize = known.iter().map(|k| k.chars().count()).sum();
    let mut chars = known_chars.min(MAX_CHARS / 2);
    let tz = now.timezone();
    // Then captured text, for a question that names no time, in at most a
    // third of the budget; entries fill the rest.
    let captures = match range {
        Some(_) => Vec::new(),
        None => captured(store, &search, &tz)?,
    };
    chars += captures
        .iter()
        .map(|c| capture_text(MAX_SOURCES, c, &tz).chars().count())
        .sum::<usize>();
    let mut episodes = Vec::new();
    for episode in candidates {
        if episodes.len() >= MAX_ENTRIES {
            break;
        }
        if !seen.insert(episode.id) {
            continue;
        }
        // As the model will see it, with its number, time and apps.
        let length = entry_text(MAX_ENTRIES, &episode, &tz).chars().count();
        if chars + length > MAX_CHARS {
            // A shorter one further down may still fit.
            continue;
        }
        chars += length;
        episodes.push(episode);
    }
    episodes.sort_by_key(|e| (e.started_at, e.id));
    Ok(Found {
        known,
        episodes,
        captures,
        recent_only,
        timed: range.is_some(),
    })
}

/// The lists in turns: the first of each, then the second of each, and so
/// on, so the budget cuts from all of them alike.
fn in_turns<const N: usize>(lists: [Vec<Episode>; N]) -> Vec<Episode> {
    let mut lists = lists.map(Vec::into_iter);
    let mut out = Vec::new();
    loop {
        let before = out.len();
        out.extend(lists.iter_mut().filter_map(Iterator::next));
        if out.len() == before {
            return out;
        }
    }
}

/// A capture as the model is given it: where and when, and the part of its
/// text around the first word the question matched. All of it, the window
/// title and app name too, is captured text (GUARDRAILS §5).
#[derive(Debug)]
struct CapturedText {
    id: i64,
    captured_at: DateTime<Utc>,
    app_name: String,
    window_title: String,
    excerpt: String,
}

/// Words about knowing, seeing or reading, not searched for in captures:
/// they say how the user came by something, not what it was ("vad har jag
/// läst om SQLCipher?", "what do I know about SQLCipher?").
const QUESTION_VERBS: &[&str] = &[
    "läst", "läste", "läsa", "läser", "sett", "såg", "tittade", "tittat", "kollade", "kollat",
    "skrev", "skrivit", "vet", "veta", "visste", "minns", "ihåg", "kommer", "lärde", "lärt",
    "read", "reading", "seen", "saw", "looked", "watched", "wrote", "written", "know", "knew",
    "remember", "learn", "learned", "learnt",
];

/// The captures `words` find ([`Store::search`]), at most `MAX_CAPTURES` in
/// a third of the budget, each cut to the part around its match, oldest
/// first like the entries. Those with every word come first, best first;
/// then, with room left, those with one of them, the word with the fewest
/// matches first, so a word every page has comes last. Only what the store
/// still has: captures expired or forgotten are gone, and excluded apps and
/// private windows were never captured.
fn captured<Tz: TimeZone>(
    store: &Store,
    words: &[String],
    tz: &Tz,
) -> crate::store::Result<Vec<CapturedText>>
where
    Tz::Offset: std::fmt::Display,
{
    let words: Vec<String> = words
        .iter()
        .filter(|w| !QUESTION_VERBS.contains(&w.as_str()))
        .cloned()
        .collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let mut found = store.search(&words.join(" "), CAPTURE_CANDIDATES)?;
    if found.len() < MAX_CAPTURES && words.len() > 1 {
        let mut each = words
            .iter()
            .map(|word| store.search(word, CAPTURE_CANDIDATES))
            .collect::<crate::store::Result<Vec<_>>>()?;
        each.sort_by_key(Vec::len);
        found.extend(each.into_iter().flatten());
    }
    let mut given: Vec<CapturedText> = Vec::new();
    let mut ids = HashSet::new();
    let mut seen = HashSet::new();
    let mut chars = 0;
    for capture in found {
        if given.len() >= MAX_CAPTURES {
            break;
        }
        if !ids.insert(capture.id) {
            continue;
        }
        let text = CapturedText {
            id: capture.id,
            captured_at: capture.captured_at,
            app_name: one_line(&capture.app_name, MAX_TITLE_CHARS),
            window_title: one_line(&capture.window_title, MAX_TITLE_CHARS),
            excerpt: excerpt(&capture.text, &words),
        };
        // The same page, captured again, says nothing new.
        let key = if text.excerpt.is_empty() {
            &text.window_title
        } else {
            &text.excerpt
        };
        if !seen.insert(key.clone()) {
            continue;
        }
        let length = capture_text(MAX_SOURCES, &text, tz).chars().count();
        if chars + length > MAX_CHARS / 3 {
            // A shorter one further down may still fit.
            continue;
        }
        chars += length;
        given.push(text);
    }
    given.sort_by_key(|c| (c.captured_at, c.id));
    Ok(given)
}

/// `text` on one line, without control or invisible characters, at most
/// `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let line: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|&c| !c.is_control() && !structured::is_invisible(c))
        .collect();
    crate::capture::truncate_chars(&line, max)
}

/// About `CAPTURE_CHARS` of `text`, on one line, around the first word that
/// starts with one of `words`, as the search matched it, and cut between
/// words. From the start when the match was only in the window title.
fn excerpt(text: &str, words: &[String]) -> String {
    let chars: Vec<char> = one_line(text, usize::MAX).chars().collect();
    if chars.len() <= CAPTURE_CHARS {
        return chars.into_iter().collect();
    }
    let starts_word =
        |i: usize| chars[i].is_alphanumeric() && (i == 0 || !chars[i - 1].is_alphanumeric());
    let at = (0..chars.len())
        .find(|&i| {
            starts_word(i) && {
                let word: String = chars[i..]
                    .iter()
                    .take_while(|c| c.is_alphanumeric())
                    .collect::<String>()
                    .to_lowercase();
                words.iter().any(|w| word.starts_with(w.as_str()))
            }
        })
        .unwrap_or(0);
    // A little before the match, so its sentence is there.
    let mut start = at
        .saturating_sub(CAPTURE_CHARS / 4)
        .min(chars.len() - CAPTURE_CHARS);
    let mut end = start + CAPTURE_CHARS;
    if start > 0 && chars[start - 1] != ' ' {
        start = chars[start..at]
            .iter()
            .position(|&c| c == ' ')
            .map_or(at, |space| start + space + 1);
    }
    if end < chars.len()
        && chars[end] != ' '
        && let Some(space) = chars[at..end].iter().rposition(|&c| c == ' ')
    {
        end = at + space;
    }
    let middle: String = chars[start..end].iter().collect();
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(middle.trim());
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// One capture as the model sees it: "[13] Captured text, Tue 2026-10-06
/// 14:05 (Safari, window "Example docs"): …".
fn capture_text<Tz: TimeZone>(number: usize, capture: &CapturedText, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let at = capture.captured_at.with_timezone(tz);
    let window = if capture.window_title.is_empty() {
        String::new()
    } else {
        format!(", window \"{}\"", capture.window_title)
    };
    format!(
        "[{number}] Captured text, {} ({}{window}): {}",
        at.format("%a %Y-%m-%d %H:%M"),
        capture.app_name,
        capture.excerpt
    )
}

/// What the memory knows that bears on `text` (the writing assistant's
/// instruction): `about-me.md`, and the pages of the entities named in it.
/// Also the names of those pages, "you" for about-me, for the user to see.
pub(crate) fn known_for(
    store: &Store,
    text: &str,
) -> crate::store::Result<(Vec<String>, Vec<String>)> {
    let named = named_entities(&words(text), &store.entity_names()?);
    let (known, mut pages) = known_with_names(store, &named)?;
    if store
        .about_me_page()?
        .and_then(|p| p.facts)
        .is_some_and(|facts| !facts.is_empty())
    {
        pages.insert(0, "you".to_string());
    }
    Ok((known, pages))
}

/// The facts on the pages of the named entities, and on `about-me.md`.
fn known(store: &Store, entities: &[i64]) -> crate::store::Result<Vec<String>> {
    Ok(known_with_names(store, entities)?.0)
}

/// [`known`], and the names of the entities whose facts it gives. A named
/// project brings up to `PARTS_PER_PROJECT` of its parts' facts too, the most
/// mentioned first, but only in the room left of half of `MAX_CHARS` after
/// the user's facts and the named entities' (which always come along, as
/// before), so parts never make the context bigger than it was. A named part
/// says which project it is part of; its whole's page isn't added for that
/// (ADR 0013).
fn known_with_names(
    store: &Store,
    entities: &[i64],
) -> crate::store::Result<(Vec<String>, Vec<String>)> {
    let mut known = Vec::new();
    let mut names = Vec::new();
    if let Some(facts) = store.about_me_page()?.and_then(|p| p.facts)
        && !facts.is_empty()
    {
        known.push(format!(
            "What the memory knows about the user:\n{}",
            bullets(&facts)
        ));
    }
    // Most mentioned first.
    let all = store.entities()?;
    let block = |entity: &Entity| -> crate::store::Result<Option<String>> {
        let Some(facts) = store.page_of(entity.id)?.and_then(|p| p.facts) else {
            return Ok(None);
        };
        if facts.is_empty() {
            return Ok(None);
        }
        let kind = match entity.kind {
            EntityKind::Person => "person",
            EntityKind::Organisation => "organisation",
            EntityKind::Project => "project",
        };
        let whole = entity
            .part_of
            .and_then(|w| all.iter().find(|e| e.id == w))
            .map(|w| format!(", part of {}", w.name))
            .unwrap_or_default();
        Ok(Some(format!(
            "What the memory knows about {} ({kind}{whole}):\n{}",
            entity.name,
            bullets(&facts)
        )))
    };
    let named: Vec<&Entity> = entities
        .iter()
        .take(MAX_KNOWN_ENTITIES)
        .filter_map(|id| all.iter().find(|e| e.id == *id))
        .collect();
    let mut given: HashSet<i64> = HashSet::new();
    for entity in &named {
        given.insert(entity.id);
        if let Some(text) = block(entity)? {
            known.push(text);
            names.push(entity.name.clone());
        }
    }
    let used: usize = known.iter().map(|k| k.chars().count()).sum();
    let mut room = (MAX_CHARS / 2).saturating_sub(used);
    for entity in &named {
        let parts = all
            .iter()
            .filter(|e| e.part_of == Some(entity.id))
            .take(PARTS_PER_PROJECT);
        for part in parts {
            if !given.insert(part.id) {
                continue;
            }
            if let Some(text) = block(part)?
                && text.chars().count() <= room
            {
                room -= text.chars().count();
                known.push(text);
                names.push(part.name.clone());
            }
        }
    }
    Ok((known, names))
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
        // Short words are left out, but not "q3"; a bare small number ("2
        // dagarna") is a count, not a subject.
        .filter(|w| {
            w.chars().count() >= 3
                || (w.chars().any(|c| c.is_ascii_digit()) && !w.chars().all(|c| c.is_ascii_digit()))
        })
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
    "hitta",
    "hittar",
    "hittade",
    "info",
    // Follow-up words: "och vad mer?", "vad sa hon?"
    "mer",
    "annat",
    "mera",
    "sen",
    "sedan",
    "innan",
    "efter",
    "hon",
    "han",
    "hen",
    "dem",
    "dom",
    "sa",
    "sade",
    "också",
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
    "find",
    "anything",
    "something",
    "information",
    // Follow-up words: "and what else?", "what did she say?"
    "else",
    "more",
    "then",
    "before",
    "after",
    "she",
    "her",
    "him",
    "they",
    "them",
    "say",
    "said",
    "also",
];

/// A small count, in digits or words, Swedish or English: "2", "två", "two".
fn count_word(word: &str) -> Option<i64> {
    if let Ok(n) = word.parse::<i64>() {
        return Some(n);
    }
    let n = match word {
        "en" | "ett" | "one" => 1,
        "två" | "two" => 2,
        "tre" | "three" => 3,
        "fyra" | "four" => 4,
        "fem" | "five" => 5,
        "sex" | "six" => 6,
        "sju" | "seven" => 7,
        "tio" | "ten" => 10,
        "fjorton" | "fourteen" => 14,
        _ => return None,
    };
    Some(n)
}

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
    "senaste",
    "dagarna",
    "dagar",
    "dygnen",
    "dygn",
    "days",
    "past",
    "few",
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
    // "De senaste 2 dagarna", "senaste tre dagarna", "the last 3 days",
    // "past two days": today and the days before it. "Senaste dagarna" and
    // "the last few days" without a number mean three.
    for (i, word) in asked.iter().enumerate() {
        if !matches!(word.as_str(), "senaste" | "last" | "past") {
            continue;
        }
        let next = asked.get(i + 1).map(String::as_str);
        let (count, unit) = match next.and_then(count_word) {
            Some(count) => (count, asked.get(i + 2).map(String::as_str)),
            None if next == Some("few") => (3, asked.get(i + 2).map(String::as_str)),
            None => (3, next),
        };
        if matches!(unit, Some("dagarna" | "dagar" | "dygnen" | "dygn" | "days")) {
            let count = count.clamp(1, 90);
            found.push(span(today - TimeDelta::days(count - 1), count));
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
    // A small model otherwise answers in the entries' language. A language
    // the question names wins.
    let language = crate::write::question_language(question)
        .filter(|_| !crate::write::names_a_language(question))
        .map(|language| format!("\n\nAnswer in {language}."))
        .unwrap_or_default();
    format!(
        "Today is {}.\n\nForm: {FORM}\n\nQuestion: {question}{language}",
        now.format("%A %Y-%m-%d, %H:%M")
    )
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AskAnswer {
    pub answer: String,
    /// The entries and captures the answer says it is based on.
    pub sources: Vec<AskSource>,
    /// How many entries the model was given.
    pub considered: usize,
    /// Nothing in the question narrowed it down, so the model was given
    /// the latest entries.
    pub recent_only: bool,
}

/// A source of an answer: a journal entry, or a capture, which the window
/// shows by its app, window title and time, and opens under Captures.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AskSource {
    Entry(EpisodeItem),
    Capture(CaptureSource),
}

/// A capture an answer is based on. The app name and window title are
/// captured text, shown as plain text only.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSource {
    pub id: i64,
    pub captured_at: i64,
    pub app_name: String,
    pub window_title: String,
}

/// The sources the model named, by their numbers (entries 1..n, captures
/// after them), or `None` if any number wasn't given.
fn cited(found: &Found, numbers: &[u32]) -> Option<Vec<AskSource>> {
    let mut numbers: Vec<usize> = numbers.iter().map(|&n| n as usize).collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
        .into_iter()
        .map(|n| {
            let i = n.checked_sub(1)?;
            match found.episodes.get(i) {
                Some(episode) => Some(AskSource::Entry(item(episode))),
                None => found.captures.get(i - found.episodes.len()).map(|c| {
                    AskSource::Capture(CaptureSource {
                        id: c.id,
                        captured_at: c.captured_at.timestamp_millis(),
                        app_name: c.app_name.clone(),
                        window_title: c.window_title.clone(),
                    })
                }),
            }
        })
        .collect()
}

/// An earlier question and its answer in the same conversation, from the
/// window. Only held there; never stored.
#[derive(Debug, Deserialize)]
pub struct Earlier {
    question: String,
    answer: String,
}

/// At most this many earlier turns, and characters of them, go along. The
/// window says so after as many questions (`REMEMBERED_TURNS` in
/// src/Ask.tsx), and offers a new conversation.
const MAX_EARLIER_TURNS: usize = 4;
const MAX_EARLIER_CHARS: usize = 3000;

/// The conversation so far as one block for the model, latest turns kept
/// when it is too long; `None` if there is none. Earlier answers came from
/// the model and captured text, so the block goes in as captured data.
fn conversation(earlier: &[Earlier]) -> Option<String> {
    let mut turns: Vec<String> = Vec::new();
    let mut chars = 0;
    for turn in earlier.iter().rev().take(MAX_EARLIER_TURNS) {
        let text = format!(
            "The user asked: {}\nLax answered: {}",
            crate::capture::truncate_chars(turn.question.trim(), MAX_QUESTION_CHARS),
            crate::capture::truncate_chars(turn.answer.trim(), MAX_ANSWER_CHARS),
        );
        chars += text.chars().count();
        if chars > MAX_EARLIER_CHARS && !turns.is_empty() {
            break;
        }
        turns.push(text);
    }
    if turns.is_empty() {
        return None;
    }
    turns.reverse();
    Some(format!(
        "The conversation so far, oldest first:\n\n{}",
        turns.join("\n\n")
    ))
}

/// Answers `question` from the journal with the chosen chat model, as a
/// follow-up to `earlier` turns of the same conversation (oldest first).
///
/// While the model writes, the answer's text so far goes to `on_text`, in
/// pieces, as plain text for the window that asked (GUARDRAILS §5). It is
/// not validated yet: the window shows it only until this returns, and then
/// shows the validated answer this returns, or the error.
#[tauri::command]
pub async fn ask_memory(
    app: AppHandle,
    question: String,
    earlier: Option<Vec<Earlier>>,
    on_text: Channel<String>,
) -> Result<AskAnswer, String> {
    let earlier = earlier.unwrap_or_default();
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
        (choice, {
            let before: Vec<&str> = earlier
                .iter()
                .rev()
                .take(MAX_EARLIER_TURNS)
                .rev()
                .map(|turn| turn.question.as_str())
                .collect();
            gather_after(&store, &question, &before, meaning.as_ref(), &now).map_err(failed)?
        })
    };
    // A time with nothing in it is answered as such: what the memory knows
    // in general says nothing about that day.
    if found.episodes.is_empty()
        && found.captures.is_empty()
        && (found.timed || found.known.is_empty())
    {
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
    let started = Instant::now();
    let texts = blocks(&found, &earlier, &Local);
    let request = request(&question, &now);
    // The window may lose interest (closed); the answer is still validated.
    let mut streamed = Streamed::new(started, |text| {
        let _ = on_text.send(text);
    });
    let asked = answer(endpoint, &choice.model, &request, &texts, |piece| {
        streamed.push(piece)
    });
    let Some(reply) = app.state::<Pause>().unless_paused(asked).await else {
        return Err("Lax was paused, so the question was dropped.".to_string());
    };
    let (first_words, shown) = (streamed.first, streamed.chars);
    let reply = reply.and_then(|mut reply| {
        // No usable sources: the references in the answer's own text.
        if reply.sources.is_empty() {
            reply.sources = references(&reply.answer);
        }
        let sources = cited(&found, &reply.sources)
            .ok_or(StructuredError::Invalid("a source that wasn't given"))?;
        Ok((reply, sources))
    });
    // Whether the server answered, for the journal's status.
    match &reply {
        Err(error) if error.model_unavailable() => app
            .state::<ModelHealth>()
            .down(Utc::now().timestamp_millis()),
        _ => app.state::<ModelHealth>().up(),
    }
    match reply {
        Ok((reply, sources)) => {
            let cited_captures = sources
                .iter()
                .filter(|s| matches!(s, AskSource::Capture(_)))
                .count();
            let mut fields = vec![
                Field::Count("entries", found.episodes.len() as u64),
                Field::Count("captures", found.captures.len() as u64),
                Field::Count("cited", sources.len() as u64),
                Field::Count("cited_captures", cited_captures as u64),
                Field::Flag("recent_only", found.recent_only),
                Field::Duration("elapsed", started.elapsed()),
            ];
            if let Some(first) = first_words {
                fields.push(Field::Duration("first_words", first));
            }
            logging::info("ask.answered", &fields);
            Ok(AskAnswer {
                answer: without_references(&reply.answer),
                sources,
                considered: found.episodes.len(),
                recent_only: found.recent_only,
            })
        }
        Err(error) => {
            logging::warn(
                "ask.failed",
                &[
                    Field::Flag("model_unavailable", error.model_unavailable()),
                    // How much the window showed before it was taken back.
                    Field::Count("streamed_chars", shown as u64),
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

/// The model's reply, validated, with `on_text` getting the raw output as
/// it is written.
async fn answer(
    endpoint: Endpoint,
    model: &str,
    request: &str,
    texts: &[String],
    on_text: impl FnMut(&str),
) -> Result<Reply, StructuredError> {
    // The user waits for this: no reasoning, which makes a small local model
    // about ten times faster.
    let client = ModelClient::new(endpoint)
        .map_err(StructuredError::Model)?
        .without_reasoning()
        .with_temperature(TEMPERATURE);
    let captured: Vec<Captured> = texts.iter().map(|t| Captured(t.as_str())).collect();
    structured::ask_streaming(
        &client, model, TASK, SHAPE, request, &captured, MAX_TOKENS, on_text,
    )
    .await
}

/// The answer's text as the model writes it, for the window: only what is
/// in the `answer` field (never the sources or anything around them), as
/// the validated answer will read (`without_references`, trimmed), at most
/// `MAX_ANSWER_CHARS`. Not validated, so it is only ever shown as plain text
/// and taken back unless the reply passes (GUARDRAILS §5). Never stored or
/// logged; only its length is.
struct Streamed<F: FnMut(String)> {
    field: FieldText,
    send: F,
    started: Instant,
    /// The field's text so far, as the model wrote it.
    written: String,
    /// What has been sent: always the start of what is shown next.
    sent: String,
    /// When the first words were sent.
    first: Option<Duration>,
    /// Characters sent so far.
    chars: usize,
}

impl<F: FnMut(String)> Streamed<F> {
    fn new(started: Instant, send: F) -> Self {
        Streamed {
            field: FieldText::new("answer"),
            send,
            started,
            written: String::new(),
            sent: String::new(),
            first: None,
            chars: 0,
        }
    }

    /// Takes the next piece of the model's raw output.
    fn push(&mut self, piece: &str) {
        let text = self.field.push(piece);
        // Enough to fill the longest answer, even after references go.
        if text.is_empty() || self.written.len() > 4 * MAX_ANSWER_CHARS {
            return;
        }
        self.written.push_str(&text);
        // What may still change is held back: a "[" that may yet turn out
        // to be a reference, and spaces, which go before a reference and
        // at the end of a line.
        let settled = match self.written.rfind('[') {
            Some(open) if is_reference(&self.written[open + 1..]) => &self.written[..open],
            _ => self.written.as_str(),
        };
        let shown = without_references(settled.trim_end());
        let shown = crate::capture::truncate_chars(&shown, MAX_ANSWER_CHARS);
        // Always the case, but if it weren't, nothing more is shown rather
        // than something that doesn't follow on.
        let Some(new) = shown.strip_prefix(self.sent.as_str()) else {
            return;
        };
        if new.is_empty() {
            return;
        }
        let new = new.to_string();
        self.chars += new.chars().count();
        self.sent.push_str(&new);
        self.first.get_or_insert_with(|| self.started.elapsed());
        (self.send)(new);
    }
}

/// Whether `text` after a "[" can still be, or be the start of, an entry
/// reference such as "3", "1, 4" or "2-5".
fn is_reference(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_ascii_digit() || matches!(c, ',' | '-' | '–' | ' '))
}

/// The captured blocks for the model, most stable first, so that a
/// server's prompt cache can reuse the start of the prompt for the next
/// question: what the memory knows about the user (the same for every
/// question), what it knows about the entities named, the journal entries,
/// the captures, numbered after the entries, and the conversation so far,
/// which grows with every turn. The question
/// and today's date and time go after all of them, as the request
/// ([`structured::ask_streaming`]).
fn blocks<Tz: TimeZone>(found: &Found, earlier: &[Earlier], tz: &Tz) -> Vec<String>
where
    Tz::Offset: std::fmt::Display,
{
    found
        .known
        .iter()
        .cloned()
        .chain(
            found
                .episodes
                .iter()
                .enumerate()
                .map(|(i, e)| entry_text(i + 1, e, tz)),
        )
        .chain(
            found
                .captures
                .iter()
                .enumerate()
                .map(|(i, c)| capture_text(found.episodes.len() + i + 1, c, tz)),
        )
        .chain(conversation(earlier))
        .collect()
}

fn with_store<T>(app: &AppHandle, work: impl FnOnce(&Store) -> T) -> Option<T> {
    let store = app.try_state::<Mutex<Store>>()?;
    let store = store.lock().unwrap_or_else(PoisonError::into_inner);
    Some(work(&store))
}

/// The answer without entry references such as "[3]" or "[1-8]", which
/// models put in although sources are shown on their own.
/// The entry numbers an answer refers to in its text ("[1]", "[2, 4]",
/// "[3-5]"), the same references `without_references` takes out.
fn references(answer: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut rest = answer;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let inside = &after[..close];
        if inside.chars().any(|c| c.is_ascii_digit())
            && inside
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, ',' | '-' | '–' | ' '))
        {
            let mut found = Vec::new();
            if numbers(inside, &mut found).is_some() {
                out.extend(found);
            }
        }
        rest = after;
    }
    out.sort_unstable();
    out.dedup();
    out.truncate(MAX_SOURCES);
    out
}

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
    fn the_last_few_days() {
        let range = |q: &str| day(time_range(q, &now()));
        // Today, Sunday the 11th, and the day before.
        assert_eq!(range("Vitas senaste 2 dagarna?"), on("2026-10-10", 2));
        assert_eq!(range("de senaste tre dagarna"), on("2026-10-09", 3));
        assert_eq!(range("the last 3 days"), on("2026-10-09", 3));
        assert_eq!(range("past two days"), on("2026-10-10", 2));
        assert_eq!(range("de senaste dagarna"), on("2026-10-09", 3));
        assert_eq!(range("the last few days"), on("2026-10-09", 3));
        // "Senaste" alone, or a count of something else, is no time.
        assert_eq!(range("senaste mejlet från Anna"), None);
        assert_eq!(range("the last 3 mails"), None);
    }

    #[test]
    fn filler_words_are_not_searched_for() {
        assert_eq!(
            search_words(&words("Kan du hitta något om Vitas senaste 2 dagarna?")),
            ["vitas"]
        );
        assert_eq!(
            search_words(&words(
                "Can you find anything about Vitas in the last 2 days?"
            )),
            ["vitas"]
        );
    }

    #[test]
    fn a_time_and_a_subject_give_only_the_subject() {
        let found = gather(
            &sample(),
            "Vad gjorde jag med Anna i tisdags?",
            None,
            &now(),
        )
        .unwrap();
        // Tuesday also has "Läste nyheter.", which isn't about Anna.
        assert_eq!(summaries(&found), ["Mailade Anna om Q3-offerten."]);
        // A subject the words don't find gives the whole time instead.
        let found = gather(
            &sample(),
            "Vad gjorde jag med kunden i tisdags?",
            None,
            &now(),
        )
        .unwrap();
        assert_eq!(
            summaries(&found),
            ["Mailade Anna om Q3-offerten.", "Läste nyheter."]
        );
        let found = gather(&sample(), "Vad gjorde jag i tisdags?", None, &now()).unwrap();
        assert_eq!(
            summaries(&found),
            ["Mailade Anna om Q3-offerten.", "Läste nyheter."]
        );
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
    fn a_named_project_brings_its_parts_and_a_part_names_its_whole() {
        let mut store = Store::open_in_memory().unwrap();
        let projects = ["Exempelappen", "Export", "Kryptering", "Ikoner", "Sökning"];
        // Mentioned more the earlier in the list, so the order is known.
        for (i, name) in projects.iter().enumerate() {
            for hour in 0..(10 - i as u32) {
                let id = episode(
                    &mut store,
                    at(6, 8 + hour),
                    &format!("Arbetade med {name}."),
                );
                store
                    .record_mentions(id, &[(EntityKind::Project, name.to_string())], at(6, 20))
                    .unwrap();
            }
        }
        let ids: Vec<i64> = projects
            .iter()
            .map(|name| {
                store
                    .entities()
                    .unwrap()
                    .into_iter()
                    .find(|e| e.name == *name)
                    .unwrap()
                    .id
            })
            .collect();
        for &id in &ids {
            store.keep_entity(id).unwrap();
        }
        for &part in &ids[1..] {
            store.set_part_of(part, ids[0]).unwrap();
        }
        store.set_memory_enabled(true).unwrap();
        store.set_pages_in_database().unwrap();
        crate::pages::sync_rows(&mut store, &HashSet::new(), at(6, 21), &stockholm()).unwrap();
        for (&id, name) in ids.iter().zip(projects) {
            let page = store.page_of(id).unwrap().unwrap();
            store
                .set_facts(&page.id, &[format!("Om {name}.")], &[], at(6, 21))
                .unwrap();
        }

        // The whole brings its three most mentioned parts.
        let (known, names) = known_for(&store, "Hur går det med Exempelappen?").unwrap();
        assert_eq!(names, ["Exempelappen", "Export", "Kryptering", "Ikoner"]);
        assert_eq!(
            known[1],
            "What the memory knows about Export (project, part of Exempelappen):\n- Om Export."
        );
        // A part says which project it is part of, without the whole's page.
        let (known, names) = known_for(&store, "Hur går det med Sökning?").unwrap();
        assert_eq!(names, ["Sökning"]);
        assert_eq!(
            known,
            ["What the memory knows about Sökning (project, part of Exempelappen):\n- Om Sökning."]
        );
    }

    #[test]
    fn parts_only_use_the_room_left() {
        let mut store = Store::open_in_memory().unwrap();
        let names = ["Exempelappen", "Export"];
        for (i, name) in names.iter().enumerate() {
            for hour in 0..(5 - i as u32) {
                let id = episode(
                    &mut store,
                    at(6, 8 + hour),
                    &format!("Arbetade med {name}."),
                );
                store
                    .record_mentions(id, &[(EntityKind::Project, name.to_string())], at(6, 20))
                    .unwrap();
            }
        }
        let all = store.entities().unwrap();
        let (app, export) = (all[0].id, all[1].id);
        store.keep_entity(app).unwrap();
        store.keep_entity(export).unwrap();
        store.set_part_of(export, app).unwrap();
        store.set_memory_enabled(true).unwrap();
        store.set_pages_in_database().unwrap();
        crate::pages::sync_rows(&mut store, &HashSet::new(), at(6, 21), &stockholm()).unwrap();
        // The whole's page takes most of the room: the part's doesn't fit.
        let long: Vec<String> = (0..12)
            .map(|i| format!("{i}: {}", "x".repeat(180)))
            .collect();
        let page = store.page_of(app).unwrap().unwrap();
        store.set_facts(&page.id, &long, &[], at(6, 21)).unwrap();
        let page = store.page_of(export).unwrap().unwrap();
        store.set_facts(&page.id, &long, &[], at(6, 21)).unwrap();
        let (known, names) = known_for(&store, "Exempelappen").unwrap();
        assert_eq!(names, ["Exempelappen"]);
        let chars: usize = known.iter().map(|k| k.chars().count()).sum();
        assert!(chars <= MAX_CHARS / 2, "{chars}");
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
            .map(|e| entry_text(MAX_ENTRIES, e, &stockholm()).chars().count())
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
        // Counted as the model sees them, with number, time and apps.
        let chars: usize = found
            .episodes
            .iter()
            .enumerate()
            .map(|(i, e)| entry_text(i + 1, e, &stockholm()).chars().count())
            .sum();
        assert!(chars <= MAX_CHARS);
        assert!(found.episodes.len() < 24);
        // The end of the day is kept.
        assert_eq!(found.episodes.last().unwrap().started_at, at(10, 23));
    }

    #[test]
    fn what_the_memory_knows_leaves_room_for_entries() {
        let mut store = sample();
        let anna = store.entity_names().unwrap()[0].0;
        store.keep_entity(anna).unwrap();
        let folder = std::env::temp_dir().join(format!(
            "lax-ask-room-test-{}",
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
        let sources: Vec<i64> = store
            .episodes(None, 10)
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect();
        // Two full pages: as many facts as the memory writes, as long as
        // they may be: without the cap, no entry would fit after them.
        let facts: Vec<String> = (0..12)
            .map(|i| format!("Fakta {i:02}: {}", "x".repeat(190)))
            .collect();
        for page in [
            store.page_of(anna).unwrap().unwrap().id,
            store.about_me_page().unwrap().unwrap().id,
        ] {
            store
                .set_facts(&page, &facts, &sources, at(11, 12))
                .unwrap();
        }
        let found = gather(&store, "Vad gjorde jag med Anna igår?", None, &now()).unwrap();
        let known: usize = found.known.iter().map(|k| k.chars().count()).sum();
        assert!(known + 60 > MAX_CHARS, "{known}");
        // Yesterday's entry about Anna is still given.
        assert_eq!(summaries(&found), ["Ringde Anna om leveransen."]);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn entries_found_by_meaning_take_turns_with_the_others() {
        let mut store = Store::open_in_memory().unwrap();
        // Many entries with the word, more than the budget holds…
        for hour in 0..20 {
            episode(
                &mut store,
                at(2, hour),
                &format!("Lanseringen {}", "x ".repeat(150)),
            );
        }
        // …and one that is about it only in meaning.
        // As long as the others, so it can't just squeeze in at the end.
        let close = episode(
            &mut store,
            at(1, 9),
            &format!("Planerade releasen med Bo. {}", "y ".repeat(150)),
        );
        let all = store.episodes(None, 30).unwrap();
        let vectors: Vec<(i64, Vec<f32>)> = all
            .iter()
            .map(|e| {
                (
                    e.id,
                    if e.id == close {
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
            vector: vec![1.0, 0.0],
        };
        let found = gather(&store, "Hur går lanseringen?", Some(&meaning), &now()).unwrap();
        assert!(found.episodes.len() < 21);
        assert!(
            summaries(&found)
                .iter()
                .any(|s| s.starts_with("Planerade releasen"))
        );
    }

    #[test]
    fn lists_take_turns() {
        let store = sample();
        let all = store.episodes(None, 10).unwrap();
        let ids = |list: &[Episode]| list.iter().map(|e| e.id).collect::<Vec<_>>();
        let (a, b) = (all[..3].to_vec(), all[3..].to_vec());
        let mixed = in_turns([a.clone(), b.clone(), Vec::new()]);
        assert_eq!(ids(&mixed), [a[0].id, b[0].id, a[1].id, a[2].id]);
        assert!(in_turns::<2>([Vec::new(), Vec::new()]).is_empty());
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
    fn references_in_the_answer_are_read() {
        assert_eq!(
            references("Mailade Anna [1].\n- Planerade [2, 4]."),
            [1, 2, 4]
        );
        assert_eq!(references("Hela veckan [3-5], och [3]."), [3, 4, 5]);
        // Not references: a link, a placeholder, a range too long to be real.
        assert!(references("Se [länken] och [date]").is_empty());
        assert!(references("[1-100000]").is_empty());
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
        // Not numbers, or a range too long to be real, gives no sources, and
        // the answer stays: its own "[1]" references are used instead. A
        // sentence instead of numbers came from the maintainer's real data.
        let none = Some(Vec::new());
        assert_eq!(sources(r#"{"answer":"ok","sources":"all of them"}"#), none);
        assert_eq!(
            sources(r#"{"answer":"ok","sources":["entries about the launch on 2026-10-13"]}"#),
            none
        );
        assert_eq!(sources(r#"{"answer":"ok","sources":"1-100000"}"#), none);
        assert_eq!(sources(r#"{"answer":"ok","sources":"5-2"}"#), none);
        assert_eq!(sources(r#"{"answer":"ok","sources":[true]}"#), none);
        assert_eq!(sources(r#"{"answer":"ok","sources":[[[[1]]]]}"#), none);
        assert_eq!(sources(r#"{"answer":"ok","sources":[-1]}"#), none);
        // The shape itself still matters.
        assert!(parse(r#"{"answer":"ok"}"#).is_err());
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
        assert_eq!(
            sources(r#"{"answer":"ok","sources":[1.5]}"#),
            Some(Vec::new())
        );
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
        // The question's language goes along when it is clear, unless the
        // question names one.
        assert!(
            request("What did I do for Example AB on Wednesday?", &now())
                .ends_with("\n\nAnswer in English.")
        );
        assert!(
            request("Vad har jag gjort för Example AB i veckan?", &now())
                .ends_with("\n\nAnswer in Swedish.")
        );
        assert!(
            request(
                "Kan du svara på engelska vad jag gjorde för Example AB?",
                &now()
            )
            .ends_with("Example AB?")
        );
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
        let mut shown = String::new();
        let mut streamed = Streamed::new(Instant::now(), |text| shown.push_str(&text));
        let reply = answer(
            Endpoint::parse(&env("LAX_LIVE_MODEL_URL")).unwrap(),
            &env("LAX_LIVE_CHAT_MODEL"),
            &request("Vad pratade jag med Anna om i tisdags?", &now()),
            &texts,
            |piece| streamed.push(piece),
        )
        .await
        .expect("an answer");
        assert!(streamed.first.is_some());
        drop(streamed);
        // What was shown as it was written is the answer itself.
        assert_eq!(shown, without_references(&reply.answer));
        assert!(reply.answer.to_lowercase().contains("offert"));
        assert!(reply.sources.contains(&1), "{reply:?}");
    }

    /// Synthetic entries that overlap, like a week of work for one client.
    fn overlapping_entries() -> Vec<String> {
        [
            "[1] Mon 2026-10-05 09:00–10:10 (Pages, Safari): Arbetade med Example AB:s processkarta för ledning och kärnverksamhet inför onboardingen; flödesschemat gick inte att flytta.",
            "[2] Mon 2026-10-05 13:00–14:00 (Safari): Granskade Example AB:s onboardingdokument och AI-genererade flödesscheman i Nyterio.",
            "[3] Tue 2026-10-06 10:00–11:30 (Pages): Gick igenom att Example AB:s onboardingdokumentation var klar, med tidslinje och projektsteg.",
            "[4] Tue 2026-10-06 15:00–16:00 (Pages, Mail): Uppdaterade flödesschemat 'Utföra tillverkning' för Example AB och läste nya onboardingdokument från Anna Example.",
            "[5] Wed 2026-10-07 09:30–11:00 (Pages): Detaljerade huvudprocesskartan för Example AB: Ledning, Reklamation, Lager och Ekonomi; saknad dokumentation noterad.",
            "[6] Wed 2026-10-07 14:00–15:00 (Claude, Pages): Korrigerade onboardingprocessen och projektets milstolpar för Example AB; testade också AI-modellernas hastighet för Lax.",
            "[7] Thu 2026-10-08 10:00–12:00 (Claude, Terminal): Arbetade med Lax skrivassistent och mergade ändringar; skrev kort till Anna Example att processkartan skickas för godkännande på fredag.",
        ]
        .map(String::from)
        .to_vec()
    }

    /// Answers over overlapping entries read well: a sentence first, then a
    /// few points; one sentence when asked for; the question's language.
    /// Synthetic data only.
    #[tokio::test]
    #[ignore = "needs a local model server: LAX_LIVE_MODEL_URL, LAX_LIVE_CHAT_MODEL"]
    async fn live_answers_read_well() {
        let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        for question in [
            "Vad har jag gjort för Example AB den här veckan?",
            "Kan du sammanfatta i en mening vad jag har kvar för Example AB?",
            "What did I do for Example AB on Wednesday?",
        ] {
            let started = Instant::now();
            let reply = answer(
                Endpoint::parse(&env("LAX_LIVE_MODEL_URL")).unwrap(),
                &env("LAX_LIVE_CHAT_MODEL"),
                &request(question, &now()),
                &overlapping_entries(),
                |_| {},
            )
            .await
            .expect("an answer");
            let answer = &reply.answer;
            assert!(started.elapsed().as_secs() < 30, "slow: {question}");
            assert!(
                !answer.starts_with("- "),
                "no summing-up sentence: {answer}"
            );
            if question.contains("en mening") {
                assert!(
                    !answer.contains("\n- "),
                    "points after one sentence: {answer}"
                );
                assert!(answer.contains("fredag"), "not what is planned: {answer}");
            } else if question.starts_with("What") {
                assert!(
                    !answer.contains(['å', 'ä', 'ö']),
                    "an English question got a Swedish answer: {answer}"
                );
            } else {
                let points = answer.matches("\n- ").count();
                assert!((2..=6).contains(&points), "{points} points: {answer}");
            }
        }
    }

    #[test]
    fn a_follow_up_takes_the_time_and_names_of_the_question_before() {
        let store = sample();
        // "And what else?" has no time or words of its own.
        let found = gather_after(
            &store,
            "Och vad mer?",
            &["Vad gjorde jag i tisdags?"],
            None,
            &now(),
        )
        .unwrap();
        assert!(found.timed);
        assert_eq!(
            summaries(&found),
            ["Mailade Anna om Q3-offerten.", "Läste nyheter."]
        );
        let alone = gather(&store, "Och vad mer?", None, &now()).unwrap();
        assert!(!alone.timed);
        // A time of its own wins over the earlier one.
        let own = gather_after(
            &store,
            "Vad gjorde jag i fredags?",
            &["Vad gjorde jag i tisdags?"],
            None,
            &now(),
        )
        .unwrap();
        assert_eq!(summaries(&own), ["Planerade lanseringen med Bo."]);
        // A new subject doesn't take the earlier time.
        let new = gather_after(
            &store,
            "Vad vet du om Bo?",
            &["Vad gjorde jag i tisdags?"],
            None,
            &now(),
        )
        .unwrap();
        assert!(!new.timed);
        // The names of the question before come along: "she" is Anna.
        let her = gather_after(&store, "Vad sa hon?", &["Vad gör Anna?"], None, &now()).unwrap();
        assert!(!her.recent_only);
        assert!(summaries(&her).contains(&"Ringde Anna om leveransen."));
    }

    #[test]
    fn only_the_answer_s_text_is_streamed() {
        let mut sent: Vec<String> = Vec::new();
        let mut streamed = Streamed::new(Instant::now(), |text| sent.push(text));
        for piece in [
            "```json\n{\"sources\": [1, 2], ",
            "\"answer\": \"  ",
            "\\nDu mail",
            "ade Anna\\",
            "\"Example\\",
            "\".\", \"sour",
            "ces\": [3]}\n```",
        ] {
            streamed.push(piece);
        }
        let (first, chars) = (streamed.first, streamed.chars);
        drop(streamed);
        // From the first visible character, as the validated answer will be
        // trimmed; never the sources, the keys or the fence.
        assert_eq!(sent, ["Du mail", "ade Anna", "\"Example", "\"."]);
        assert_eq!(chars, "Du mailade Anna\"Example\".".chars().count());
        assert!(first.is_some());
    }

    #[test]
    fn the_streamed_text_reads_as_the_validated_answer_will() {
        // Entry references and spaces at line ends go, as `without_references`
        // takes them out of the validated answer, so nothing jumps when it
        // takes the streamed text's place.
        let answer = "Du mailade Anna [1] och Bo [2-3].\n- Rad två  \n- [x] tre";
        let pieces = [
            "{\"answer\": \"Du mailade Anna [",
            "1",
            "] och Bo [2-",
            "3].\\n- Rad två  ",
            "\\n- [",
            "x] tre\"}",
        ];
        let mut sent: Vec<String> = Vec::new();
        let mut streamed = Streamed::new(Instant::now(), |text| sent.push(text));
        for piece in pieces {
            streamed.push(piece);
        }
        drop(streamed);
        assert_eq!(sent.concat(), without_references(answer));
        assert_eq!(
            sent.concat(),
            "Du mailade Anna och Bo.\n- Rad två\n- [x] tre"
        );
        assert!(sent.iter().all(|s| !s.contains("[1") && !s.contains("[2")));
        // Cut anywhere, it comes out the same.
        let reply = format!("{{\"answer\": \"{}\"}}", answer.replace('\n', "\\n"));
        for (i, _) in reply.char_indices() {
            let mut shown = String::new();
            let mut streamed = Streamed::new(Instant::now(), |text| shown.push_str(&text));
            streamed.push(&reply[..i]);
            streamed.push(&reply[i..]);
            drop(streamed);
            assert_eq!(shown, without_references(answer), "cut at {i}");
        }
    }

    #[test]
    fn a_streamed_answer_stops_at_the_longest_answer() {
        let mut shown = String::new();
        let mut streamed = Streamed::new(Instant::now(), |text| shown.push_str(&text));
        streamed.push("{\"answer\": \"");
        for _ in 0..50 {
            streamed.push(&"ä".repeat(100));
        }
        drop(streamed);
        assert_eq!(shown.chars().count(), MAX_ANSWER_CHARS);
    }

    #[test]
    fn nothing_is_streamed_from_a_reply_without_an_answer() {
        let mut sent: Vec<String> = Vec::new();
        let mut streamed = Streamed::new(Instant::now(), |text| sent.push(text));
        streamed.push("I can't answer that. {\"sources\": []}");
        streamed.push("{\"answer\": \"too late\"}");
        assert!(streamed.first.is_none());
        drop(streamed);
        assert!(sent.is_empty());
    }

    #[test]
    fn only_the_main_window_can_ask() {
        // The streamed answer goes back through the caller's own channel, so
        // only a window allowed to call `ask_memory` can receive it.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        let mut found = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let capability: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let allows_ask = capability["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == "allow-ask-memory");
            if allows_ask {
                assert_eq!(
                    capability["windows"],
                    serde_json::json!(["main"]),
                    "{}",
                    path.display()
                );
                found += 1;
            }
        }
        assert_eq!(found, 1);
    }

    #[test]
    fn only_ask_streams() {
        // GUARDRAILS §5: only Ask may show model output before it is
        // validated. The journal, the memory, page facts and the writing
        // assistant validate first, so they never ask for a stream.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let allowed = [
            src.join("ask.rs"),
            src.join("models").join("client.rs"),
            src.join("models").join("structured.rs"),
        ];
        let mut checked = 0;
        let mut dirs = vec![src.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") && !allowed.contains(&path) {
                    checked += 1;
                    let source = std::fs::read_to_string(&path).unwrap();
                    for name in ["ask_streaming", "chat_stream"] {
                        assert!(!source.contains(name), "{} uses {name}", path.display());
                    }
                }
            }
        }
        // The journal, memory, facts and writing assistant were among them.
        assert!(checked > 20);
    }

    #[test]
    fn questions_and_follow_ups_share_the_start_of_the_prompt() {
        let store = sample();
        let known = vec![
            "What the memory knows about the user:\n- Jobbar med lanseringar.".to_string(),
            "What the memory knows about Anna (person):\n- Leder Q3-lanseringen.".to_string(),
        ];
        let prompt = |found: &Found, question: &str, earlier: &[Earlier], minutes: i64| {
            let texts = blocks(found, earlier, &stockholm());
            let captured: Vec<Captured> = texts.iter().map(|t| Captured(t.as_str())).collect();
            let now = now() + TimeDelta::minutes(minutes);
            structured::messages(
                TASK,
                SHAPE,
                Some(structured::Request::Last(&request(question, &now))),
                &captured,
            )
        };
        let turn = |question: &str, answer: &str| Earlier {
            question: question.to_string(),
            answer: answer.to_string(),
        };

        // A question, then follow-ups with no words of their own, which
        // gather what the question before them did.
        let asked = "Vad gjorde jag med Anna?";
        let mut found = gather(&store, asked, None, &now()).unwrap();
        found.known = known.clone();
        let [system, first] = prompt(&found, asked, &[], 0);
        let mut follow_up = gather_after(&store, "Och vad mer?", &[asked], None, &now()).unwrap();
        follow_up.known = known.clone();
        let ids = |found: &Found| found.episodes.iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids(&follow_up), ids(&found));
        let earlier = [turn(asked, "Du mailade henne om offerten.")];
        let [same_system, second] = prompt(&follow_up, "Och vad mer?", &earlier, 3);
        let longer = [
            turn(asked, "Du mailade henne om offerten."),
            turn("Och vad mer?", "Du ringde henne."),
        ];
        let [_, third] = prompt(&follow_up, "Vad sa hon?", &longer, 5);
        let [_, other] = prompt(&follow_up, "Och sen då?", &longer, 5);

        // The system message is the same, byte for byte: no date or count.
        assert_eq!(system.content, same_system.content);
        // The first question's prompt up to its request is the start of the
        // follow-ups' prompts: the memory, then the entries.
        let stable = &first.content[..first.content.find("Request from the user:").unwrap()];
        assert!(stable.starts_with("<captured>\nWhat the memory knows about the user:"));
        assert!(stable.contains("Ringde Anna om leveransen."));
        assert!(second.content.starts_with(stable));
        assert!(third.content.starts_with(stable));
        // The conversation grows at its end, so a later follow-up also
        // starts with the one before it, up to that one's request.
        let before = &second.content[..second.content.find("Request from the user:").unwrap()];
        assert!(
            third
                .content
                .starts_with(before.trim_end_matches("\n</captured>\n\n"))
        );
        // Two different questions after the same conversation differ only at
        // the very end, in the question.
        let shared = third
            .content
            .char_indices()
            .zip(other.content.chars())
            .find(|((_, a), b)| a != b)
            .map(|((i, _), _)| i)
            .unwrap();
        assert!(third.content[..shared].ends_with("\n\nQuestion: "));
        // The conversation comes after the entries, and the request last.
        let conversation = second.content.find("The conversation so far").unwrap();
        assert!(conversation > second.content.find("Ringde Anna").unwrap());
        assert!(second.content.find("Request from the user:").unwrap() > conversation);
        assert!(
            second
                .content
                .ends_with("Question: Och vad mer?\n\nAnswer in Swedish.")
        );
    }

    fn capture(store: &mut Store, when: DateTime<Utc>, title: &str, text: &str) -> i64 {
        store
            .insert(&NewCapture {
                captured_at: when,
                bundle_id: "com.example.browser",
                app_name: "Example Browser",
                window_title: title,
                site: Some("example.com"),
                text,
            })
            .unwrap()
            .unwrap()
    }

    /// A page about SQLCipher, far into a long text.
    fn page_about_sqlcipher(n: usize) -> String {
        format!(
            "{} Page {n}. SQLCipher encrypts the whole database file with a key from the \
             keychain. {}",
            "Filler words before it. ".repeat(80),
            "More words after it. ".repeat(80)
        )
    }

    #[test]
    fn a_question_without_a_time_gets_captures_after_the_entries() {
        let mut store = sample();
        capture(
            &mut store,
            at(8, 10),
            "SQLCipher – Example docs",
            &page_about_sqlcipher(1),
        );
        capture(
            &mut store,
            at(9, 16),
            "Notes",
            "Anna Example asked whether SQLCipher is slower.",
        );
        capture(&mut store, at(9, 17), "Other", "Nothing about databases.");
        let question = "Vad har jag läst om SQLCipher?";
        let found = gather(&store, question, None, &now()).unwrap();
        // "läst" is how, not what: the captures with the word are found.
        assert_eq!(found.captures.len(), 2);
        // Oldest first, the long page cut around its match, between words.
        let page = &found.captures[0];
        assert_eq!(page.window_title, "SQLCipher – Example docs");
        assert!(
            page.excerpt
                .contains("SQLCipher encrypts the whole database")
        );
        assert!(page.excerpt.starts_with("…Filler") || page.excerpt.starts_with("…words"));
        assert!(page.excerpt.ends_with("…"));
        assert!(page.excerpt.chars().count() <= CAPTURE_CHARS + 2);
        assert_eq!(
            found.captures[1].excerpt,
            "Anna Example asked whether SQLCipher is slower."
        );
        // No capture has every word: those with one of them, the rarest
        // word's first. "databases" is on one page, "sqlcipher" on two.
        let some = gather(
            &store,
            "Hur fungerar SQLCipher med databases?",
            None,
            &now(),
        )
        .unwrap();
        let titles: Vec<&str> = some
            .captures
            .iter()
            .map(|c| c.window_title.as_str())
            .collect();
        assert_eq!(titles, ["SQLCipher – Example docs", "Notes", "Other"]);
        let one = gather(&store, "Hur fungerar SQLCipher?", None, &now()).unwrap();
        assert_eq!(one.captures.len(), 2);

        // Numbered after the entries, as captured blocks with app, title and time.
        let texts = blocks(&found, &[], &stockholm());
        let n = found.episodes.len();
        let entries: Vec<&String> = texts
            .iter()
            .filter(|t| !t.contains("Captured text"))
            .collect();
        assert_eq!(entries.len(), n + found.known.len());
        assert!(texts.contains(&format!(
            "[{}] Captured text, Fri 2026-10-09 16:00 (Example Browser, window \"Notes\"): \
             Anna Example asked whether SQLCipher is slower.",
            n + 2
        )));
        assert!(texts.iter().any(|t| t.starts_with(&format!(
            "[{}] Captured text, Thu 2026-10-08 10:00 (Example Browser, window \"SQLCipher – Example docs\"): …",
            n + 1
        ))));
    }

    #[test]
    fn captures_take_at_most_a_third_of_the_budget() {
        let mut store = sample();
        for i in 0..20 {
            capture(
                &mut store,
                at(7, 0) + TimeDelta::minutes(i),
                &format!("Example docs {i}"),
                &page_about_sqlcipher(i as usize),
            );
        }
        let found = gather(&store, "What do I know about SQLCipher?", None, &now()).unwrap();
        assert!(!found.captures.is_empty());
        assert!(found.captures.len() <= MAX_CAPTURES);
        let numbered = blocks(&found, &[], &stockholm());
        let captures: usize = numbered
            .iter()
            .filter(|t| t.contains("] Captured text, "))
            .map(|t| t.chars().count())
            .sum();
        assert!(captures <= MAX_CHARS / 3, "{captures}");
        // Everything given still fits the budget.
        let all: usize = numbered.iter().map(|t| t.chars().count()).sum();
        assert!(all <= MAX_CHARS, "{all}");
        // Short captures: no more than the most.
        let mut store = sample();
        for i in 0..20 {
            capture(
                &mut store,
                at(7, 0) + TimeDelta::minutes(i),
                "Docs",
                &format!("SQLCipher note {i}."),
            );
        }
        let found = gather(&store, "SQLCipher", None, &now()).unwrap();
        assert_eq!(found.captures.len(), MAX_CAPTURES);
    }

    #[test]
    fn a_time_question_gets_no_captures() {
        let mut store = sample();
        capture(
            &mut store,
            at(10, 10),
            "Notes",
            "SQLCipher and Anna Example.",
        );
        for question in [
            "Vad läste jag om SQLCipher igår?",
            "What did I read about SQLCipher yesterday?",
        ] {
            let found = gather(&store, question, None, &now()).unwrap();
            assert!(found.timed);
            assert!(found.captures.is_empty(), "{question}");
        }
        // A follow-up that takes the time before it gets none either.
        let found = gather_after(
            &store,
            "Och vad mer?",
            &["Vad läste jag om SQLCipher igår?"],
            None,
            &now(),
        )
        .unwrap();
        assert!(found.timed && found.captures.is_empty());
        // Only words about reading find nothing.
        let found = gather(&store, "Vad har jag läst?", None, &now()).unwrap();
        assert!(found.captures.is_empty());
    }

    #[test]
    fn a_capture_cited_is_a_capture_source() {
        let mut store = sample();
        let id = capture(
            &mut store,
            at(9, 16),
            "Notes\u{202E} \u{7}about",
            "Anna Example asked whether SQLCipher is slower.",
        );
        let found = gather(&store, "Vad vet jag om SQLCipher?", None, &now()).unwrap();
        let n = found.episodes.len() as u32;
        assert_eq!(found.captures.len(), 1);
        let sources = cited(&found, &[n + 1]).unwrap();
        assert_eq!(
            sources,
            [AskSource::Capture(CaptureSource {
                id,
                captured_at: at(9, 16).timestamp_millis(),
                app_name: "Example Browser".to_string(),
                // Control and invisible characters left out.
                window_title: "Notes about".to_string(),
            })]
        );
        let json = serde_json::to_value(&sources[0]).unwrap();
        assert_eq!(json["kind"], "capture");
        assert_eq!(json["windowTitle"], "Notes about");
        // An entry is an entry, before the captures.
        if n > 0 {
            let both = cited(&found, &[n + 1, 1]).unwrap();
            assert!(matches!(both[0], AskSource::Entry(_)));
            assert!(matches!(both[1], AskSource::Capture(_)));
            assert_eq!(serde_json::to_value(&both[0]).unwrap()["kind"], "entry");
        }
        // A number not given is still refused.
        assert!(cited(&found, &[n + 2]).is_none());
        assert!(cited(&found, &[0]).is_none());
        assert!(cited(&found, &[1, n + 2]).is_none());
        // Without captures, the numbers after the entries weren't given.
        let timed = gather(&store, "Vad läste jag om SQLCipher igår?", None, &now()).unwrap();
        let after = timed.episodes.len() as u32 + 1;
        assert!(cited(&timed, &[after]).is_none());
    }

    #[test]
    fn excerpts_are_around_the_match_between_words() {
        let words = vec!["sqlcipher".to_string()];
        // Short text: all of it, on one line.
        assert_eq!(excerpt("Om\n\nSQLCipher.", &words), "Om SQLCipher.");
        // The match far in: the part around it.
        let text = one_line(&page_about_sqlcipher(1), usize::MAX);
        let cut = excerpt(&text, &words);
        assert!(cut.contains("SQLCipher encrypts"));
        let inner = cut.trim_start_matches('…').trim_end_matches('…');
        assert!(text.contains(inner));
        // Cut between words: the text right before and after each cut is a space.
        let at = text.find(inner).unwrap();
        assert_eq!(&text[at - 1..at], " ");
        assert_eq!(&text[at + inner.len()..at + inner.len() + 1], " ");
        // A prefix matches, as in the search; no match in the text: the start.
        assert!(excerpt(&text, &["sqlciph".to_string()]).contains("SQLCipher"));
        let start = excerpt(&text, &["nowhere".to_string()]);
        assert!(start.starts_with("Filler words before it."));
        assert!(start.ends_with('…'));
        // A match near the end keeps a whole excerpt.
        let late = format!("{} SQLCipher.", "Words. ".repeat(200));
        let cut = excerpt(&late, &words);
        assert!(cut.ends_with("SQLCipher.") && cut.chars().count() > CAPTURE_CHARS / 2);
    }

    /// Against a real model server: a question about something read gets
    /// an answer from captured text, and cites the capture. Synthetic data.
    #[tokio::test]
    #[ignore = "needs a local model server: LAX_LIVE_MODEL_URL, LAX_LIVE_CHAT_MODEL"]
    async fn live_answer_from_captures() {
        let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let mut store = sample();
        capture(
            &mut store,
            at(8, 10),
            "SQLCipher – Example docs",
            &page_about_sqlcipher(1),
        );
        capture(
            &mut store,
            at(9, 16),
            "Example forum",
            "Anna Example wrote that SQLCipher costs about 5 percent in speed.",
        );
        let question = "Vad har jag läst om SQLCipher?";
        let found = gather(&store, question, None, &now()).unwrap();
        assert_eq!(found.captures.len(), 2);
        let texts = blocks(&found, &[], &stockholm());
        let reply = answer(
            Endpoint::parse(&env("LAX_LIVE_MODEL_URL")).unwrap(),
            &env("LAX_LIVE_CHAT_MODEL"),
            &request(question, &now()),
            &texts,
            |_| {},
        )
        .await
        .expect("an answer");
        let sources = cited(&found, &reply.sources).expect("sources that were given");
        assert!(
            sources.iter().any(|s| matches!(s, AskSource::Capture(_))),
            "no capture cited: {reply:?}"
        );
        let text = reply.answer.to_lowercase();
        assert!(
            text.contains("nyckel") || text.contains("krypter") || text.contains("5"),
            "{}",
            reply.answer
        );
    }

    #[test]
    fn the_conversation_goes_along_latest_first_within_limits() {
        assert_eq!(conversation(&[]), None);
        let turn = |q: &str, a: &str| Earlier {
            question: q.to_string(),
            answer: a.to_string(),
        };
        let block = conversation(&[turn("Vad gjorde jag igår?", "Du skrev offerten.")]).unwrap();
        assert!(block.contains("The user asked: Vad gjorde jag igår?"));
        assert!(block.contains("Lax answered: Du skrev offerten."));
        // Only the latest four turns.
        let many: Vec<Earlier> = (0..6)
            .map(|i| turn(&format!("Fråga {i}"), "Svar"))
            .collect();
        let block = conversation(&many).unwrap();
        assert!(
            !block.contains("Fråga 1\n") && block.contains("Fråga 2") && block.contains("Fråga 5")
        );
        // Long turns: the latest is kept, older ones dropped.
        let long = "x".repeat(2500);
        let block = conversation(&[turn("Gammal", &long), turn("Ny", &long)]).unwrap();
        assert!(block.contains("Ny") && !block.contains("Gammal"));
    }
}
