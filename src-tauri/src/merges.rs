//! Merge suggestions judged by the local model (ADR 0013, point 3). Pairs
//! come from the store, found by their names alone ([`Store::pair_candidates`]);
//! the model reads the entries that mention each name, and the facts on its
//! page, and says whether they are the same. A "yes" becomes a suggestion in
//! the Memory view; the user merges or says "not the same".
//!
//! The names, entries and facts come from captured text, so they go to the
//! model only as [`Captured`] data, and the reason it gives back is untrusted
//! text: cleaned, capped, shown as text only and never logged. Injected text
//! in an entry can at worst flip a verdict; the pair itself comes from Lax,
//! and a merge always needs the user's click.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::journal::{When, may_use_model};
use crate::logging::{self, ErrorKind, Field};
use crate::models::structured::{self, Captured, StructuredError, Validate, is_invisible};
use crate::models::{Endpoint, ModelClient, ModelTurn};
use crate::pause::Pause;
use crate::store::{Candidate, EntityKind, ModelTask, Store, Suggestion, Verdict};

/// Pairs judged per round: each is a model call while the user is away.
const PAIRS_PER_ROUND: usize = 2;
/// Entries and page facts given per name.
const ENTRIES_PER_NAME: usize = 3;
const FACTS_PER_NAME: usize = 10;
/// The longest reason shown, in characters.
const MAX_REASON_CHARS: usize = 200;
/// Room for a model that reasons before answering, as for the memory.
const MAX_TOKENS: u32 = 2000;
/// Bad answers about one pair before it counts as "unsure", so it waits
/// instead of taking the first turn every round.
const ATTEMPTS_PER_PAIR: u32 = 3;

/// Pairs the model answered badly about, and how often. Only in memory.
#[derive(Default)]
pub struct Setbacks {
    failures: HashMap<(i64, i64), u32>,
}

impl Setbacks {
    /// Counts a bad answer; `true` once the pair has had
    /// `ATTEMPTS_PER_PAIR`, and then it is no longer counted.
    fn bad_answer(&mut self, pair: (i64, i64)) -> bool {
        let count = self.failures.entry(pair).or_insert(0);
        *count += 1;
        if *count >= ATTEMPTS_PER_PAIR {
            self.failures.remove(&pair);
            return true;
        }
        false
    }
}

const TASK: &str = "Decide whether two names in the user's memory are the same person, \
organisation or project, of the kind given. You get each name with entries from the user's \
journal that mention it, and facts from its page if it has one. Answer \"yes\" only when they \
make it clear: one name is a short form, a longer form, a misspelling or an inflection of the \
other, they tell of the same role, work or place, and nothing in them speaks against it. Sharing a \
first name or a word, or being mentioned near similar topics, is not enough. Answer \"no\" when \
they show the two are different, for instance different roles, companies, places or kinds of \
work. Answer \"unsure\" when they don't tell. The reason is one short sentence.";
const SHAPE: &str = r#"{"same": "yes" | "no" | "unsure", "reason": string}"#;

/// The model's answer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    same: Same,
    reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Same {
    Yes,
    No,
    Unsure,
}

impl Validate for Answer {
    fn validate(&self) -> Result<(), &'static str> {
        // Cut to size below; far longer than asked is no answer.
        if self.reason.chars().count() > 5 * MAX_REASON_CHARS {
            return Err("the reason was far too long");
        }
        Ok(())
    }
}

/// The reason as shown: control and invisible characters out, spaces
/// collapsed, at most `MAX_REASON_CHARS`.
fn clean(reason: &str) -> String {
    let kept: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !is_invisible(*c))
        .collect();
    let words = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    match words.char_indices().nth(MAX_REASON_CHARS) {
        Some((cut, _)) => format!("{}…", words[..cut].trim_end()),
        None => words,
    }
}

/// A pair to judge, with what the model reads about each name.
struct Work {
    base_url: String,
    model: String,
    candidate: Candidate,
    about_a: String,
    about_b: String,
}

/// One round: judge up to `PAIRS_PER_ROUND` pairs, only while the user is
/// away and on power (`may_use_model(When::Later)`), and not while paused.
/// `Err` means the model server is unavailable.
pub async fn round(app: &AppHandle, setbacks: &mut Setbacks) -> Result<(), ()> {
    if app.state::<Pause>().is_paused() || !may_use_model(When::Later) {
        return Ok(());
    }
    let work = with_store(app, gather)
        .and_then(Result::ok)
        .unwrap_or_default();
    for work in work {
        if !may_use_model(When::Later) {
            break;
        }
        let started = Instant::now();
        let Some(answer) = app
            .state::<ModelTurn>()
            .background(&app.state::<Pause>(), judge(&work))
            .await
        else {
            break;
        };
        let (a, b) = (&work.candidate.a, &work.candidate.b);
        match answer {
            Ok(answer) => {
                setbacks.failures.remove(&(a.id, b.id));
                let verdict = match answer.same {
                    Same::Yes => Verdict::Yes,
                    Same::No => Verdict::No,
                    Same::Unsure => Verdict::Unsure,
                };
                let stored = with_store(app, |store| {
                    store.record_verdict(
                        (a.id, a.mentions),
                        (b.id, b.mentions),
                        verdict,
                        &clean(&answer.reason),
                        Utc::now(),
                    )
                });
                logging::info(
                    "merges.judged",
                    &[
                        Field::Flag("same", verdict == Verdict::Yes),
                        Field::Flag("stored", matches!(stored, Some(Ok(true)))),
                        Field::Duration("elapsed", started.elapsed()),
                    ],
                );
            }
            Err(error) => {
                logging::warn(
                    "merges.failed",
                    &[
                        Field::Flag("model_unavailable", error.model_unavailable()),
                        Field::Error(ErrorKind::Other),
                    ],
                );
                if error.model_unavailable() {
                    return Err(());
                }
                // Asked again next round, and after a few bad answers it
                // waits like an "unsure".
                if setbacks.bad_answer((a.id, b.id)) {
                    let _ = with_store(app, |store| {
                        store.record_verdict(
                            (a.id, a.mentions),
                            (b.id, b.mentions),
                            Verdict::Unsure,
                            "",
                            Utc::now(),
                        )
                    });
                }
            }
        }
    }
    Ok(())
}

/// The pairs to judge this round, if the memory is on and has a model.
fn gather(store: &mut Store) -> crate::store::Result<Vec<Work>> {
    if !store.memory_enabled()? {
        return Ok(Vec::new());
    }
    let Some(choice) = store.model_choice(ModelTask::Chat)? else {
        return Ok(Vec::new());
    };
    let mut work = Vec::new();
    for candidate in store
        .pair_candidates(Utc::now())?
        .into_iter()
        .take(PAIRS_PER_ROUND)
    {
        work.push(Work {
            base_url: choice.base_url.clone(),
            model: choice.model.clone(),
            about_a: about(store, candidate.kind, candidate.a.id, &candidate.a.name)?,
            about_b: about(store, candidate.kind, candidate.b.id, &candidate.b.name)?,
            candidate,
        });
    }
    Ok(work)
}

/// What the model reads about one name: its kind, newest entries and the
/// facts on its page.
fn about(store: &Store, kind: EntityKind, id: i64, name: &str) -> crate::store::Result<String> {
    let kind = match kind {
        EntityKind::Person => "person",
        EntityKind::Organisation => "organisation",
        EntityKind::Project => "project",
    };
    let mut text = format!("Name: {name}\nKind: {kind}\nEntries that mention it:\n");
    for episode in store.episodes_mentioning(&[id], None, ENTRIES_PER_NAME)? {
        text.push_str(&format!(
            "- {}\n",
            episode.summary.replace(['\n', '\r'], " ")
        ));
    }
    let facts = store.page_of(id)?.and_then(|p| p.facts).unwrap_or_default();
    if !facts.is_empty() {
        text.push_str("Facts on its page:\n");
        for fact in facts.iter().take(FACTS_PER_NAME) {
            text.push_str(&format!("- {fact}\n"));
        }
    }
    Ok(text)
}

async fn judge(work: &Work) -> Result<Answer, StructuredError> {
    let endpoint =
        Endpoint::parse(&work.base_url).map_err(|_| StructuredError::NotTheRequestedJson)?;
    let client = ModelClient::new(endpoint).map_err(StructuredError::Model)?;
    structured::ask(
        &client,
        &work.model,
        TASK,
        SHAPE,
        &[Captured(&work.about_a), Captured(&work.about_b)],
        MAX_TOKENS,
    )
    .await
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionItem {
    pub kind: &'static str,
    pub into: NameItem,
    pub from: NameItem,
    pub reason: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NameItem {
    pub id: i64,
    pub name: String,
}

impl From<Suggestion> for SuggestionItem {
    fn from(s: Suggestion) -> Self {
        SuggestionItem {
            kind: s.kind.as_str(),
            into: NameItem {
                id: s.into.0,
                name: s.into.1,
            },
            from: NameItem {
                id: s.from.0,
                name: s.from.1,
            },
            reason: s.reason,
        }
    }
}

/// The pairs the model said are the same, for the Memory view.
#[tauri::command(async)]
pub fn merge_suggestions(app: AppHandle) -> Result<Vec<SuggestionItem>, String> {
    with_store(&app, |store| {
        store
            .merge_suggestions()
            .map(|list| list.into_iter().map(SuggestionItem::from).collect())
            .map_err(|_| "Lax couldn't read the memory.".to_string())
    })
    .unwrap_or_else(|| Err("The database isn't available.".to_string()))
}

fn with_store<T>(app: &AppHandle, work: impl FnOnce(&mut Store) -> T) -> Option<T> {
    let store = app.try_state::<Mutex<Store>>()?;
    let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
    Some(work(&mut store))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(json: &str) -> Result<Answer, StructuredError> {
        structured::parse(json)
    }

    #[test]
    fn answers_are_one_of_three_with_a_reason() {
        let yes = answer(r#"{"same":"yes","reason":"Same team."}"#).unwrap();
        assert_eq!((yes.same, yes.reason.as_str()), (Same::Yes, "Same team."));
        assert_eq!(
            answer(r#"{"same":"unsure","reason":""}"#).unwrap().same,
            Same::Unsure
        );
        for reply in [
            r#"{"same":"maybe","reason":"x"}"#,
            r#"{"same":true,"reason":"x"}"#,
            r#"{"same":"yes"}"#,
            r#"{"same":"yes","reason":"x","merge":"now"}"#,
        ] {
            assert_eq!(
                answer(reply).err(),
                Some(StructuredError::NotTheRequestedJson),
                "{reply}"
            );
        }
        let long = format!(r#"{{"same":"yes","reason":"{}"}}"#, "a".repeat(1001));
        assert!(matches!(answer(&long), Err(StructuredError::Invalid(_))));
    }

    #[test]
    fn a_pair_waits_after_three_bad_answers() {
        let mut setbacks = Setbacks::default();
        assert!(!setbacks.bad_answer((1, 2)));
        assert!(!setbacks.bad_answer((1, 2)));
        assert!(setbacks.bad_answer((1, 2)));
        assert!(setbacks.failures.is_empty());
    }

    #[test]
    fn reasons_are_cleaned_and_capped() {
        assert_eq!(
            clean("  Same\n team,\u{0007} same\u{200B} place.  "),
            "Same team, same place."
        );
        let long = clean(&"word ".repeat(100));
        assert!(long.chars().count() <= MAX_REASON_CHARS + 1);
        assert!(long.ends_with('…'));
        assert_eq!(clean("Bara åäö."), "Bara åäö.");
    }

    /// Against a real model server: synthetic pairs that are and aren't the
    /// same. A "yes" for a pair that isn't is the costly mistake.
    #[tokio::test]
    #[ignore = "needs a local model server: LAX_LIVE_MODEL_URL, LAX_LIVE_CHAT_MODEL"]
    async fn live_pair_judge() {
        let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let entity = |name: &str| crate::store::Entity {
            id: 1,
            kind: EntityKind::Person,
            name: name.to_string(),
            aliases: Vec::new(),
            mentions: 1,
            days: 1,
            last_seen: Utc::now(),
            first_seen: Utc::now(),
            kept: false,
        };
        let block = |kind: &str, name: &str, entries: &[&str]| {
            let mut text = format!("Name: {name}\nKind: {kind}\nEntries that mention it:\n");
            for entry in entries {
                text.push_str(&format!("- {entry}\n"));
            }
            text
        };
        type Case<'a> = (
            &'a str,
            &'a str,
            &'a [&'a str],
            &'a str,
            &'a [&'a str],
            Same,
        );
        let cases: [Case; 6] = [
            (
                "person",
                "Anna",
                &["Lunch med Anna om offerten till Exempelbolaget."],
                "Anna Example",
                &[
                    "Mejlade Anna Example offerten till Exempelbolaget.",
                    "Möte med Anna Example om leveransen vecka 46.",
                ],
                Same::Yes,
            ),
            (
                "organisation",
                "Nyterios",
                &["Uppdaterade Nyterios webbplats med nya priser."],
                "Nyterio",
                &["Skrev ett LinkedIn-inlägg för Nyterio om processkartläggning."],
                Same::Yes,
            ),
            (
                "organisation",
                "Example AB",
                &["Skickade offert till Example AB i Göteborg."],
                "Example Oy",
                &["Läste om Example Oy:s nya kontor i Helsingfors."],
                Same::No,
            ),
            (
                "person",
                "Erik Example",
                &["Parprogrammerade med Erik Example på backend-koden."],
                "Erika Example",
                &["Erika Example på ekonomi skickade lönebeskedet."],
                Same::No,
            ),
            (
                "project",
                "Q3",
                &["Stängde bokföringen för Q3."],
                "Q3 lansering",
                &["Planerade Q3 lansering med marknadsavdelningen."],
                Same::No,
            ),
            (
                "organisation",
                "Example",
                &["Läste om Example, ett energibolag som höjer elpriset."],
                "Example Labs",
                &["Testade Example Labs nya AI-modell i ett projekt."],
                Same::No,
            ),
        ];
        let mut wrong = Vec::new();
        for (kind, a, ea, b, eb, want) in cases {
            let work = Work {
                base_url: env("LAX_LIVE_MODEL_URL"),
                model: env("LAX_LIVE_CHAT_MODEL"),
                candidate: Candidate {
                    kind: EntityKind::Person,
                    a: entity(a),
                    b: entity(b),
                    signal: crate::store::Signal::Contains,
                },
                about_a: block(kind, a, ea),
                about_b: block(kind, b, eb),
            };
            let got = judge(&work).await.expect("an answer").same;
            // "unsure" is fine either way; a wrong yes or no is not.
            if got != want && got != Same::Unsure {
                wrong.push(format!("{a} / {b}: {got:?}, expected {want:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}
