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
}

fn page_from_row(row: &Row) -> rusqlite::Result<Page> {
    Ok(Page {
        id: row.get(0)?,
        entity_id: row.get(1)?,
        path: row.get(2)?,
        state: PageState::parse(&row.get::<_, String>(3)?),
        last_block: row.get(4)?,
    })
}

const PAGE_COLUMNS: &str = "p.id, p.entity_id, p.path, p.state,
    (SELECT block FROM page_versions WHERE id = p.last_version)";

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
