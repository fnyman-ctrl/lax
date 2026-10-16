//! Merge suggestions (ADR 0013, point 3): pairs of names that look like the
//! same entity, found by their names alone. The obvious pairs need no model:
//! organisations whose names are equal once a legal suffix and punctuation
//! are taken off ("Example" and "Example AB"). Other pairs ("Anna" and "Anna
//! Example", "Nyterios" and "Nyterio") are candidates the local model judges
//! from the entries that mention them; its "yes" becomes a suggestion. The
//! user's "not the same" is final either way.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, TimeDelta, Utc};
use rusqlite::params;

use super::{Entity, EntityKind, Result, Store, name_key};

/// A "no" or "unsure" is judged again after this long, or once either side
/// has `REJUDGE_FACTOR` times the mentions it had.
const REJUDGE_AFTER_DAYS: i64 = 30;
const REJUDGE_FACTOR: i64 = 2;

/// Why two names may be the same, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Signal {
    /// One name is the other's words with more around them ("Anna" and
    /// "Anna Example", "Lax" and "Lax development").
    Contains,
    /// Organisations equal but for different legal suffixes, or a suffix
    /// next to two others ("Example AB" and "Example Oy"): not obvious.
    LegalSuffix,
    /// A letter or two apart: a typo or an inflection ("Nyterios").
    Typo,
}

/// Two names of one kind that may be the same: the model judges them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: EntityKind,
    pub a: Entity,
    pub b: Entity,
    pub signal: Signal,
}

/// The model's answer on a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Yes,
    No,
    Unsure,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Yes => "yes",
            Verdict::No => "no",
            Verdict::Unsure => "unsure",
        }
    }
}

/// A pair the model said is the same: `from` would be merged into `into`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub kind: EntityKind,
    pub into: (i64, String),
    pub from: (i64, String),
    /// The model's reason: untrusted text, shown as text only.
    pub reason: String,
}

/// Legal suffixes at the end of an organisation's name, lowercase.
const LEGAL_SUFFIXES: &[&str] = &[
    "ab",
    "aktiebolag",
    "hb",
    "kb",
    "inc",
    "ltd",
    "limited",
    "llc",
    "gmbh",
    "oy",
    "oyj",
    "as",
    "asa",
    "aps",
    "a/s",
    "bv",
    "nv",
    "sa",
    "sas",
    "plc",
];

/// An organisation's name without punctuation, case or a legal suffix at
/// the end, and that suffix. The suffix counts only when something is left
/// before it: "AB" alone is a name.
pub fn legal_base(name: &str) -> (String, Option<&'static str>) {
    let lower = name.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_alphanumeric() || c == '/'))
        .filter(|w| !w.is_empty())
        .collect();
    let suffix = match words.as_slice() {
        [_, .., last] => LEGAL_SUFFIXES.iter().find(|s| *s == last).copied(),
        _ => None,
    };
    let base = &words[..words.len() - usize::from(suffix.is_some())];
    (base.join(" ").replace('/', " "), suffix)
}

/// Names that look like one organisation: `from` is merged into `into`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObviousPair {
    pub into: (i64, String),
    pub from: (i64, String),
}

impl Store {
    /// The obvious pairs (ADR 0013): organisations equal once a legal suffix
    /// and punctuation are taken off, with the same suffix or one without.
    /// A group whose names carry two different suffixes ("Example AB" and
    /// "Example Oy") is not obvious: they are often companies in different
    /// countries. In each group the name with a page, else a kept one, else
    /// the most mentioned, is the one the others merge into, so merging them
    /// all never chains. Groups of new names only are left out: most of them
    /// fade. Pairs the user said are not the same are left out.
    pub fn obvious_pairs(&self) -> Result<Vec<ObviousPair>> {
        let with_page: HashSet<i64> = self
            .pages()?
            .into_iter()
            .filter_map(|p| p.entity_id)
            .collect();
        let not_same = self.not_same_pairs()?;
        let mut groups: HashMap<String, Vec<_>> = HashMap::new();
        for entity in self.entities()? {
            if entity.kind != EntityKind::Organisation {
                continue;
            }
            let (base, suffix) = legal_base(&entity.name);
            if !base.is_empty() {
                groups.entry(base).or_default().push((entity, suffix));
            }
        }
        let mut pairs = Vec::new();
        for mut group in groups.into_values() {
            let suffixes: HashSet<_> = group.iter().filter_map(|(_, s)| *s).collect();
            if group.len() < 2 || suffixes.len() > 1 || group.iter().all(|(e, _)| e.is_new()) {
                continue;
            }
            // Most likely to stay first: a page, kept, mentions, then age.
            group.sort_by_key(|(e, _)| {
                (
                    !with_page.contains(&e.id),
                    !e.kept,
                    std::cmp::Reverse(e.mentions),
                    e.id,
                )
            });
            let (into, _) = &group[0];
            for (from, _) in &group[1..] {
                if !not_same.contains(&ordered(into.id, from.id)) {
                    pairs.push(ObviousPair {
                        into: (into.id, into.name.clone()),
                        from: (from.id, from.name.clone()),
                    });
                }
            }
        }
        pairs.sort_by(|a, b| a.into.1.cmp(&b.into.1).then(a.from.1.cmp(&b.from.1)));
        Ok(pairs)
    }

    /// The user said `a` and `b` are not the same: never suggested again.
    /// `false` if either is gone, or they are one entity.
    pub fn mark_not_same(&self, a: i64, b: i64) -> Result<bool> {
        if a == b {
            return Ok(false);
        }
        let (a, b) = ordered(a, b);
        let added = self.conn.execute(
            "INSERT OR IGNORE INTO not_same (a, b)
             SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM entities WHERE id = ?1)
                             AND EXISTS (SELECT 1 FROM entities WHERE id = ?2)",
            params![a, b],
        )?;
        Ok(added > 0
            || self.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM not_same WHERE a = ?1 AND b = ?2)",
                params![a, b],
                |row| row.get(0),
            )?)
    }

    /// Pairs of names that may be the same and need the model's judgment
    /// (ADR 0013): same kind, a name signal ([`Signal`]), at least one side
    /// kept or seen on two days, not an obvious pair, not mostly named in the
    /// same entries (then they are usually two things one text named), not
    /// "not the same", and not judged yet, or judged "no" or "unsure" before
    /// the side's mentions doubled or 30 days passed. Strongest signal first,
    /// then most mentioned.
    pub fn pair_candidates(&self, now: DateTime<Utc>) -> Result<Vec<Candidate>> {
        let entities = self.entities()?;
        let not_same = self.not_same_pairs()?;
        let obvious: HashSet<(i64, i64)> = self
            .obvious_pairs()?
            .into_iter()
            .map(|p| ordered(p.into.0, p.from.0))
            .collect();
        let verdicts = self.verdicts()?;
        let mentions = self.mentions_by_entity()?;
        let mut candidates = Vec::new();
        for (i, a) in entities.iter().enumerate() {
            for b in &entities[i + 1..] {
                if a.kind != b.kind || (a.is_new() && b.is_new()) {
                    continue;
                }
                let key = ordered(a.id, b.id);
                if not_same.contains(&key) || obvious.contains(&key) {
                    continue;
                }
                let Some(signal) = signal(a, b) else {
                    continue;
                };
                if mostly_together(mentions.get(&a.id), mentions.get(&b.id)) {
                    continue;
                }
                let (first, second) = if a.id < b.id { (a, b) } else { (b, a) };
                if let Some(v) = verdicts.get(&key)
                    && !v.due(now, first.mentions as i64, second.mentions as i64)
                {
                    continue;
                }
                candidates.push(Candidate {
                    kind: a.kind,
                    a: first.clone(),
                    b: second.clone(),
                    signal,
                });
            }
        }
        candidates.sort_by_key(|c| (c.signal, std::cmp::Reverse(c.a.mentions + c.b.mentions)));
        Ok(candidates)
    }

    /// Keeps the model's verdict on a pair. Refused (`false`) if either
    /// entity is gone or its mentions changed since `mentions` were counted:
    /// an entry may have been forgotten while the model read it.
    pub fn record_verdict(
        &mut self,
        (a, mentions_a): (i64, usize),
        (b, mentions_b): (i64, usize),
        verdict: Verdict,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let ((a, mentions_a), (b, mentions_b)) = if a < b {
            ((a, mentions_a), (b, mentions_b))
        } else {
            ((b, mentions_b), (a, mentions_a))
        };
        if a == b {
            return Ok(false);
        }
        let tx = self.conn.transaction()?;
        let count = |id: i64| -> rusqlite::Result<i64> {
            tx.query_row(
                "SELECT count(*) FROM mentions WHERE entity_id = ?1",
                [id],
                |row| row.get(0),
            )
        };
        if count(a)? != mentions_a as i64 || count(b)? != mentions_b as i64 || mentions_a == 0 {
            return Ok(false);
        }
        tx.execute(
            "INSERT OR REPLACE INTO pair_verdicts
                 (a, b, verdict, reason, judged_at, mentions_a, mentions_b)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                a,
                b,
                verdict.as_str(),
                reason,
                now.timestamp_millis(),
                mentions_a as i64,
                mentions_b as i64
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// The pairs the model said are the same, not answered "not the same"
    /// by the user, each merged into the name with a page, else a kept one,
    /// else the most mentioned.
    pub fn merge_suggestions(&self) -> Result<Vec<Suggestion>> {
        let with_page: HashSet<i64> = self
            .pages()?
            .into_iter()
            .filter_map(|p| p.entity_id)
            .collect();
        let entities: HashMap<i64, Entity> =
            self.entities()?.into_iter().map(|e| (e.id, e)).collect();
        let not_same = self.not_same_pairs()?;
        let mut statement = self.conn.prepare(
            "SELECT a, b, reason FROM pair_verdicts WHERE verdict = 'yes' ORDER BY judged_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut suggestions = Vec::new();
        for row in rows {
            let (a, b, reason) = row?;
            let (Some(a), Some(b)) = (entities.get(&a), entities.get(&b)) else {
                continue;
            };
            if not_same.contains(&ordered(a.id, b.id)) || a.kind != b.kind {
                continue;
            }
            let stays = |e: &Entity| {
                (
                    with_page.contains(&e.id),
                    e.kept,
                    e.mentions,
                    std::cmp::Reverse(e.id),
                )
            };
            let (into, from) = if stays(a) >= stays(b) { (a, b) } else { (b, a) };
            suggestions.push(Suggestion {
                kind: a.kind,
                into: (into.id, into.name.clone()),
                from: (from.id, from.name.clone()),
                reason,
            });
        }
        Ok(suggestions)
    }

    /// The names of `id`'s kind most likely to be the same as it, for the
    /// Change panel's "Same as" list (ADR 0013, point 4), best first: pairs
    /// suggested (obvious or judged "yes"), then pairs with a name signal,
    /// then names that share a word with it (not a legal suffix); within
    /// each, the most mentioned first. Never one the user said is not the
    /// same.
    pub fn likely_same(&self, id: i64) -> Result<Vec<i64>> {
        let entities = self.entities()?;
        let Some(me) = entities.iter().find(|e| e.id == id) else {
            return Ok(Vec::new());
        };
        let not_same = self.not_same_pairs()?;
        // The names suggested together with it, obvious or judged.
        let paired = self
            .obvious_pairs()?
            .into_iter()
            .map(|p| (p.into.0, p.from.0))
            .chain(
                self.merge_suggestions()?
                    .into_iter()
                    .map(|s| (s.into.0, s.from.0)),
            );
        let suggested: HashSet<i64> = paired
            .filter_map(|(a, b)| match (a == id, b == id) {
                (true, _) => Some(b),
                (_, true) => Some(a),
                _ => None,
            })
            .collect();
        let my_words: HashSet<String> = words(&me.name)
            .into_iter()
            .filter(|w| !LEGAL_SUFFIXES.contains(&w.as_str()))
            .collect();
        let mut ranked: Vec<(u8, std::cmp::Reverse<usize>, i64)> = Vec::new();
        for other in &entities {
            if other.id == id || other.kind != me.kind || not_same.contains(&ordered(id, other.id))
            {
                continue;
            }
            let rank = if suggested.contains(&other.id) {
                0
            } else if signal(me, other).is_some() {
                1
            } else if words(&other.name).iter().any(|w| my_words.contains(w)) {
                2
            } else {
                continue;
            };
            ranked.push((rank, std::cmp::Reverse(other.mentions), other.id));
        }
        ranked.sort();
        Ok(ranked.into_iter().map(|(_, _, id)| id).collect())
    }

    fn verdicts(&self) -> Result<HashMap<(i64, i64), Judged>> {
        let mut statement = self.conn.prepare(
            "SELECT a, b, verdict, judged_at, mentions_a, mentions_b FROM pair_verdicts",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                (row.get(0)?, row.get(1)?),
                Judged {
                    yes: row.get::<_, String>(2)? == "yes",
                    at: DateTime::from_timestamp_millis(row.get(3)?).unwrap_or_default(),
                    mentions: (row.get(4)?, row.get(5)?),
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn mentions_by_entity(&self) -> Result<HashMap<i64, HashSet<i64>>> {
        let mut statement = self
            .conn
            .prepare("SELECT entity_id, episode_id FROM mentions")?;
        let rows =
            statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
        let mut by_entity: HashMap<i64, HashSet<i64>> = HashMap::new();
        for row in rows {
            let (entity, episode) = row?;
            by_entity.entry(entity).or_default().insert(episode);
        }
        Ok(by_entity)
    }

    /// The model's reason on a pair, for tests.
    #[cfg(test)]
    fn verdict_of(&self, a: i64, b: i64) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        let (a, b) = ordered(a, b);
        Ok(self
            .conn
            .query_row(
                "SELECT verdict FROM pair_verdicts WHERE a = ?1 AND b = ?2",
                params![a, b],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn not_same_pairs(&self) -> Result<HashSet<(i64, i64)>> {
        let mut statement = self.conn.prepare("SELECT a, b FROM not_same")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn ordered(a: i64, b: i64) -> (i64, i64) {
    (a.min(b), a.max(b))
}

/// A verdict already given.
struct Judged {
    yes: bool,
    at: DateTime<Utc>,
    mentions: (i64, i64),
}

impl Judged {
    /// A "yes" waits for the user; a "no" or "unsure" is judged again once
    /// either side's mentions doubled, or after 30 days.
    fn due(&self, now: DateTime<Utc>, mentions_a: i64, mentions_b: i64) -> bool {
        !self.yes
            && (mentions_a >= self.mentions.0 * REJUDGE_FACTOR
                || mentions_b >= self.mentions.1 * REJUDGE_FACTOR
                || now - self.at >= TimeDelta::days(REJUDGE_AFTER_DAYS))
    }
}

/// Words of a name for comparing: lowercase, without punctuation.
fn words(name: &str) -> Vec<String> {
    name_key(name)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// The strongest reason `a` and `b` may be the same, if any.
fn signal(a: &Entity, b: &Entity) -> Option<Signal> {
    let (wa, wb) = (words(&a.name), words(&b.name));
    if wa.is_empty() || wb.is_empty() {
        return None;
    }
    let (short, long) = if wa.len() <= wb.len() {
        (&wa, &wb)
    } else {
        (&wb, &wa)
    };
    if short.len() < long.len() && long.windows(short.len()).any(|w| w == short.as_slice()) {
        return Some(Signal::Contains);
    }
    if a.kind == EntityKind::Organisation {
        let ((base_a, suffix_a), (base_b, suffix_b)) = (legal_base(&a.name), legal_base(&b.name));
        if !base_a.is_empty() && base_a == base_b && suffix_a != suffix_b {
            return Some(Signal::LegalSuffix);
        }
    }
    let (ka, kb) = (wa.join(" "), wb.join(" "));
    let shortest = ka.chars().count().min(kb.chars().count());
    let allowed = match shortest {
        0..5 => 0,
        5..10 => 1,
        _ => 2,
    };
    (allowed > 0 && ka != kb && edit_distance(&ka, &kb) <= allowed).then_some(Signal::Typo)
}

/// Levenshtein distance in characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(previous + usize::from(ca != *cb));
            previous = above;
        }
    }
    row[b.len()]
}

/// More than half of the less mentioned name's entries name both: usually
/// two things one article or one meeting named, not one thing twice.
fn mostly_together(a: Option<&HashSet<i64>>, b: Option<&HashSet<i64>>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };
    let shared = a.intersection(b).count();
    shared * 2 > a.len().min(b.len())
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, TimeZone, Utc};

    use super::*;
    use crate::store::{Merged, NewCapture, NewEpisode};

    fn at(day: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 8, 10, 0, 0).unwrap() + TimeDelta::days(day)
    }

    /// Mentions each organisation in `names` in an episode on `day`.
    fn mention(store: &mut Store, day: i64, names: &[&str]) {
        mention_as(store, day, EntityKind::Organisation, names);
    }

    fn mention_as(store: &mut Store, day: i64, kind: EntityKind, names: &[&str]) {
        let capture = store
            .insert(&NewCapture {
                captured_at: at(day),
                bundle_id: "com.example.mail",
                app_name: "Mail",
                window_title: "Synthetic window",
                site: None,
                text: "Synthetic text",
            })
            .unwrap()
            .unwrap();
        let episode = store
            .record_episode(
                &NewEpisode {
                    summary: "Synthetic episode.".to_string(),
                    model: "example-model".to_string(),
                    created_at: at(day),
                },
                &[capture],
            )
            .unwrap()
            .unwrap();
        let names: Vec<_> = names.iter().map(|n| (kind, n.to_string())).collect();
        store.record_mentions(episode, &names, at(day)).unwrap();
    }

    fn id(store: &Store, name: &str) -> i64 {
        store
            .entities()
            .unwrap()
            .into_iter()
            .find(|e| e.name == name)
            .unwrap()
            .id
    }

    fn pairs(store: &Store) -> Vec<(String, String)> {
        store
            .obvious_pairs()
            .unwrap()
            .into_iter()
            .map(|p| (p.into.1, p.from.1))
            .collect()
    }

    #[test]
    fn legal_suffixes_and_punctuation_come_off() {
        assert_eq!(
            legal_base("Exempelbolaget AB"),
            ("exempelbolaget".into(), Some("ab"))
        );
        assert_eq!(
            legal_base("Exempelbolaget, AB."),
            ("exempelbolaget".into(), Some("ab"))
        );
        assert_eq!(legal_base("Example A/S"), ("example".into(), Some("a/s")));
        assert_eq!(
            legal_base("Example Labs Inc."),
            ("example labs".into(), Some("inc"))
        );
        assert_eq!(legal_base("EXEMPEL"), ("exempel".into(), None));
        // Only at the end, and only with something before it.
        assert_eq!(legal_base("AB Exempel"), ("ab exempel".into(), None));
        assert_eq!(legal_base("AB"), ("ab".into(), None));
        assert_eq!(legal_base("Åkeri Öst AB"), ("åkeri öst".into(), Some("ab")));
    }

    #[test]
    fn names_equal_but_for_a_suffix_are_obvious_pairs() {
        let mut store = Store::open_in_memory().unwrap();
        // Exempelbolaget AB is seen on two days, so the group isn't all new.
        mention(
            &mut store,
            0,
            &["Exempelbolaget AB", "Exempelbolaget", "Other Example"],
        );
        mention(&mut store, 1, &["Exempelbolaget AB", "EXEMPELBOLAGET, AB"]);
        assert_eq!(
            pairs(&store),
            [
                (
                    "Exempelbolaget AB".to_string(),
                    "EXEMPELBOLAGET, AB".to_string()
                ),
                (
                    "Exempelbolaget AB".to_string(),
                    "Exempelbolaget".to_string()
                ),
            ]
        );
    }

    #[test]
    fn different_suffixes_are_no_obvious_pair() {
        let mut store = Store::open_in_memory().unwrap();
        mention(&mut store, 0, &["Example AB", "Example Oy", "Example"]);
        mention(&mut store, 1, &["Example AB"]);
        // Example could be either: the whole group waits for the model.
        assert!(pairs(&store).is_empty());
    }

    #[test]
    fn groups_of_new_names_only_wait() {
        let mut store = Store::open_in_memory().unwrap();
        mention(&mut store, 0, &["Example AB", "Example"]);
        assert!(pairs(&store).is_empty());
        let example = id(&store, "Example");
        assert!(store.keep_entity(example).unwrap());
        // Kept: it stays, the other merges into it.
        assert_eq!(
            pairs(&store),
            [("Example".to_string(), "Example AB".to_string())]
        );
    }

    #[test]
    fn only_organisations_pair_up() {
        let mut store = Store::open_in_memory().unwrap();
        for day in [0, 1] {
            mention_as(
                &mut store,
                day,
                EntityKind::Project,
                &["Example", "Example AB"],
            );
        }
        assert!(pairs(&store).is_empty());
    }

    #[test]
    fn not_the_same_is_final_and_follows_a_merge() {
        let mut store = Store::open_in_memory().unwrap();
        mention(&mut store, 0, &["Example AB", "Example", "Example Group"]);
        mention(&mut store, 1, &["Example AB"]);
        let (ab, plain, group) = (
            id(&store, "Example AB"),
            id(&store, "Example"),
            id(&store, "Example Group"),
        );
        assert!(store.mark_not_same(plain, ab).unwrap());
        assert!(
            store.mark_not_same(ab, plain).unwrap(),
            "either order, once"
        );
        assert!(!store.mark_not_same(ab, ab).unwrap());
        assert!(pairs(&store).is_empty());
        // Merging Example into Example Group: Group isn't Example AB either.
        assert_eq!(
            store.merge_entities(plain, group, at(2)).unwrap(),
            Merged::Done
        );
        assert!(
            store
                .not_same_pairs()
                .unwrap()
                .contains(&ordered(ab, group))
        );
        // Forgetting a name takes its rows.
        assert!(store.forget_entity(group, false, at(2)).unwrap());
        assert!(store.not_same_pairs().unwrap().is_empty());
    }

    fn people(store: &mut Store, day: i64, names: &[&str]) {
        mention_as(store, day, EntityKind::Person, names);
    }

    fn candidates(store: &Store, now: i64) -> Vec<(String, String, Signal)> {
        store
            .pair_candidates(at(now))
            .unwrap()
            .into_iter()
            .map(|c| (c.a.name, c.b.name, c.signal))
            .collect()
    }

    fn counts(store: &Store, a: &str, b: &str) -> ((i64, usize), (i64, usize)) {
        let entity = |name: &str| {
            store
                .entities()
                .unwrap()
                .into_iter()
                .find(|e| e.name == name)
                .unwrap()
        };
        let (a, b) = (entity(a), entity(b));
        ((a.id, a.mentions), (b.id, b.mentions))
    }

    #[test]
    fn names_signal_by_words_suffixes_and_typos() {
        let entity = |kind, name: &str| Entity {
            id: 0,
            kind,
            name: name.to_string(),
            aliases: Vec::new(),
            mentions: 1,
            days: 1,
            last_seen: at(0),
            first_seen: at(0),
            kept: false,
            part_of: None,
        };
        let p = |a: &str, b: &str| {
            signal(
                &entity(EntityKind::Person, a),
                &entity(EntityKind::Person, b),
            )
        };
        let o = |a: &str, b: &str| {
            signal(
                &entity(EntityKind::Organisation, a),
                &entity(EntityKind::Organisation, b),
            )
        };
        assert_eq!(p("Anna", "Anna Example"), Some(Signal::Contains));
        assert_eq!(p("lax", "Lax development"), Some(Signal::Contains));
        // Whole words only.
        assert_eq!(p("Ann", "Anna Example"), None);
        assert_eq!(o("Example AB", "Example Oy"), Some(Signal::LegalSuffix));
        assert_eq!(o("Nyterios", "Nyterio"), Some(Signal::Typo));
        assert_eq!(p("Erik Example", "Erika Example"), Some(Signal::Typo));
        // Short names need to be equal: "Bo" and "Bob" are two people.
        assert_eq!(p("Bo", "Bob"), None);
        assert_eq!(p("Anna Example", "Cecilia Other"), None);
    }

    #[test]
    fn candidates_need_a_name_signal_and_one_side_known() {
        let mut store = Store::open_in_memory().unwrap();
        people(&mut store, 0, &["Anna Example"]);
        people(&mut store, 1, &["Anna Example"]);
        people(&mut store, 2, &["Anna"]);
        people(&mut store, 3, &["Bo", "Bob Exempel"]);
        assert_eq!(
            candidates(&store, 3),
            [(
                "Anna Example".to_string(),
                "Anna".to_string(),
                Signal::Contains
            )]
        );
    }

    #[test]
    fn names_mostly_in_the_same_entries_are_no_candidates() {
        let mut store = Store::open_in_memory().unwrap();
        // An article that names a company and its product line every time.
        for day in [0, 1] {
            mention(&mut store, day, &["Example Motors", "Example"]);
        }
        assert!(candidates(&store, 1).is_empty());
    }

    #[test]
    fn obvious_pairs_and_not_the_same_are_no_candidates() {
        let mut store = Store::open_in_memory().unwrap();
        for day in [0, 1] {
            mention(&mut store, day, &["Example AB"]);
        }
        mention(&mut store, 2, &["Example"]);
        assert!(
            candidates(&store, 2).is_empty(),
            "obvious, not for the model"
        );
        people(&mut store, 3, &["Anna Example"]);
        people(&mut store, 4, &["Anna Example"]);
        people(&mut store, 5, &["Anna"]);
        let ((anna, _), (example, _)) = counts(&store, "Anna", "Anna Example");
        assert!(store.mark_not_same(anna, example).unwrap());
        assert!(candidates(&store, 5).is_empty());
    }

    #[test]
    fn a_verdict_is_kept_and_judged_again_when_due() {
        let mut store = Store::open_in_memory().unwrap();
        people(&mut store, 0, &["Anna Example"]);
        people(&mut store, 1, &["Anna Example"]);
        people(&mut store, 2, &["Anna"]);
        let (a, b) = counts(&store, "Anna", "Anna Example");
        assert!(
            store
                .record_verdict(a, b, Verdict::Unsure, "", at(2))
                .unwrap()
        );
        assert!(candidates(&store, 3).is_empty());
        // After 30 days it is due again.
        assert_eq!(candidates(&store, 32).len(), 1);
        // So it is once a side's mentions double.
        people(&mut store, 4, &["Anna"]);
        assert_eq!(candidates(&store, 5).len(), 1);
    }

    #[test]
    fn a_verdict_on_changed_mentions_is_refused() {
        let mut store = Store::open_in_memory().unwrap();
        people(&mut store, 0, &["Anna Example"]);
        people(&mut store, 1, &["Anna Example"]);
        people(&mut store, 2, &["Anna"]);
        let (a, b) = counts(&store, "Anna", "Anna Example");
        // Another entry arrived while the model read: counted again next time.
        people(&mut store, 3, &["Anna"]);
        assert!(
            !store
                .record_verdict(a, b, Verdict::Yes, "Same.", at(3))
                .unwrap()
        );
        assert!(store.merge_suggestions().unwrap().is_empty());
    }

    #[test]
    fn yes_is_a_suggestion_into_the_name_that_stays() {
        let mut store = Store::open_in_memory().unwrap();
        people(&mut store, 0, &["Anna Example"]);
        people(&mut store, 1, &["Anna Example"]);
        people(&mut store, 2, &["Anna"]);
        let (a, b) = counts(&store, "Anna", "Anna Example");
        assert!(
            store
                .record_verdict(a, b, Verdict::Yes, "Same team.", at(2))
                .unwrap()
        );
        let suggestions = store.merge_suggestions().unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].into.1, "Anna Example");
        assert_eq!(suggestions[0].from.1, "Anna");
        assert_eq!(suggestions[0].reason, "Same team.");
        // A yes is never judged again: it waits for the user.
        assert!(candidates(&store, 40).is_empty());
        // Not the same takes it away.
        assert!(store.mark_not_same(a.0, b.0).unwrap());
        assert!(store.merge_suggestions().unwrap().is_empty());
    }

    #[test]
    fn a_forgotten_entry_takes_the_verdict() {
        let mut store = Store::open_in_memory().unwrap();
        people(&mut store, 0, &["Anna Example"]);
        people(&mut store, 1, &["Anna Example"]);
        people(&mut store, 2, &["Anna"]);
        let (a, b) = counts(&store, "Anna", "Anna Example");
        assert!(
            store
                .record_verdict(a, b, Verdict::Yes, "Same team.", at(2))
                .unwrap()
        );
        // The reason may have drawn on the first entry: forgetting it, the
        // verdict goes, and the pair is judged anew.
        store
            .conn
            .execute(
                "DELETE FROM mentions WHERE episode_id = (SELECT min(id) FROM episodes)",
                [],
            )
            .unwrap();
        assert_eq!(store.verdict_of(a.0, b.0).unwrap(), None);
        assert!(store.merge_suggestions().unwrap().is_empty());
    }

    #[test]
    fn likely_names_come_first_by_how_likely() {
        let mut store = Store::open_in_memory().unwrap();
        for day in [0, 1] {
            people(&mut store, day, &["Anna Example", "Anna Other"]);
        }
        people(&mut store, 2, &["Anna"]);
        people(&mut store, 3, &["Anne"]);
        people(&mut store, 4, &["Bo Example"]);
        people(&mut store, 5, &["Cecilia Unrelated"]);
        people(&mut store, 6, &["Dan Example"]);
        mention_as(&mut store, 7, EntityKind::Project, &["Anna Projekt"]);
        let (anna, example) = counts(&store, "Anna", "Anna Example");
        assert!(
            store
                .record_verdict(anna, example, Verdict::Yes, "Same team.", at(7))
                .unwrap()
        );
        let name = |id: i64| {
            store
                .entities()
                .unwrap()
                .into_iter()
                .find(|e| e.id == id)
                .unwrap()
                .name
        };
        let likely: Vec<String> = store
            .likely_same(anna.0)
            .unwrap()
            .into_iter()
            .map(name)
            .collect();
        // Suggested, then a name signal; never another kind or an unrelated name.
        assert_eq!(likely, ["Anna Example", "Anna Other"]);
        let likely: Vec<String> = store
            .likely_same(counts(&store, "Bo Example", "Anna").0.0)
            .unwrap()
            .into_iter()
            .map(name)
            .collect();
        // Sharing a word: the most mentioned first.
        assert_eq!(likely, ["Anna Example", "Dan Example"]);
        // Not the same takes a name off.
        assert!(store.mark_not_same(anna.0, example.0).unwrap());
        let likely: Vec<String> = store
            .likely_same(anna.0)
            .unwrap()
            .into_iter()
            .map(name)
            .collect();
        assert_eq!(likely, ["Anna Other"]);
    }
}
