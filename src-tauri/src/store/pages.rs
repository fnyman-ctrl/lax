//! The memory pages' table and their versions. See `migrations/0008_pages.sql`.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};

use super::{EntityKind, Result, Store};

/// Where a page stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageState {
    Ok,
    /// No file in the memory folder has its id.
    Missing,
    /// The user removed the markers from its file.
    Detached,
    /// The user asked for it to be written again.
    Recreate,
}

impl PageState {
    fn as_str(self) -> &'static str {
        match self {
            PageState::Ok => "ok",
            PageState::Missing => "missing",
            PageState::Detached => "detached",
            PageState::Recreate => "recreate",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "missing" => PageState::Missing,
            "detached" => PageState::Detached,
            "recreate" => PageState::Recreate,
            _ => PageState::Ok,
        }
    }
}

/// A memory page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub id: String,
    /// `None` once its entity is forgotten.
    pub entity_id: Option<i64>,
    /// Relative to the memory folder.
    pub path: String,
    pub state: PageState,
    /// The block Lax last wrote (normalized), if that version still exists.
    pub last_block: Option<String>,
    /// What the model learned, one fact per line; `None` until it has.
    pub facts: Option<Vec<String>>,
    pub kind: PageKind,
    /// When the model last wrote the facts.
    pub facts_at: Option<DateTime<Utc>>,
}

/// What a page is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// A person, organisation or project.
    Entity,
    /// The user: `about-me.md`.
    AboutMe,
}

/// A page whose facts are out of date: never written, rebuilt after a
/// forget, or with mentions the facts haven't seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalePage {
    pub page: Page,
    pub entity_id: i64,
}

/// A fact as compared: case, spacing and a final full stop don't count.
pub fn fact_key(fact: &str) -> String {
    let key = fact
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    key.trim_end_matches(['.', '!', ';']).trim_end().to_string()
}

fn page_from_row(row: &Row) -> rusqlite::Result<Page> {
    Ok(Page {
        id: row.get(0)?,
        entity_id: row.get(1)?,
        path: row.get(2)?,
        state: PageState::parse(&row.get::<_, String>(3)?),
        last_block: row.get(4)?,
        facts: row
            .get::<_, Option<String>>(5)?
            .map(|facts| facts.lines().map(String::from).collect()),
        kind: if row.get::<_, String>(6)? == "about-me" {
            PageKind::AboutMe
        } else {
            PageKind::Entity
        },
        facts_at: row
            .get::<_, Option<i64>>(7)?
            .and_then(DateTime::from_timestamp_millis),
    })
}

const PAGE_COLUMNS: &str = "p.id, p.entity_id, p.path, p.state,
    (SELECT block FROM page_versions WHERE id = p.last_version), p.facts, p.kind, p.facts_at";

/// How many of Lax's versions of a page are kept.
pub const LAX_VERSIONS_KEPT: i64 = 20;

impl Store {
    pub fn pages(&self) -> Result<Vec<Page>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {PAGE_COLUMNS} FROM pages AS p ORDER BY p.path"
        ))?;
        let rows = statement.query_map([], page_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn page_of(&self, entity: i64) -> Result<Option<Page>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {PAGE_COLUMNS} FROM pages AS p WHERE p.entity_id = ?1"),
                [entity],
                page_from_row,
            )
            .optional()?)
    }

    /// The episodes a page's facts were written from.
    pub fn fact_sources(&self, page: &str) -> Result<Vec<i64>> {
        let mut statement = self
            .conn
            .prepare("SELECT episode_id FROM page_fact_sources WHERE page_id = ?1")?;
        let rows = statement.query_map([page], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn about_me_page(&self) -> Result<Option<Page>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {PAGE_COLUMNS} FROM pages AS p WHERE p.kind = 'about-me'"),
                [],
                page_from_row,
            )
            .optional()?)
    }

    /// Starts keeping track of `about-me.md`; its file is written next.
    pub fn add_about_me_page(&self, id: &str, path: &str, now: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO pages (id, entity_id, path, state, kind, created_at)
             VALUES (?1, NULL, ?2, 'ok', 'about-me', ?3)",
            params![id, path, now.timestamp_millis()],
        )?;
        Ok(())
    }

    /// Deletes all but the newest of Lax's versions of a page. For
    /// `about-me.md`, whose block names other entities: a forgotten name
    /// mustn't stay in its history.
    pub fn keep_only_latest_version(&self, page: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM page_versions WHERE page_id = ?1 AND by = 'lax'
             AND id != (SELECT coalesce(last_version, -1) FROM pages WHERE id = ?1)",
            [page],
        )?;
        Ok(())
    }

    /// The most-mentioned entities of `kind` in episodes since `since`, among
    /// those Lax keeps, with their counts.
    pub fn most_mentioned_since(
        &self,
        kind: EntityKind,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<(String, usize)>> {
        let mut statement = self.conn.prepare(
            "SELECT n.name, count(*) FROM entities AS n
             JOIN mentions AS m ON m.entity_id = n.id
             JOIN episodes AS e ON e.id = m.episode_id
             WHERE n.kind = ?1 AND e.started_at >= ?2
               AND (n.kept = 1 OR (SELECT count(DISTINCT date(e2.started_at / 1000, 'unixepoch', 'localtime'))
                                   FROM mentions AS m2 JOIN episodes AS e2 ON e2.id = m2.episode_id
                                   WHERE m2.entity_id = n.id) >= 2)
             GROUP BY n.id ORDER BY count(*) DESC, n.name LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                kind.as_str(),
                since.timestamp_millis(),
                super::sql_limit(limit)
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        let mut list = Vec::new();
        for row in rows {
            let (name, count) = row?;
            list.push((name, usize::try_from(count).unwrap_or(0)));
        }
        Ok(list)
    }

    /// The apps in most episodes since `since`.
    pub fn most_used_apps_since(&self, since: DateTime<Utc>, limit: usize) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT app_names FROM episodes WHERE started_at >= ?1")?;
        let rows =
            statement.query_map([since.timestamp_millis()], |row| row.get::<_, String>(0))?;
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for names in rows {
            for name in names?.lines().filter(|n| !n.is_empty()) {
                *counts.entry(name.to_string()).or_default() += 1;
            }
        }
        let mut apps: Vec<(String, usize)> = counts.into_iter().collect();
        apps.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(apps.into_iter().take(limit).map(|(name, _)| name).collect())
    }

    /// Starts keeping track of a page; its file is written next.
    pub fn add_page(&self, id: &str, entity: i64, path: &str, now: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO pages (id, entity_id, path, state, created_at)
             VALUES (?1, ?2, ?3, 'ok', ?4)",
            params![id, entity, path, now.timestamp_millis()],
        )?;
        Ok(())
    }

    /// Records that Lax wrote `block` (normalized) to the page at `path`,
    /// from `sources`.
    pub fn page_written(
        &mut self,
        id: &str,
        path: &str,
        block: &str,
        sources: &[i64],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO page_versions (page_id, written_at, by, block) VALUES (?1, ?2, 'lax', ?3)",
            params![id, now.timestamp_millis(), block],
        )?;
        let version = tx.last_insert_rowid();
        let mut linked = 0;
        for episode in sources {
            // An episode forgotten meanwhile has no row to point to.
            linked += tx.execute(
                "INSERT INTO page_version_sources (version_id, episode_id)
                 SELECT ?1, id FROM episodes WHERE id = ?2",
                [version, *episode],
            )?;
        }
        // If a source was forgotten meanwhile, the block holds forgotten
        // text: it is written to the file (and replaced on the next sync),
        // but not kept as a version, and not trusted to tell edits apart.
        let version = if linked == sources.len() {
            Some(version)
        } else {
            tx.execute("DELETE FROM page_versions WHERE id = ?1", [version])?;
            None
        };
        tx.execute(
            "UPDATE pages SET path = ?2, state = 'ok', last_version = ?3 WHERE id = ?1",
            params![id, path, version],
        )?;
        // Lax's versions change with every new mention; the latest few are
        // enough to see and undo what it wrote. The user's are all kept.
        tx.execute(
            "DELETE FROM page_versions
             WHERE page_id = ?1 AND by = 'lax' AND id NOT IN (
                 SELECT id FROM page_versions WHERE page_id = ?1 AND by = 'lax'
                 ORDER BY id DESC LIMIT ?2)",
            params![id, LAX_VERSIONS_KEPT],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Pages whose facts are out of date, most-mentioned entities first.
    pub fn stale_pages(&self, limit: usize) -> Result<Vec<StalePage>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {PAGE_COLUMNS}, (SELECT count(*) FROM mentions WHERE entity_id = p.entity_id)
             FROM pages AS p
             WHERE p.kind = 'entity' AND p.entity_id IS NOT NULL AND p.state = 'ok'
               AND (p.facts IS NULL OR EXISTS (
                    SELECT 1 FROM mentions AS m WHERE m.entity_id = p.entity_id
                      AND m.episode_id NOT IN
                          (SELECT episode_id FROM page_fact_sources WHERE page_id = p.id)))
             ORDER BY 9 DESC, p.id
             LIMIT ?1"
        ))?;
        let rows = statement.query_map([super::sql_limit(limit)], page_from_row)?;
        let mut stale = Vec::new();
        for page in rows {
            let page = page?;
            if let Some(entity_id) = page.entity_id {
                stale.push(StalePage { page, entity_id });
            }
        }
        Ok(stale)
    }

    /// The episodes mentioning the page's entity that its facts haven't
    /// seen, oldest first.
    pub fn unseen_mentions(&self, page: &str, entity: i64, limit: usize) -> Result<Vec<i64>> {
        let mut statement = self.conn.prepare(
            "SELECT m.episode_id FROM mentions AS m
             JOIN episodes AS e ON e.id = m.episode_id
             WHERE m.entity_id = ?2 AND m.episode_id NOT IN
                   (SELECT episode_id FROM page_fact_sources WHERE page_id = ?1)
             ORDER BY e.started_at, e.id LIMIT ?3",
        )?;
        let rows = statement.query_map(params![page, entity, super::sql_limit(limit)], |row| {
            row.get(0)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Stores what the model learned from `sources`. Refused (`false`) if
    /// the page or a source was forgotten meanwhile: the facts may hold
    /// what was forgotten.
    pub fn set_facts(
        &mut self,
        page: &str,
        facts: &[String],
        sources: &[i64],
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let page_exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM pages WHERE id = ?1)",
            [page],
            |row| row.get(0),
        )?;
        if !page_exists {
            return Ok(false);
        }
        for episode in sources {
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM episodes WHERE id = ?1)",
                [episode],
                |row| row.get(0),
            )?;
            if !exists {
                return Ok(false);
            }
            tx.execute(
                "INSERT OR IGNORE INTO page_fact_sources (page_id, episode_id) VALUES (?1, ?2)",
                params![page, episode],
            )?;
        }
        let updated = tx.execute(
            "UPDATE pages SET facts = ?2, facts_at = ?3 WHERE id = ?1",
            params![page, facts.join("\n"), now.timestamp_millis()],
        )?;
        tx.commit()?;
        Ok(updated > 0)
    }

    /// The user deleted `fact` from the page: it goes, and is never written
    /// there again.
    pub fn reject_fact(&self, page: &str, fact: &str, now: DateTime<Utc>) -> Result<()> {
        let key = fact_key(fact);
        self.conn.execute(
            "INSERT OR IGNORE INTO rejected_facts (page_id, key, fact, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![page, key, fact, now.timestamp_millis()],
        )?;
        let facts: Option<String> = self
            .conn
            .query_row("SELECT facts FROM pages WHERE id = ?1", [page], |row| {
                row.get(0)
            })
            .optional()?
            .flatten();
        if let Some(facts) = facts {
            let kept: Vec<&str> = facts.lines().filter(|f| fact_key(f) != key).collect();
            self.conn.execute(
                "UPDATE pages SET facts = ?2 WHERE id = ?1",
                params![page, kept.join("\n")],
            )?;
        }
        Ok(())
    }

    /// Facts the user deleted from a page.
    pub fn rejected_facts(&self, page: &str) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT fact FROM rejected_facts WHERE page_id = ?1 ORDER BY created_at")?;
        let rows = statement.query_map([page], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Keeps the user's edit of Lax's block as a version of theirs.
    pub fn page_edited_by_user(&self, id: &str, block: &str, now: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO page_versions (page_id, written_at, by, block) VALUES (?1, ?2, 'user', ?3)",
            params![id, now.timestamp_millis(), block],
        )?;
        Ok(())
    }

    pub fn set_page_state(&self, id: &str, state: PageState, path: Option<&str>) -> Result<()> {
        match path {
            Some(path) => self.conn.execute(
                "UPDATE pages SET state = ?2, path = ?3 WHERE id = ?1",
                params![id, state.as_str(), path],
            )?,
            None => self.conn.execute(
                "UPDATE pages SET state = ?2 WHERE id = ?1",
                params![id, state.as_str()],
            )?,
        };
        Ok(())
    }

    /// Stops keeping track of a page and deletes its versions.
    pub fn delete_page(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM pages WHERE id = ?1", [id])?;
        Ok(())
    }

    /// How many versions a page has, by Lax and by the user.
    #[cfg(test)]
    pub fn page_versions(&self, id: &str) -> Result<(usize, usize)> {
        let count = |by: &str| -> Result<usize> {
            let n: i64 = self.conn.query_row(
                "SELECT count(*) FROM page_versions WHERE page_id = ?1 AND by = ?2",
                params![id, by],
                |row| row.get(0),
            )?;
            Ok(usize::try_from(n).unwrap_or(0))
        };
        Ok((count("lax")?, count("user")?))
    }
}
