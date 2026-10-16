//! Merge suggestions (ADR 0013, point 3): pairs of names that look like the
//! same entity, found by their names alone. This part needs no model: the
//! obvious pairs, organisations whose names are equal once a legal suffix
//! and punctuation are taken off ("Example" and "Example AB"), and the
//! user's "not the same" answers.

use std::collections::{HashMap, HashSet};

use rusqlite::params;

use super::{EntityKind, Result, Store};

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

    fn not_same_pairs(&self) -> Result<HashSet<(i64, i64)>> {
        let mut statement = self.conn.prepare("SELECT a, b FROM not_same")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn ordered(a: i64, b: i64) -> (i64, i64) {
    (a.min(b), a.max(b))
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
}
