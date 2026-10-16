//! The memory's tables (Stage 4, ADR 0008): entities, the names they go by,
//! the episodes that mention them, and what the user decided about them.
//! See `migrations/0006_memory.sql` and `0007_memory_choices.sql`.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::{Result, Store, sql_limit};

const MEMORY_KEY: &str = "memory_enabled";
const OWN_NAMES_KEY: &str = "own_names";

/// An entity is new, and Lax only notes it, until it is mentioned on this
/// many different days, or the user keeps it (ADR 0008).
pub const DAYS_TO_KEEP: usize = 2;

/// A new name not mentioned for this many days fades (ADR 0013), or for
/// `FADE_DAYS_OFTEN` days once it was mentioned `OFTEN` times.
pub const FADE_DAYS: i64 = 14;
pub const FADE_DAYS_OFTEN: i64 = 28;
pub const OFTEN: i64 = 3;

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

    pub fn parse(value: &str) -> Option<Self> {
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
    pub first_seen: DateTime<Utc>,
    /// The user chose to keep it.
    pub kept: bool,
}

impl Entity {
    /// Still new: not kept, and not yet mentioned on `DAYS_TO_KEEP` days.
    pub fn is_new(&self) -> bool {
        !self.kept && self.days < DAYS_TO_KEEP
    }
}

/// A name on the "don't learn" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DontLearn {
    pub kind: EntityKind,
    pub name: String,
}

/// What became of a rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Renamed {
    Done,
    NotFound,
    /// Another entity of the same kind goes by that name: merge instead.
    Taken {
        by: String,
    },
    /// An empty name.
    Empty,
}

/// What became of a merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merged {
    Done,
    NotFound,
    /// A person can't be merged into a project, and so on.
    DifferentKinds,
}

impl Store {
    pub fn memory_enabled(&self) -> Result<bool> {
        Ok(self.setting(MEMORY_KEY)?.as_deref() == Some("1"))
    }

    pub fn set_memory_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting(MEMORY_KEY, if enabled { "1" } else { "0" })
    }

    /// Other names the user goes by, as they typed them.
    pub fn own_names(&self) -> Result<Vec<String>> {
        Ok(self
            .setting(OWN_NAMES_KEY)?
            .unwrap_or_default()
            .lines()
            .filter(|n| !n.is_empty())
            .map(String::from)
            .collect())
    }

    pub fn set_own_names(&self, names: &[String]) -> Result<()> {
        self.set_setting(OWN_NAMES_KEY, &names.join("\n"))
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
            let unwanted: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM dont_learn WHERE kind = ?1 AND key = ?2)",
                params![kind.as_str(), key],
                |row| row.get(0),
            )?;
            if unwanted {
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
                    max(e.ended_at),
                    n.kept,
                    min(e.started_at)
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
                    kept: row.get::<_, i64>(7)? != 0,
                    first_seen: DateTime::from_timestamp_millis(row.get(8)?).unwrap_or_default(),
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
    /// episodes stay. With `dont_learn`, its names go on the "don't learn"
    /// list first, so later episodes can't bring it back. Returns whether
    /// it existed.
    ///
    /// The rows are short, so this skips the full rebuild of
    /// [`Store::forget`]: `secure_delete` overwrites the freed pages and the
    /// checkpoint empties the write-ahead log, as for [`Store::expire`].
    pub fn forget_entity(&mut self, id: i64, dont_learn: bool, now: DateTime<Utc>) -> Result<bool> {
        let tx = self.conn.transaction()?;
        if dont_learn {
            tx.execute(
                "INSERT OR IGNORE INTO dont_learn (kind, key, name, created_at)
                 SELECT kind, key, name, ?2 FROM aliases WHERE entity_id = ?1 ORDER BY rowid",
                params![id, now.timestamp_millis()],
            )?;
        }
        // Once the pages are in the database (ADR 0011), the page goes in
        // the same transaction, the user's text on it too: the forget
        // dialog showed it first. Before, the next page round takes Lax's
        // part out of the file.
        tx.execute(
            "DELETE FROM pages WHERE entity_id = ?1
             AND EXISTS (SELECT 1 FROM settings WHERE key = ?2 AND value = '1')",
            params![id, super::pages::PAGES_IN_DATABASE_KEY],
        )?;
        let deleted = tx.execute("DELETE FROM entities WHERE id = ?1", [id])?;
        tx.commit()?;
        if deleted > 0 {
            self.checkpoint()?;
        }
        Ok(deleted > 0)
    }

    /// Keeps an entity even though it is still new. Returns whether it
    /// exists.
    pub fn keep_entity(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("UPDATE entities SET kept = 1 WHERE id = ?1", [id])?
            > 0)
    }

    /// Gives an entity a new name. The new name also becomes one of its
    /// names, so later episodes that use it find the entity, and comes off
    /// the "don't learn" list, since the user wants it; the old names stay.
    /// Refused if another entity of its kind goes by the name.
    pub fn rename_entity(&mut self, id: i64, name: &str) -> Result<Renamed> {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        let key = name_key(&name);
        if key.is_empty() {
            return Ok(Renamed::Empty);
        }
        let tx = self.conn.transaction()?;
        let Some(kind): Option<String> = tx
            .query_row("SELECT kind FROM entities WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?
        else {
            return Ok(Renamed::NotFound);
        };
        let owner: Option<(i64, String)> = tx
            .query_row(
                "SELECT a.entity_id, e.name FROM aliases AS a
                 JOIN entities AS e ON e.id = a.entity_id
                 WHERE a.kind = ?1 AND a.key = ?2",
                params![kind, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match owner {
            Some((other, by)) if other != id => return Ok(Renamed::Taken { by }),
            Some(_) => {}
            None => {
                tx.execute(
                    "INSERT INTO aliases (kind, key, name, entity_id) VALUES (?1, ?2, ?3, ?4)",
                    params![kind, key, name, id],
                )?;
            }
        }
        tx.execute(
            "DELETE FROM dont_learn WHERE kind = ?1 AND key = ?2",
            params![kind, key],
        )?;
        tx.execute(
            "UPDATE entities SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        tx.commit()?;
        Ok(Renamed::Done)
    }

    /// Merges entity `from` into `into`: its names become `into`'s other
    /// names, its mentions become `into`'s, and `from` is gone. Kept if
    /// either was.
    pub fn merge_entities(&mut self, from: i64, into: i64, now: DateTime<Utc>) -> Result<Merged> {
        if from == into {
            return Ok(Merged::NotFound);
        }
        let tx = self.conn.transaction()?;
        let kind = |id: i64| {
            tx.query_row("SELECT kind FROM entities WHERE id = ?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .optional()
        };
        let (Some(from_kind), Some(into_kind)) = (kind(from)?, kind(into)?) else {
            return Ok(Merged::NotFound);
        };
        if from_kind != into_kind {
            return Ok(Merged::DifferentKinds);
        }
        tx.execute(
            "UPDATE aliases SET entity_id = ?2 WHERE entity_id = ?1",
            [from, into],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO mentions (entity_id, episode_id)
             SELECT ?2, episode_id FROM mentions WHERE entity_id = ?1",
            [from, into],
        )?;
        tx.execute(
            "UPDATE entities SET kept = max(kept, (SELECT kept FROM entities WHERE id = ?1))
             WHERE id = ?2",
            [from, into],
        )?;
        carry_page(&tx, from, into, now)?;
        tx.execute("DELETE FROM entities WHERE id = ?1", [from])?;
        tx.commit()?;
        Ok(Merged::Done)
    }

    /// Deletes the new names that faded (ADR 0013): not kept, mentioned on
    /// fewer than `DAYS_TO_KEEP` days, without a page, and not mentioned for
    /// `FADE_DAYS` (or `FADE_DAYS_OFTEN`) days before the newest journal
    /// entry, not the clock, so a holiday or a pause doesn't make names fade.
    /// Deleted like a forget, with their names and mentions, but not put on
    /// the "don't learn" list: a name that comes back starts over as new.
    /// Returns how many faded.
    pub fn fade_new_names(&mut self) -> Result<usize> {
        const DAY_MS: i64 = 24 * 60 * 60 * 1000;
        let faded = self.conn.execute(
            "DELETE FROM entities WHERE id IN (
                 SELECT n.id FROM entities AS n
                 JOIN mentions AS m ON m.entity_id = n.id
                 JOIN episodes AS e ON e.id = m.episode_id
                 WHERE n.kept = 0
                   AND NOT EXISTS (SELECT 1 FROM pages AS p WHERE p.entity_id = n.id)
                 GROUP BY n.id
                 HAVING count(DISTINCT date(e.started_at / 1000, 'unixepoch', 'localtime')) < ?1
                    AND max(e.ended_at) < (SELECT max(ended_at) FROM episodes)
                        - CASE WHEN count(*) >= ?2 THEN ?3 ELSE ?4 END
             )",
            params![
                DAYS_TO_KEEP as i64,
                OFTEN,
                FADE_DAYS_OFTEN * DAY_MS,
                FADE_DAYS * DAY_MS
            ],
        )?;
        if faded > 0 {
            self.checkpoint()?;
        }
        Ok(faded)
    }

    /// The "don't learn" list, newest first.
    pub fn dont_learn(&self) -> Result<Vec<DontLearn>> {
        let mut statement = self
            .conn
            .prepare("SELECT kind, name FROM dont_learn ORDER BY created_at DESC, rowid")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut list = Vec::new();
        for row in rows {
            let (kind, name) = row?;
            if let Some(kind) = EntityKind::parse(&kind) {
                list.push(DontLearn { kind, name });
            }
        }
        Ok(list)
    }

    /// Takes a name off the "don't learn" list, so Lax may learn it again
    /// from new episodes. Returns whether it was there.
    pub fn allow_learning(&self, kind: EntityKind, name: &str) -> Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM dont_learn WHERE kind = ?1 AND key = ?2",
            params![kind.as_str(), name_key(name)],
        )?;
        if removed > 0 {
            self.checkpoint()?;
        }
        Ok(removed > 0)
    }

    /// Forgets everything the memory noted and turns it off. Episodes it
    /// already read stay read, so turning it on again starts with new ones.
    /// The "don't learn" list stays: it is the user's own choice. Returns
    /// how many entities were forgotten.
    pub fn forget_memory(&mut self) -> Result<usize> {
        self.set_memory_enabled(false)?;
        let in_database = self.pages_in_database()?;
        // All of it at once, or nothing.
        let tx = self.conn.transaction()?;
        // Once the pages are in the database (ADR 0011), every page goes now,
        // About me and the user's text on them included, as the
        // confirmation says.
        if in_database {
            tx.execute("DELETE FROM pages", [])?;
        }
        let deleted = tx.execute("DELETE FROM entities", [])?;
        // Before: the About me page is something the memory noted too: its
        // facts go, and its page is handed to the next sync like a forgotten
        // entity's, which takes Lax's part out of the file.
        tx.execute(
            "DELETE FROM page_fact_sources
             WHERE page_id IN (SELECT id FROM pages WHERE kind = 'about-me')",
            [],
        )?;
        tx.execute(
            "UPDATE pages SET kind = 'entity', entity_id = NULL, facts = NULL
             WHERE kind = 'about-me'",
            [],
        )?;
        tx.commit()?;
        if deleted > 0 {
            self.checkpoint()?;
        }
        Ok(deleted)
    }

    /// Forgets the people noted under any of `names`: the user's own names,
    /// which Lax shouldn't have noted. Returns how many.
    pub fn forget_people_named(&mut self, names: &[String], now: DateTime<Utc>) -> Result<usize> {
        let mut forgotten = 0;
        for name in names {
            let entity: Option<i64> = self
                .conn
                .query_row(
                    "SELECT entity_id FROM aliases WHERE kind = 'person' AND key = ?1",
                    [name_key(name)],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(entity) = entity
                && self.forget_entity(entity, false, now)?
            {
                forgotten += 1;
            }
        }
        Ok(forgotten)
    }
}

/// What the user wrote on the page of `from`, kept when `from` is merged
/// into `into`: the merged entity is deleted, and its page would go with
/// it as a forgotten one's does. If `into` has no page with text, `from`'s
/// page becomes its page, whole. Otherwise the user's text on it (outside
/// Lax's block, the title left out) is added at the end of `into`'s page,
/// under a heading, and saved as a version of theirs.
fn carry_page(
    tx: &rusqlite::Transaction<'_>,
    from: i64,
    into: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    let page_of = |entity: i64| {
        tx.query_row(
            "SELECT id, text FROM pages WHERE entity_id = ?1",
            [entity],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
    };
    let Some((from_page, from_text)) = page_of(from)? else {
        return Ok(());
    };
    let move_page = |tx: &rusqlite::Transaction<'_>| -> Result<()> {
        tx.execute(
            "UPDATE pages SET entity_id = ?2 WHERE id = ?1",
            params![from_page, into],
        )?;
        Ok(())
    };
    match page_of(into)? {
        None => move_page(tx)?,
        // Pages still in files: the file keeps the user's text when Lax
        // takes its block out.
        Some(_) if from_text.is_none() => {}
        // A page without text has nothing of the user's on it.
        Some((into_page, None)) => {
            tx.execute("DELETE FROM pages WHERE id = ?1", [&into_page])?;
            move_page(tx)?;
        }
        Some((into_page, Some(into_text))) => {
            let from_text = from_text.unwrap_or_default();
            let notes =
                crate::pages::block::without_block(&from_text, &from_page).unwrap_or(from_text);
            if crate::pages::block::only_a_title(&notes) {
                return Ok(());
            }
            let mut lines = notes.trim().lines().peekable();
            if lines.peek().is_some_and(|line| line.starts_with("# ")) {
                lines.next();
            }
            let notes = lines.collect::<Vec<_>>().join("\n");
            let name: String =
                tx.query_row("SELECT name FROM entities WHERE id = ?1", [from], |row| {
                    row.get(0)
                })?;
            let heading = crate::pages::block::defuse(&name.replace(['\n', '\r'], " "));
            let text = format!(
                "{}\n\n## From the page of {heading}\n\n{}\n",
                into_text.trim_end(),
                crate::pages::block::defuse(notes.trim())
            );
            tx.execute(
                "UPDATE pages SET text = ?2 WHERE id = ?1",
                params![into_page, text],
            )?;
            tx.execute(
                "INSERT INTO page_edits (page_id, saved_at, text) VALUES (?1, ?2, ?3)",
                params![into_page, now.timestamp_millis(), text],
            )?;
            tx.execute(
                "DELETE FROM page_edits WHERE page_id = ?1 AND id NOT IN
                 (SELECT id FROM page_edits WHERE page_id = ?1 ORDER BY id DESC LIMIT ?2)",
                params![into_page, super::pages::USER_EDITS_KEPT],
            )?;
        }
    }
    Ok(())
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

    const DAY: i64 = 24 * 60;

    /// Mentions `name` in a new episode on `day` (at 10:00 UTC).
    fn mention_on(store: &mut Store, day: i64, name: &str) {
        let id = episode(store, day * DAY, "Mail");
        store
            .record_mentions(id, &[person(name)], at(day * DAY + 1))
            .unwrap();
    }

    fn faded(store: &mut Store) -> (usize, Vec<String>) {
        let count = store.fade_new_names().unwrap();
        let left = names(store).into_iter().map(|(name, _)| name).collect();
        (count, left)
    }

    #[test]
    fn new_names_fade_two_weeks_before_the_newest_entry() {
        let mut store = Store::open_in_memory().unwrap();
        mention_on(&mut store, 0, "Anna Example");
        mention_on(&mut store, 13, "Bo Exempel");
        // The newest entry is 13 days after Anna: she stays.
        assert_eq!(faded(&mut store).0, 0);
        mention_on(&mut store, 15, "Bo Exempel");
        let (count, left) = faded(&mut store);
        assert_eq!((count, left), (1, vec!["Bo Exempel".to_string()]));
        // Not on the "don't learn" list: she comes back as new.
        assert!(store.dont_learn().unwrap().is_empty());
        mention_on(&mut store, 16, "Anna Example");
        assert!(names(&store).iter().any(|(name, _)| name == "Anna Example"));
    }

    #[test]
    fn the_newest_entry_counts_not_the_clock() {
        let mut store = Store::open_in_memory().unwrap();
        mention_on(&mut store, 0, "Anna Example");
        // Weeks of nothing (a holiday, a pause): no newer entry, no fading.
        assert_eq!(faded(&mut store).0, 0);
    }

    #[test]
    fn names_mentioned_often_get_four_weeks() {
        let mut store = Store::open_in_memory().unwrap();
        for _ in 0..3 {
            mention_on(&mut store, 0, "Anna Example");
        }
        mention_on(&mut store, 20, "Bo Exempel");
        assert_eq!(faded(&mut store).0, 0);
        mention_on(&mut store, 29, "Bo Exempel");
        assert_eq!(faded(&mut store).0, 1);
    }

    #[test]
    fn kept_names_names_seen_on_two_days_and_names_with_a_page_stay() {
        let mut store = Store::open_in_memory().unwrap();
        mention_on(&mut store, 0, "Anna Example");
        mention_on(&mut store, 0, "Bo Exempel");
        mention_on(&mut store, 1, "Bo Exempel");
        mention_on(&mut store, 0, "Cecilia Example");
        mention_on(&mut store, 0, "Dan Example");
        let id = |store: &Store, name: &str| {
            store
                .entities()
                .unwrap()
                .into_iter()
                .find(|e| e.name == name)
                .unwrap()
                .id
        };
        assert!(store.keep_entity(id(&store, "Anna Example")).unwrap());
        // A page without the two days: its episodes were forgotten.
        store
            .conn
            .execute(
                "INSERT INTO pages (id, entity_id, path, created_at) VALUES ('p1', ?1, 'x.md', 0)",
                [id(&store, "Cecilia Example")],
            )
            .unwrap();
        mention_on(&mut store, 30, "Erik Example");
        let (count, mut left) = faded(&mut store);
        left.sort();
        assert_eq!(count, 1, "only Dan fades");
        assert_eq!(
            left,
            [
                "Anna Example",
                "Bo Exempel",
                "Cecilia Example",
                "Erik Example"
            ]
        );
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
        assert!(store.forget_entity(anna, false, at(40)).unwrap());
        assert!(!store.forget_entity(anna, false, at(41)).unwrap());
        assert_eq!(names(&store), [("Bo Exempel".to_string(), 1)]);
        assert_eq!(store.episodes(None, 10).unwrap().len(), 1);
    }

    fn id_of(store: &Store, name: &str) -> i64 {
        store
            .entities()
            .unwrap()
            .into_iter()
            .find(|e| e.name == name)
            .unwrap()
            .id
    }

    #[test]
    fn an_entity_is_new_until_seen_on_two_days_or_kept() {
        let mut store = Store::open_in_memory().unwrap();
        let today = episode(&mut store, 0, "Mail");
        store
            .record_mentions(
                today,
                &[person("Anna Example"), person("Bo Exempel")],
                at(10),
            )
            .unwrap();
        assert!(store.entities().unwrap().iter().all(Entity::is_new));

        let tomorrow = episode(&mut store, 24 * 60, "Mail");
        store
            .record_mentions(tomorrow, &[person("Anna Example")], at(24 * 60 + 10))
            .unwrap();
        let bo = id_of(&store, "Bo Exempel");
        assert!(store.keep_entity(bo).unwrap());
        assert!(!store.entities().unwrap().iter().any(Entity::is_new));
    }

    #[test]
    fn forgotten_with_dont_learn_never_comes_back() {
        let mut store = Store::open_in_memory().unwrap();
        let first = episode(&mut store, 0, "Mail");
        store
            .record_mentions(first, &[person("Anna Example")], at(10))
            .unwrap();
        let anna = id_of(&store, "Anna Example");
        store.rename_entity(anna, "Anna E.").unwrap();
        assert!(store.forget_entity(anna, true, at(11)).unwrap());
        assert_eq!(
            store.dont_learn().unwrap(),
            [
                DontLearn {
                    kind: EntityKind::Person,
                    name: "Anna Example".to_string()
                },
                DontLearn {
                    kind: EntityKind::Person,
                    name: "Anna E.".to_string()
                },
            ]
        );

        // Under either name, in any case, she isn't learned again; a project
        // of the same name still is.
        let second = episode(&mut store, 20, "Mail");
        store
            .record_mentions(
                second,
                &[
                    person("anna e."),
                    person("ANNA EXAMPLE"),
                    (EntityKind::Project, "Anna Example".to_string()),
                ],
                at(30),
            )
            .unwrap();
        let left = store.entities().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].kind, EntityKind::Project);

        // Allowed again, she is learned from new episodes.
        assert!(
            store
                .allow_learning(EntityKind::Person, "Anna Example")
                .unwrap()
        );
        assert!(
            !store
                .allow_learning(EntityKind::Person, "Anna Example")
                .unwrap()
        );
        let third = episode(&mut store, 40, "Mail");
        store
            .record_mentions(third, &[person("Anna Example")], at(50))
            .unwrap();
        assert_eq!(store.entities().unwrap().len(), 2);
    }

    #[test]
    fn forgetting_without_dont_learn_keeps_no_names() {
        let mut store = Store::open_in_memory().unwrap();
        let id = episode(&mut store, 0, "Mail");
        store
            .record_mentions(id, &[person("Anna Example")], at(10))
            .unwrap();
        let anna = id_of(&store, "Anna Example");
        store.forget_entity(anna, false, at(11)).unwrap();
        assert!(store.dont_learn().unwrap().is_empty());
    }

    #[test]
    fn deleting_everything_clears_the_dont_learn_list() {
        let mut store = Store::open_in_memory().unwrap();
        let id = episode(&mut store, 0, "Mail");
        store
            .record_mentions(id, &[person("Anna Example")], at(10))
            .unwrap();
        let anna = id_of(&store, "Anna Example");
        store.forget_entity(anna, true, at(11)).unwrap();
        store.forget_memory().unwrap();
        assert_eq!(store.dont_learn().unwrap().len(), 1);
        store.forget(&crate::store::Forget::Everything).unwrap();
        assert!(store.dont_learn().unwrap().is_empty());
    }

    #[test]
    fn a_new_name_is_found_by_later_episodes() {
        let mut store = Store::open_in_memory().unwrap();
        let first = episode(&mut store, 0, "Mail");
        store
            .record_mentions(first, &[person("Anna")], at(10))
            .unwrap();
        let anna = id_of(&store, "Anna");
        assert_eq!(
            store.rename_entity(anna, "  Anna   Example ").unwrap(),
            Renamed::Done
        );
        let second = episode(&mut store, 20, "Mail");
        store
            .record_mentions(second, &[person("anna example")], at(30))
            .unwrap();
        let entities = store.entities().unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].name, "Anna Example");
        assert_eq!(entities[0].aliases, ["Anna"]);
        assert_eq!(entities[0].mentions, 2);
        // Back to an old name it already has.
        assert_eq!(store.rename_entity(anna, "Anna").unwrap(), Renamed::Done);
        assert_eq!(store.entities().unwrap()[0].aliases, ["Anna Example"]);
    }

    #[test]
    fn a_name_taken_by_another_entity_is_refused() {
        let mut store = Store::open_in_memory().unwrap();
        let id = episode(&mut store, 0, "Mail");
        store
            .record_mentions(id, &[person("Anna Example"), person("Anna")], at(10))
            .unwrap();
        let anna = id_of(&store, "Anna");
        assert_eq!(
            store.rename_entity(anna, "anna example").unwrap(),
            Renamed::Taken {
                by: "Anna Example".to_string()
            }
        );
        assert_eq!(store.rename_entity(9999, "Bo").unwrap(), Renamed::NotFound);
    }

    #[test]
    fn merging_moves_names_and_mentions() {
        let mut store = Store::open_in_memory().unwrap();
        let first = episode(&mut store, 0, "Mail");
        let second = episode(&mut store, 24 * 60, "Mail");
        store
            .record_mentions(first, &[person("Anna Example"), person("Anna")], at(10))
            .unwrap();
        store
            .record_mentions(second, &[person("Anna")], at(24 * 60 + 10))
            .unwrap();
        let full = id_of(&store, "Anna Example");
        let short = id_of(&store, "Anna");
        store.keep_entity(short).unwrap();

        assert_eq!(
            store.merge_entities(short, full, at(50)).unwrap(),
            Merged::Done
        );
        let entities = store.entities().unwrap();
        assert_eq!(entities.len(), 1);
        let anna = &entities[0];
        assert_eq!(anna.name, "Anna Example");
        assert_eq!(anna.aliases, ["Anna"]);
        // The episode that named both counts once.
        assert_eq!(anna.mentions, 2);
        assert_eq!(anna.days, 2);
        assert!(anna.kept);

        // Later episodes under the merged name find it.
        let third = episode(&mut store, 30, "Mail");
        store
            .record_mentions(third, &[person("anna")], at(40))
            .unwrap();
        assert_eq!(store.entities().unwrap()[0].mentions, 3);
        assert_eq!(
            store.merge_entities(short, full, at(50)).unwrap(),
            Merged::NotFound
        );
        assert_eq!(
            store.merge_entities(full, full, at(50)).unwrap(),
            Merged::NotFound
        );
    }

    #[test]
    fn only_the_same_kind_can_be_merged() {
        let mut store = Store::open_in_memory().unwrap();
        let id = episode(&mut store, 0, "Mail");
        store
            .record_mentions(
                id,
                &[
                    person("Anna Example"),
                    (EntityKind::Project, "Q3 lansering".to_string()),
                ],
                at(10),
            )
            .unwrap();
        let anna = id_of(&store, "Anna Example");
        let project = id_of(&store, "Q3 lansering");
        assert_eq!(
            store.merge_entities(anna, project, at(50)).unwrap(),
            Merged::DifferentKinds
        );
        assert_eq!(store.entities().unwrap().len(), 2);
    }

    #[test]
    fn a_name_the_user_gives_comes_off_the_dont_learn_list() {
        let mut store = Store::open_in_memory().unwrap();
        let first = episode(&mut store, 0, "Mail");
        store
            .record_mentions(first, &[person("Anna Example"), person("Anna")], at(10))
            .unwrap();
        let full = id_of(&store, "Anna Example");
        store.forget_entity(full, true, at(11)).unwrap();
        let anna = id_of(&store, "Anna");
        assert_eq!(
            store.rename_entity(anna, "Anna Example").unwrap(),
            Renamed::Done
        );
        assert!(store.dont_learn().unwrap().is_empty());
        let second = episode(&mut store, 20, "Mail");
        store
            .record_mentions(second, &[person("Anna Example")], at(30))
            .unwrap();
        assert_eq!(store.entities().unwrap()[0].mentions, 2);
        assert_eq!(store.rename_entity(anna, "   ").unwrap(), Renamed::Empty);
    }
}
