//! Finding the journal episodes a question is about ("Ask", Stage 5 v1).

use chrono::{DateTime, Utc};
use rusqlite::params;

use super::{Episode, Result, Store, episode_from_row, sql_limit};

/// The columns `episode_from_row` reads, for `episodes AS e`.
const EPISODE_COLUMNS: &str = "e.id, e.started_at, e.ended_at, e.summary, e.app_names,
    (SELECT count(*) FROM captures AS c WHERE c.episode_id = e.id)";

impl Store {
    /// Episodes that overlap `from..to`, oldest first, at most `limit`
    /// (the latest ones if there are more).
    pub fn episodes_between(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<Episode>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT * FROM (
                 SELECT {EPISODE_COLUMNS} FROM episodes AS e
                 WHERE e.started_at < ?2 AND e.ended_at >= ?1
                 ORDER BY e.started_at DESC, e.id DESC LIMIT ?3)
             ORDER BY 2, 1"
        ))?;
        let rows = statement.query_map(
            params![
                from.timestamp_millis(),
                to.timestamp_millis(),
                sql_limit(limit)
            ],
            episode_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The newest episodes that mention any of `entities`, newest first,
    /// within `range` if given.
    pub fn episodes_mentioning(
        &self,
        entities: &[i64],
        range: Option<(DateTime<Utc>, DateTime<Utc>)>,
        limit: usize,
    ) -> Result<Vec<Episode>> {
        if entities.is_empty() {
            return Ok(Vec::new());
        }
        let (from, to) = bounds(range);
        // Ids are numbers from the database, so joining them is safe.
        let ids = entities
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut statement = self.conn.prepare(&format!(
            "SELECT {EPISODE_COLUMNS} FROM episodes AS e
             WHERE e.id IN (SELECT episode_id FROM mentions WHERE entity_id IN ({ids}))
               AND e.started_at < ?2 AND e.ended_at >= ?1
             ORDER BY e.started_at DESC, e.id DESC LIMIT ?3"
        ))?;
        let rows = statement.query_map(params![from, to, sql_limit(limit)], episode_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Episodes whose summary has any of `words` (as a word or the start of
    /// one), best matches first, within `range` if given.
    pub fn search_episodes(
        &self,
        words: &[String],
        range: Option<(DateTime<Utc>, DateTime<Utc>)>,
        limit: usize,
    ) -> Result<Vec<Episode>> {
        let terms: Vec<String> = words
            .iter()
            .filter(|w| !w.is_empty())
            .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut statement = self.conn.prepare(&format!(
            "SELECT {EPISODE_COLUMNS} FROM episodes_fts
             JOIN episodes AS e ON e.id = episodes_fts.rowid
             WHERE episodes_fts MATCH ?1 AND e.started_at < ?3 AND e.ended_at >= ?2
             ORDER BY rank, e.started_at DESC LIMIT ?4"
        ))?;
        let (from, to) = bounds(range);
        let rows = statement.query_map(
            params![terms.join(" OR "), from, to, sql_limit(limit)],
            episode_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every name the memory knows, normalized, with its entity.
    pub fn entity_names(&self) -> Result<Vec<(i64, String)>> {
        let mut statement = self.conn.prepare("SELECT entity_id, key FROM aliases")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// `range` in Unix ms, or everything.
fn bounds(range: Option<(DateTime<Utc>, DateTime<Utc>)>) -> (i64, i64) {
    range.map_or((i64::MIN, i64::MAX), |(from, to)| {
        (from.timestamp_millis(), to.timestamp_millis())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{EntityKind, NewCapture, NewEpisode};
    use chrono::{TimeDelta, TimeZone};

    fn at(minute: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 8, 10, 0, 0).unwrap() + TimeDelta::minutes(minute)
    }

    fn episode(store: &mut Store, minute: i64, summary: &str) -> i64 {
        let id = store
            .insert(&NewCapture {
                captured_at: at(minute),
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
                    created_at: at(minute),
                },
                &[id],
            )
            .unwrap()
            .unwrap()
    }

    fn summaries(episodes: &[Episode]) -> Vec<&str> {
        episodes.iter().map(|e| e.summary.as_str()).collect()
    }

    #[test]
    fn episodes_in_a_range_come_oldest_first() {
        let mut store = Store::open_in_memory().unwrap();
        episode(&mut store, 0, "Early");
        episode(&mut store, 60, "Middle");
        episode(&mut store, 120, "Late");
        episode(&mut store, 24 * 60, "Tomorrow");
        let found = store.episodes_between(at(30), at(24 * 60), 10).unwrap();
        assert_eq!(summaries(&found), ["Middle", "Late"]);
        // Over the limit, the latest are kept, still oldest first.
        let found = store.episodes_between(at(0), at(200), 2).unwrap();
        assert_eq!(summaries(&found), ["Middle", "Late"]);
    }

    #[test]
    fn episodes_are_found_by_entity() {
        let mut store = Store::open_in_memory().unwrap();
        let first = episode(&mut store, 0, "Mailed Anna about the Q3 offer.");
        let second = episode(&mut store, 60, "Planned the launch with Bo.");
        episode(&mut store, 120, "Read the news.");
        store
            .record_mentions(
                first,
                &[(EntityKind::Person, "Anna Example".to_string())],
                at(130),
            )
            .unwrap();
        store
            .record_mentions(
                second,
                &[(EntityKind::Person, "Bo Exempel".to_string())],
                at(131),
            )
            .unwrap();
        let names = store.entity_names().unwrap();
        let anna = names
            .iter()
            .find(|(_, key)| key == "anna example")
            .unwrap()
            .0;
        let found = store.episodes_mentioning(&[anna], None, 10).unwrap();
        assert_eq!(summaries(&found), ["Mailed Anna about the Q3 offer."]);
        assert!(store.episodes_mentioning(&[], None, 10).unwrap().is_empty());
        assert!(
            store
                .episodes_mentioning(&[anna], Some((at(30), at(200))), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn episodes_are_found_by_words_and_word_starts() {
        let mut store = Store::open_in_memory().unwrap();
        episode(&mut store, 0, "Skrev offerten för Q3.");
        episode(&mut store, 60, "Planerade lanseringen.");
        episode(&mut store, 120, "Läste nyheter.");
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        let found = store
            .search_episodes(&words(&["offert", "lansering"]), None, 10)
            .unwrap();
        assert_eq!(found.len(), 2);
        let found = store
            .search_episodes(&words(&["offert", "lansering"]), Some((at(30), at(90))), 10)
            .unwrap();
        assert_eq!(summaries(&found), ["Planerade lanseringen."]);
        // Search syntax in a word is plain text.
        assert!(
            store
                .search_episodes(&words(&["\"OR", "NEAR("]), None, 10)
                .unwrap()
                .is_empty()
        );
        assert!(store.search_episodes(&[], None, 10).unwrap().is_empty());
    }
}
