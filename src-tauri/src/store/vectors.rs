//! Episode vectors, for searching the journal by meaning (Stage 5). See
//! `migrations/0011_episode_vectors.sql`.

use chrono::{DateTime, Utc};
use rusqlite::params;

use super::{Episode, Result, Store, episode_from_row, sql_limit};

fn to_bytes(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn from_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

/// How alike two vectors point, from -1 to 1; 0 if either is empty or of
/// another length.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut aa, mut bb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa == 0.0 || bb == 0.0 {
        0.0
    } else {
        dot / (aa.sqrt() * bb.sqrt())
    }
}

impl Store {
    /// Episodes without a vector from `model`, oldest first. Vectors from
    /// other models are deleted first: they can't be compared.
    pub fn unvectored_episodes(&self, model: &str, limit: usize) -> Result<Vec<(i64, String)>> {
        self.conn
            .execute("DELETE FROM episode_vectors WHERE model != ?1", [model])?;
        let mut statement = self.conn.prepare(
            "SELECT e.id, e.summary FROM episodes AS e
             WHERE NOT EXISTS (SELECT 1 FROM episode_vectors AS v WHERE v.episode_id = e.id)
             ORDER BY e.id LIMIT ?1",
        )?;
        let rows = statement.query_map([sql_limit(limit)], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Stores vectors; one for an episode forgotten meanwhile is dropped.
    pub fn set_vectors(&mut self, model: &str, vectors: &[(i64, Vec<f32>)]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut stored = 0;
        for (episode, vector) in vectors {
            stored += tx.execute(
                "INSERT OR REPLACE INTO episode_vectors (episode_id, model, vector)
                 SELECT id, ?2, ?3 FROM episodes WHERE id = ?1",
                params![episode, model, to_bytes(vector)],
            )?;
        }
        tx.commit()?;
        Ok(stored)
    }

    /// The episodes closest in meaning to `query`, best first, within
    /// `range` if given. Only vectors from `model` count.
    pub fn nearest_episodes(
        &self,
        model: &str,
        query: &[f32],
        range: Option<(DateTime<Utc>, DateTime<Utc>)>,
        limit: usize,
    ) -> Result<Vec<Episode>> {
        let (from, to) = range.map_or((i64::MIN, i64::MAX), |(from, to)| {
            (from.timestamp_millis(), to.timestamp_millis())
        });
        let mut statement = self.conn.prepare(
            "SELECT v.episode_id, v.vector FROM episode_vectors AS v
             JOIN episodes AS e ON e.id = v.episode_id
             WHERE v.model = ?1 AND e.started_at < ?3 AND e.ended_at >= ?2",
        )?;
        let rows = statement.query_map(params![model, from, to], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut scored: Vec<(f32, i64)> = Vec::new();
        for row in rows {
            let (id, bytes) = row?;
            scored.push((cosine(query, &from_bytes(&bytes)), id));
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(limit);
        let mut episodes = Vec::new();
        for (_, id) in scored {
            let mut statement = self.conn.prepare_cached(
                "SELECT e.id, e.started_at, e.ended_at, e.summary, e.app_names,
                    (SELECT count(*) FROM captures AS c WHERE c.episode_id = e.id)
                 FROM episodes AS e WHERE e.id = ?1",
            )?;
            if let Some(episode) = statement.query_map([id], episode_from_row)?.next() {
                episodes.push(episode?);
            }
        }
        Ok(episodes)
    }

    /// How many episodes have a vector from `model`.
    pub fn vector_count(&self, model: &str) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM episode_vectors WHERE model = ?1",
            [model],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Forget, NewCapture, NewEpisode};
    use chrono::{TimeDelta, TimeZone};

    fn at(minute: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 8, 10, 0, 0).unwrap() + TimeDelta::minutes(minute)
    }

    fn episode(store: &mut Store, minute: i64, app: &str, summary: &str) -> i64 {
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
                    summary: summary.to_string(),
                    model: "example-model".to_string(),
                    created_at: at(minute),
                },
                &[id],
            )
            .unwrap()
            .unwrap()
    }

    #[test]
    fn vectors_round_trip_and_cosine_ranks() {
        assert_eq!(from_bytes(&to_bytes(&[1.5, -2.0, 0.25])), [1.5, -2.0, 0.25]);
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn the_closest_episodes_come_first() {
        let mut store = Store::open_in_memory().unwrap();
        let offer = episode(&mut store, 0, "Mail", "Skrev offerten.");
        let launch = episode(&mut store, 60, "Slack", "Planerade lanseringen.");
        let news = episode(&mut store, 120, "Safari", "Läste nyheter.");
        assert_eq!(store.unvectored_episodes("m", 10).unwrap().len(), 3);
        store
            .set_vectors(
                "m",
                &[
                    (offer, vec![1.0, 0.1, 0.0]),
                    (launch, vec![0.2, 1.0, 0.0]),
                    (news, vec![0.0, 0.0, 1.0]),
                ],
            )
            .unwrap();
        assert!(store.unvectored_episodes("m", 10).unwrap().is_empty());
        let found = store
            .nearest_episodes("m", &[0.9, 0.2, 0.0], None, 2)
            .unwrap();
        assert_eq!(
            found.iter().map(|e| e.id).collect::<Vec<_>>(),
            [offer, launch]
        );
        // Within a time.
        let found = store
            .nearest_episodes("m", &[0.9, 0.2, 0.0], Some((at(30), at(200))), 2)
            .unwrap();
        assert_eq!(found[0].id, launch);
        // Another model's vectors don't count, and are dropped when the model changes.
        assert!(
            store
                .nearest_episodes("other", &[1.0, 0.0, 0.0], None, 5)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.unvectored_episodes("other", 10).unwrap().len(), 3);
        assert_eq!(store.vector_count("m").unwrap(), 0);
    }

    #[test]
    fn an_episode_the_model_cant_take_is_set_aside() {
        let mut store = Store::open_in_memory().unwrap();
        let odd = episode(&mut store, 0, "Mail", "Något modellen inte tar.");
        let fine = episode(&mut store, 60, "Mail", "Skrev offerten.");
        store
            .set_vectors("m", &[(odd, Vec::new()), (fine, vec![1.0, 0.0])])
            .unwrap();
        // Not sent again…
        assert!(store.unvectored_episodes("m", 10).unwrap().is_empty());
        // …and never the closest.
        let found = store.nearest_episodes("m", &[1.0, 0.0], None, 1).unwrap();
        assert_eq!(found[0].id, fine);
    }

    #[test]
    fn vectors_go_with_their_episodes() {
        let mut store = Store::open_in_memory().unwrap();
        let mail = episode(&mut store, 0, "Mail", "Skrev offerten.");
        let slack = episode(&mut store, 60, "Slack", "Planerade lanseringen.");
        store
            .set_vectors("m", &[(mail, vec![1.0, 0.0]), (slack, vec![0.0, 1.0])])
            .unwrap();
        store
            .forget(&Forget::App("com.example.mail".to_string()))
            .unwrap();
        assert_eq!(store.vector_count("m").unwrap(), 1);
        // A vector for an episode forgotten meanwhile isn't stored.
        assert_eq!(
            store.set_vectors("m", &[(mail, vec![1.0, 0.0])]).unwrap(),
            0
        );
    }
}
