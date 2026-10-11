//! The memory pages' table and their versions. See `migrations/0008_pages.sql`.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Row, params};

use super::{Result, Store};

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
    /// The newest episode the facts took into account.
    pub facts_through: Option<i64>,
}

/// A page whose facts are out of date: never written, rebuilt after a
/// forget, or with newer mentions than the facts took into account.
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
        facts_through: row.get(6)?,
    })
}

const PAGE_COLUMNS: &str = "p.id, p.entity_id, p.path, p.state,
    (SELECT block FROM page_versions WHERE id = p.last_version), p.facts, p.facts_through";

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
             WHERE p.entity_id IS NOT NULL AND p.state = 'ok'
               AND (p.facts IS NULL OR p.facts_through <
                    (SELECT max(episode_id) FROM mentions WHERE entity_id = p.entity_id))
             ORDER BY 8 DESC, p.id
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

    /// Stores what the model learned from `sources`, up to episode
    /// `through`. Refused (`false`) if a source was forgotten meanwhile: the
    /// facts may hold what was forgotten.
    pub fn set_facts(
        &mut self,
        page: &str,
        facts: &[String],
        through: i64,
        sources: &[i64],
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
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
            "UPDATE pages SET facts = ?2, facts_through = ?3 WHERE id = ?1",
            params![page, facts.join("\n"), through],
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
