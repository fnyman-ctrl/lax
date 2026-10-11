//! The memory's tables (Stage 4, ADR 0008): entities, the names they go by,
//! and the episodes that mention them. See `migrations/0006_memory.sql`.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::{Result, Store, sql_limit};

const MEMORY_KEY: &str = "memory_enabled";

/// What an entity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    Person,
    Organisation,
    Project,
}

impl EntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntityKind::Person => "person",
            EntityKind::Organisation => "organisation",
            EntityKind::Project => "project",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "person" => Some(EntityKind::Person),
            "organisation" => Some(EntityKind::Organisation),
            "project" => Some(EntityKind::Project),
            _ => None,
        }
    }
}

/// A name as it is matched: case-folded and with whitespace collapsed.
/// Diacritics are kept, so "Åsa" and "Asa" stay two people (ADR 0008).
pub fn name_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// An episode the memory hasn't read yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrecognizedEpisode {
    pub id: i64,
    pub summary: String,
    pub app_names: Vec<String>,
}

/// An entity, for showing in the Memory view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    pub id: i64,
    pub kind: EntityKind,
    pub name: String,
    /// Other names it was seen under, in the order they were first seen.
    pub aliases: Vec<String>,
    /// How many episodes mention it.
    pub mentions: usize,
    /// On how many different days (local time) it was mentioned.
    pub days: usize,
    pub last_seen: DateTime<Utc>,
}

impl Store {
    pub fn memory_enabled(&self) -> Result<bool> {
        Ok(self.setting(MEMORY_KEY)?.as_deref() == Some("1"))
    }

    pub fn set_memory_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting(MEMORY_KEY, if enabled { "1" } else { "0" })
    }

    /// Episodes the memory hasn't read, oldest first.
    pub fn unrecognized_episodes(&self, limit: usize) -> Result<Vec<UnrecognizedEpisode>> {
        let mut statement = self.conn.prepare(
            "SELECT id, summary, app_names FROM episodes
             WHERE recognized_at IS NULL ORDER BY id LIMIT ?1",
        )?;
        let rows = statement.query_map([sql_limit(limit)], |row| {
            let names: String = row.get(2)?;
            Ok(UnrecognizedEpisode {
                id: row.get(0)?,
                summary: row.get(1)?,
                app_names: names
                    .lines()
                    .filter(|n| !n.is_empty())
                    .map(String::from)
                    .collect(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn unrecognized_count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM episodes WHERE recognized_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// Records that `episode` names `names` and marks it read. A name joins
    /// the entity of its kind it was seen under before, or starts a new one
    /// (ADR 0008: only an exact match of the normalized name).
    ///
    /// Refused (`false`) if the episode is gone or already read: the user may
    /// have forgotten it while the model was reading.
    pub fn record_mentions(
        &mut self,
        episode: i64,
        names: &[(EntityKind, String)],
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let marked = tx.execute(
            "UPDATE episodes SET recognized_at = ?2 WHERE id = ?1 AND recognized_at IS NULL",
            params![episode, now.timestamp_millis()],
        )?;
        if marked == 0 {
            return Ok(false);
        }
        for (kind, name) in names {
            let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
            let key = name_key(&name);
            if key.is_empty() {
                continue;
            }
            let known: Option<i64> = tx
                .query_row(
                    "SELECT entity_id FROM aliases WHERE kind = ?1 AND key = ?2",
                    params![kind.as_str(), key],
                    |row| row.get(0),
                )
                .optional()?;
            let entity = match known {
                Some(entity) => entity,
                None => {
                    tx.execute(
                        "INSERT INTO entities (kind, name, created_at) VALUES (?1, ?2, ?3)",
                        params![kind.as_str(), name, now.timestamp_millis()],
                    )?;
                    let entity = tx.last_insert_rowid();
                    tx.execute(
                        "INSERT INTO aliases (kind, key, name, entity_id) VALUES (?1, ?2, ?3, ?4)",
                        params![kind.as_str(), key, name, entity],
                    )?;
                    entity
                }
            };
            tx.execute(
                "INSERT OR IGNORE INTO mentions (entity_id, episode_id) VALUES (?1, ?2)",
                [entity, episode],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Every entity, most mentioned first.
    pub fn entities(&self) -> Result<Vec<Entity>> {
        let mut statement = self.conn.prepare(
            "SELECT n.id, n.kind, n.name,
                    (SELECT group_concat(name, char(10) ORDER BY rowid) FROM aliases
                     WHERE entity_id = n.id AND name != n.name),
                    count(*),
                    count(DISTINCT date(e.started_at / 1000, 'unixepoch', 'localtime')),
                    max(e.ended_at)
             FROM entities AS n
             JOIN mentions AS m ON m.entity_id = n.id
             JOIN episodes AS e ON e.id = m.episode_id
             GROUP BY n.id
             ORDER BY count(*) DESC, max(e.ended_at) DESC, n.id",
        )?;
        let rows = statement.query_map([], |row| {
            let kind: String = row.get(1)?;
            let aliases: Option<String> = row.get(3)?;
            let mentions: i64 = row.get(4)?;
            let days: i64 = row.get(5)?;
            let last: i64 = row.get(6)?;
            Ok((
                kind,
                Entity {
                    id: row.get(0)?,
                    kind: EntityKind::Person,
                    name: row.get(2)?,
                    aliases: aliases
                        .unwrap_or_default()
                        .lines()
                        .map(String::from)
                        .collect(),
                    mentions: usize::try_from(mentions).unwrap_or(0),
                    days: usize::try_from(days).unwrap_or(0),
                    last_seen: DateTime::from_timestamp_millis(last).unwrap_or_default(),
                },
            ))
        })?;
        let mut entities = Vec::new();
        for row in rows {
            let (kind, mut entity) = row?;
            // The table only allows the three kinds.
            let Some(kind) = EntityKind::parse(&kind) else {
                continue;
            };
            entity.kind = kind;
            entities.push(entity);
        }
        Ok(entities)
    }

    /// Deletes an entity, the names it goes by and its mentions. The
    /// episodes stay. Returns whether it existed.
    ///
    /// The rows are short, so this skips the full rebuild of
    /// [`Store::forget`]: `secure_delete` overwrites the freed pages and the
    /// checkpoint empties the write-ahead log, as for [`Store::expire`].
    pub fn forget_entity(&self, id: i64) -> Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM entities WHERE id = ?1", [id])?;
        if deleted > 0 {
            self.checkpoint()?;
        }
        Ok(deleted > 0)
    }

    /// Forgets everything the memory noted and turns it off. Episodes it
    /// already read stay read, so turning it on again starts with new ones.
    /// Returns how many entities were forgotten.
    pub fn forget_memory(&self) -> Result<usize> {
        self.set_memory_enabled(false)?;
        let deleted = self.conn.execute("DELETE FROM entities", [])?;
        if deleted > 0 {
            self.checkpoint()?;
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewCapture, NewEpisode};
    use chrono::TimeDelta;

    /// Minutes after 10:00 UTC, which is the same day in local time from
    /// UTC-10 to UTC+13, so day counts don't depend on the time zone.
    fn at(minute: i64) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 10, 8, 10, 0, 0).unwrap() + TimeDelta::minutes(minute)
    }

    /// Stores one capture of `app` at `minute` and an episode about it.
    fn episode(store: &mut Store, minute: i64, app: &str) -> i64 {
        let id = store
            .insert(&NewCapture {
                captured_at: at(minute),
                bundle_id: &format!("com.example.{}", app.to_lowercase()),
                app_name: app,
                window_title: "Synthetic window",
                site: None,
                text: "Synthetic text",
            })
            .unwrap()
            .unwrap();
        store
            .record_episode(
                &NewEpisode {
                    summary: format!("Synthetic episode at {minute}."),
                    model: "example-model".to_string(),
                    created_at: at(minute),
                },
                &[id],
            )
            .unwrap()
            .unwrap()
    }

    fn person(name: &str) -> (EntityKind, String) {
        (EntityKind::Person, name.to_string())
    }

    fn names(store: &Store) -> Vec<(String, usize)> {
        store
            .entities()
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.mentions))
            .collect()
    }

    #[test]
    fn the_memory_is_off_until_turned_on() {
        let store = Store::open_in_memory().unwrap();
        assert!(!store.memory_enabled().unwrap());
        store.set_memory_enabled(true).unwrap();
        assert!(store.memory_enabled().unwrap());
    }

    #[test]
    fn episodes_are_read_once() {
        let mut store = Store::open_in_memory().unwrap();
        let first = episode(&mut store, 0, "Mail");
        let second = episode(&mut store, 20, "Slack");
        let waiting = store.unrecognized_episodes(10).unwrap();
        assert_eq!(
            waiting.iter().map(|e| e.id).collect::<Vec<_>>(),
            [first, second]
        );
        assert_eq!(waiting[0].app_names, ["Mail"]);
        assert_eq!(store.unrecognized_count().unwrap(), 2);

        assert!(store.record_mentions(first, &[], at(30)).unwrap());
        assert!(
            !store
                .record_mentions(first, &[person("Anna Example")], at(31))
                .unwrap()
        );
        assert_eq!(store.unrecognized_count().unwrap(), 1);
        assert!(names(&store).is_empty());
    }

    #[test]
    fn the_same_name_is_the_same_entity() {
        let mut store = Store::open_in_memory().unwrap();
        let first = episode(&mut store, 0, "Mail");
        let second = episode(&mut store, 20, "Mail");
        store
            .record_mentions(first, &[person("Anna Example")], at(30))
            .unwrap();
        store
            .record_mentions(
                second,
                &[person("anna  example"), person("Bo Exempel")],
                at(31),
            )
            .unwrap();
        assert_eq!(
            names(&store),
            [
                ("Anna Example".to_string(), 2),
                ("Bo Exempel".to_string(), 1)
            ]
        );
    }

    #[test]
    fn names_keep_their_swedish_letters_and_kind() {
        let mut store = Store::open_in_memory().unwrap();
        let id = episode(&mut store, 0, "Mail");
        store
            .record_mentions(
                id,
                &[
                    person("Åsa Exempel"),
                    person("Asa Exempel"),
                    (EntityKind::Project, "Åsa Exempel".to_string()),
                ],
                at(30),
            )
            .unwrap();
        let entities = store.entities().unwrap();
        assert_eq!(entities.len(), 3);
        assert_eq!(
            entities
                .iter()
                .filter(|e| e.kind == EntityKind::Project)
                .count(),
            1
        );
    }

    #[test]
    fn a_name_twice_in_one_episode_is_one_mention() {
        let mut store = Store::open_in_memory().unwrap();
        let id = episode(&mut store, 0, "Mail");
        store
            .record_mentions(
                id,
                &[person("Anna Example"), person("ANNA EXAMPLE")],
                at(30),
            )
            .unwrap();
        assert_eq!(names(&store), [("Anna Example".to_string(), 1)]);
    }

    #[test]
    fn days_and_last_seen_come_from_the_episodes() {
        let mut store = Store::open_in_memory().unwrap();
        let today = episode(&mut store, 0, "Mail");
        let later = episode(&mut store, 30, "Mail");
        let tomorrow = episode(&mut store, 24 * 60, "Mail");
        for id in [today, later, tomorrow] {
            store
                .record_mentions(id, &[person("Anna Example")], at(24 * 60 + 10))
                .unwrap();
        }
        let anna = &store.entities().unwrap()[0];
        assert_eq!(anna.mentions, 3);
        assert_eq!(anna.days, 2);
        assert_eq!(anna.last_seen, at(24 * 60));
    }

    #[test]
    fn forgetting_episodes_forgets_what_only_they_mentioned() {
        let mut store = Store::open_in_memory().unwrap();
        let mail = episode(&mut store, 0, "Mail");
        let slack = episode(&mut store, 20, "Slack");
        store
            .record_mentions(
                mail,
                &[person("Anna Example"), person("Bo Exempel")],
                at(30),
            )
            .unwrap();
        store
            .record_mentions(slack, &[person("Anna Example")], at(31))
            .unwrap();

        store
            .forget(&crate::store::Forget::App("com.example.mail".to_string()))
            .unwrap();
        assert_eq!(names(&store), [("Anna Example".to_string(), 1)]);
        // Bo's name is gone too, not only his mentions.
        let aliases: i64 = store
            .conn
            .query_row("SELECT count(*) FROM aliases", [], |r| r.get(0))
            .unwrap();
        assert_eq!(aliases, 1);

        store.forget(&crate::store::Forget::Everything).unwrap();
        assert!(names(&store).is_empty());
        let left: i64 = store
            .conn
            .query_row(
                "SELECT (SELECT count(*) FROM entities) + (SELECT count(*) FROM aliases)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn mentions_of_a_forgotten_episode_are_refused() {
        let mut store = Store::open_in_memory().unwrap();
        let id = episode(&mut store, 0, "Mail");
        store.forget(&crate::store::Forget::Everything).unwrap();
        assert!(
            !store
                .record_mentions(id, &[person("Anna Example")], at(30))
                .unwrap()
        );
        assert!(names(&store).is_empty());
    }

    #[test]
    fn forgetting_the_memory_forgets_every_entity_and_turns_it_off() {
        let mut store = Store::open_in_memory().unwrap();
        store.set_memory_enabled(true).unwrap();
        let id = episode(&mut store, 0, "Mail");
        store
            .record_mentions(id, &[person("Anna Example"), person("Bo Exempel")], at(30))
            .unwrap();
        assert_eq!(store.forget_memory().unwrap(), 2);
        assert!(names(&store).is_empty());
        assert!(!store.memory_enabled().unwrap());
        assert_eq!(store.unrecognized_count().unwrap(), 0);
        assert_eq!(store.episodes(None, 10).unwrap().len(), 1);
    }

    #[test]
    fn other_names_are_listed_in_the_order_first_seen() {
        let mut store = Store::open_in_memory().unwrap();
        let id = episode(&mut store, 0, "Mail");
        store
            .record_mentions(id, &[person("Anna Example")], at(30))
            .unwrap();
        let anna = store.entities().unwrap()[0].id;
        // Merging comes in a later step; add names the way it will.
        for name in ["Anna E.", "Anna"] {
            store
                .conn
                .execute(
                    "INSERT INTO aliases (kind, key, name, entity_id) VALUES ('person', ?1, ?2, ?3)",
                    params![name_key(name), name, anna],
                )
                .unwrap();
        }
        assert_eq!(store.entities().unwrap()[0].aliases, ["Anna E.", "Anna"]);
    }

    #[test]
    fn forgetting_an_entity_keeps_the_episodes() {
        let mut store = Store::open_in_memory().unwrap();
        let id = episode(&mut store, 0, "Mail");
        store
            .record_mentions(id, &[person("Anna Example"), person("Bo Exempel")], at(30))
            .unwrap();
        let anna = store.entities().unwrap()[0].id;
        assert!(store.forget_entity(anna).unwrap());
        assert!(!store.forget_entity(anna).unwrap());
        assert_eq!(names(&store), [("Bo Exempel".to_string(), 1)]);
        assert_eq!(store.episodes(None, 10).unwrap().len(), 1);
    }
}
