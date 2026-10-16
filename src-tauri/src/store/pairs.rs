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
    /// Two projects named in the same entries on two or more days: how a
    /// project and its part show up when their names differ (ADR 0013).
    Together,
}

/// Projects named together on at least this many days are a candidate pair.
const TOGETHER_DAYS: usize = 2;

/// How two projects relate, as the model judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Same,
    /// Part of the other; which one is the whole is given with it.
    PartOf,
    Neither,
    Unsure,
}

impl Relation {
    fn as_str(self) -> &'static str {
        match self {
            Relation::Same => "same",
            Relation::PartOf => "part_of",
            Relation::Neither => "neither",
            Relation::Unsure => "unsure",
        }
    }
}

/// A project the model said is part of another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartSuggestion {
    pub part: (i64, String),
    pub whole: (i64, String),
    /// The model's reason: untrusted text, shown as text only.
    pub reason: String,
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
                // Projects have their own judge, which knows about parts.
                if a.kind != b.kind || a.kind == EntityKind::Project || (a.is_new() && b.is_new()) {
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

    /// Pairs of projects for the model to judge as the same, one part of the
    /// other, or neither (ADR 0013): a name signal with at least one side
    /// known, or both known and named together on `TOGETHER_DAYS` days. Not
    /// "not the same", not "not part", not already one part of the other,
    /// and not judged yet or due again, as for [`Store::pair_candidates`].
    pub fn part_candidates(&self, now: DateTime<Utc>) -> Result<Vec<Candidate>> {
        let projects: Vec<Entity> = self
            .entities()?
            .into_iter()
            .filter(|e| e.kind == EntityKind::Project)
            .collect();
        let not_same = self.not_same_pairs()?;
        let not_part = self.not_part_pairs()?;
        let verdicts = self.project_verdicts()?;
        let mentions = self.mentions_by_entity()?;
        let days = self.episode_days()?;
        let mut candidates = Vec::new();
        for (i, a) in projects.iter().enumerate() {
            for b in &projects[i + 1..] {
                let key = ordered(a.id, b.id);
                if not_same.contains(&key)
                    || not_part.contains(&key)
                    || a.part_of == Some(b.id)
                    || b.part_of == Some(a.id)
                {
                    continue;
                }
                let signal = match signal(a, b) {
                    Some(signal) if !(a.is_new() && b.is_new()) => signal,
                    _ if !a.is_new() && !b.is_new() => {
                        let together: HashSet<&String> =
                            match (mentions.get(&a.id), mentions.get(&b.id)) {
                                (Some(x), Some(y)) => {
                                    x.intersection(y).filter_map(|e| days.get(e)).collect()
                                }
                                _ => HashSet::new(),
                            };
                        if together.len() < TOGETHER_DAYS {
                            continue;
                        }
                        Signal::Together
                    }
                    _ => continue,
                };
                let (first, second) = if a.id < b.id { (a, b) } else { (b, a) };
                if let Some(v) = verdicts.get(&key)
                    && !v.due(now, first.mentions as i64, second.mentions as i64)
                {
                    continue;
                }
                candidates.push(Candidate {
                    kind: EntityKind::Project,
                    a: first.clone(),
                    b: second.clone(),
                    signal,
                });
            }
        }
        candidates.sort_by_key(|c| (c.signal, std::cmp::Reverse(c.a.mentions + c.b.mentions)));
        Ok(candidates)
    }

    /// Keeps the model's verdict on a pair of projects; `whole` is the
    /// whole's id for `Relation::PartOf`. Refused (`false`) as
    /// [`Store::record_verdict`] is, and for a `whole` that isn't one of them.
    pub fn record_project_verdict(
        &mut self,
        (a, mentions_a): (i64, usize),
        (b, mentions_b): (i64, usize),
        relation: Relation,
        whole: Option<i64>,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let whole = whole.filter(|_| relation == Relation::PartOf);
        if a == b
            || (relation == Relation::PartOf) != whole.is_some()
            || whole.is_some_and(|w| w != a && w != b)
        {
            return Ok(false);
        }
        let ((a, mentions_a), (b, mentions_b)) = if a < b {
            ((a, mentions_a), (b, mentions_b))
        } else {
            ((b, mentions_b), (a, mentions_a))
        };
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
            "INSERT OR REPLACE INTO project_verdicts
                 (a, b, relation, whole, reason, judged_at, mentions_a, mentions_b)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                a,
                b,
                relation.as_str(),
                whole,
                reason,
                now.timestamp_millis(),
                mentions_a as i64,
                mentions_b as i64
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// The projects the model said are part of another, that can be made so
    /// now (one level deep) and the user hasn't answered "No" or "Not the
    /// same" for.
    pub fn part_suggestions(&self) -> Result<Vec<PartSuggestion>> {
        let entities: HashMap<i64, Entity> =
            self.entities()?.into_iter().map(|e| (e.id, e)).collect();
        let has_parts: HashSet<i64> = entities.values().filter_map(|e| e.part_of).collect();
        let (not_same, not_part) = (self.not_same_pairs()?, self.not_part_pairs()?);
        let mut statement = self.conn.prepare(
            "SELECT a, b, whole, reason FROM project_verdicts
             WHERE relation = 'part_of' ORDER BY judged_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut suggestions = Vec::new();
        for row in rows {
            let (a, b, whole, reason) = row?;
            let key = ordered(a, b);
            if not_same.contains(&key) || not_part.contains(&key) {
                continue;
            }
            let part = if whole == a { b } else { a };
            let (Some(part), Some(whole)) = (entities.get(&part), entities.get(&whole)) else {
                continue;
            };
            // Only what can be done now: one level deep.
            if part.part_of.is_some() || whole.part_of.is_some() || has_parts.contains(&part.id) {
                continue;
            }
            suggestions.push(PartSuggestion {
                part: (part.id, part.name.clone()),
                whole: (whole.id, whole.name.clone()),
                reason,
            });
        }
        Ok(suggestions)
    }

    /// The user says project `a` isn't part of `b`, nor `b` of `a`: never
    /// suggested as parts again. `false` if either is gone.
    pub fn mark_not_part(&self, a: i64, b: i64) -> Result<bool> {
        if a == b {
            return Ok(false);
        }
        let (a, b) = ordered(a, b);
        self.conn.execute(
            "INSERT OR IGNORE INTO not_part (a, b)
             SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM entities WHERE id = ?1)
                             AND EXISTS (SELECT 1 FROM entities WHERE id = ?2)",
            params![a, b],
        )?;
        Ok(self.not_part_pairs()?.contains(&(a, b)))
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
            "SELECT a, b, reason, judged_at FROM pair_verdicts WHERE verdict = 'yes'
             UNION ALL
             SELECT a, b, reason, judged_at FROM project_verdicts WHERE relation = 'same'
             ORDER BY 4",
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

    fn project_verdicts(&self) -> Result<HashMap<(i64, i64), Judged>> {
        let mut statement = self.conn.prepare(
            "SELECT a, b, relation, judged_at, mentions_a, mentions_b FROM project_verdicts",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                (row.get(0)?, row.get(1)?),
                Judged {
                    // "same" and "part_of" wait for the user, like a yes.
                    yes: matches!(row.get::<_, String>(2)?.as_str(), "same" | "part_of"),
                    at: DateTime::from_timestamp_millis(row.get(3)?).unwrap_or_default(),
                    mentions: (row.get(4)?, row.get(5)?),
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The day (local time) of every episode that mentions something.
    fn episode_days(&self) -> Result<HashMap<i64, String>> {
        let mut statement = self.conn.prepare(
            "SELECT id, date(started_at / 1000, 'unixepoch', 'localtime') FROM episodes
             WHERE id IN (SELECT episode_id FROM mentions)",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn not_part_pairs(&self) -> Result<HashSet<(i64, i64)>> {
        let mut statement = self.conn.prepare("SELECT a, b FROM not_part")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
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
    fn projects(store: &mut Store, day: i64, names: &[&str]) {
        mention_as(store, day, EntityKind::Project, names);
    }

    fn part_candidates(store: &Store, now: i64) -> Vec<(String, String, Signal)> {
        store
            .part_candidates(at(now))
            .unwrap()
            .into_iter()
            .map(|c| (c.a.name, c.b.name, c.signal))
            .collect()
    }

    #[test]
    fn projects_named_together_on_two_days_are_candidates() {
        let mut store = Store::open_in_memory().unwrap();
        // Two known projects named together on one day only: not yet.
        projects(&mut store, 0, &["Nyterio", "LinkedIn-serien"]);
        projects(&mut store, 1, &["Nyterio"]);
        projects(&mut store, 2, &["LinkedIn-serien"]);
        assert!(part_candidates(&store, 2).is_empty());
        projects(&mut store, 3, &["Nyterio", "LinkedIn-serien"]);
        assert_eq!(
            part_candidates(&store, 3),
            [(
                "Nyterio".to_string(),
                "LinkedIn-serien".to_string(),
                Signal::Together
            )]
        );
        // Projects go to the part judge, not the same-name judge.
        assert!(candidates(&store, 3).is_empty());
    }

    #[test]
    fn project_names_with_a_signal_are_candidates() {
        let mut store = Store::open_in_memory().unwrap();
        projects(&mut store, 0, &["Exempelappen"]);
        projects(&mut store, 1, &["Exempelappen"]);
        projects(&mut store, 2, &["Exempelappen export"]);
        assert_eq!(
            part_candidates(&store, 2),
            [(
                "Exempelappen".to_string(),
                "Exempelappen export".to_string(),
                Signal::Contains
            )]
        );
    }

    #[test]
    fn part_of_is_a_suggestion_until_answered() {
        let mut store = Store::open_in_memory().unwrap();
        projects(&mut store, 0, &["Exempelappen"]);
        projects(&mut store, 1, &["Exempelappen"]);
        projects(&mut store, 2, &["Exempelappen export"]);
        let (app, export) = counts(&store, "Exempelappen", "Exempelappen export");
        // A whole that isn't one of the two is refused.
        assert!(
            !store
                .record_project_verdict(app, export, Relation::PartOf, Some(999), "", at(2))
                .unwrap()
        );
        assert!(
            store
                .record_project_verdict(
                    app,
                    export,
                    Relation::PartOf,
                    Some(app.0),
                    "A feature.",
                    at(2)
                )
                .unwrap()
        );
        let suggestions = store.part_suggestions().unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].part.1, "Exempelappen export");
        assert_eq!(suggestions[0].whole.1, "Exempelappen");
        assert_eq!(suggestions[0].reason, "A feature.");
        // Waiting for the user: not judged again.
        assert!(part_candidates(&store, 40).is_empty());
        // Taken out after a Yes, it isn't suggested again.
        assert_eq!(
            store.set_part_of(export.0, app.0).unwrap(),
            crate::store::PartOf::Done
        );
        assert!(store.part_suggestions().unwrap().is_empty());
        assert!(store.take_out(export.0).unwrap());
        assert!(store.part_suggestions().unwrap().is_empty());
        // The user's No is final.
        assert!(store.mark_not_part(export.0, app.0).unwrap());
        assert!(store.part_suggestions().unwrap().is_empty());
        assert!(part_candidates(&store, 40).is_empty());
    }

    #[test]
    fn same_projects_are_a_merge_suggestion_and_parts_need_one_level() {
        let mut store = Store::open_in_memory().unwrap();
        projects(&mut store, 0, &["Lax", "Exempelappen", "Kryptering"]);
        projects(&mut store, 1, &["Lax", "Exempelappen", "Kryptering"]);
        projects(&mut store, 2, &["Lax development"]);
        let (lax, dev) = counts(&store, "Lax", "Lax development");
        assert!(
            store
                .record_project_verdict(lax, dev, Relation::Same, None, "One project.", at(2))
                .unwrap()
        );
        let merges = store.merge_suggestions().unwrap();
        assert_eq!(merges.len(), 1);
        assert_eq!(merges[0].into.1, "Lax");
        // A part suggestion that can't be done now (the whole is a part) waits.
        let (app, crypto) = counts(&store, "Exempelappen", "Kryptering");
        assert!(
            store
                .record_project_verdict(app, crypto, Relation::PartOf, Some(app.0), "", at(2))
                .unwrap()
        );
        assert_eq!(
            store.set_part_of(app.0, lax.0).unwrap(),
            crate::store::PartOf::Done
        );
        assert!(store.part_suggestions().unwrap().is_empty());
    }

    #[test]
    fn a_forgotten_entry_takes_the_project_verdict() {
        let mut store = Store::open_in_memory().unwrap();
        projects(&mut store, 0, &["Exempelappen"]);
        projects(&mut store, 1, &["Exempelappen"]);
        projects(&mut store, 2, &["Exempelappen export"]);
        let (app, export) = counts(&store, "Exempelappen", "Exempelappen export");
        assert!(
            store
                .record_project_verdict(app, export, Relation::PartOf, Some(app.0), "x", at(2))
                .unwrap()
        );
        store
            .conn
            .execute(
                "DELETE FROM mentions WHERE episode_id = (SELECT min(id) FROM episodes)",
                [],
            )
            .unwrap();
        assert!(store.part_suggestions().unwrap().is_empty());
    }
}
