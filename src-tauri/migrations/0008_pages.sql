-- Memory pages (Stage 4 step 3, ADR 0008): one Markdown file per entity in
-- the memory folder (~/Lax). The files are the pages; this keeps track of
-- them and of every version Lax wrote or found.
CREATE TABLE pages (
    id           TEXT PRIMARY KEY, -- in the page's begin marker
    -- NULL once the entity is forgotten: Lax then takes its block out.
    entity_id    INTEGER UNIQUE REFERENCES entities (id) ON DELETE SET NULL,
    path         TEXT NOT NULL,    -- relative to the memory folder, where last seen
    -- ok; missing (no file has the id); detached (the user removed the
    -- markers); recreate (the user asked for it back).
    state        TEXT NOT NULL DEFAULT 'ok'
                 CHECK (state IN ('ok', 'missing', 'detached', 'recreate')),
    -- The block Lax last wrote, to tell the user's edits from its own. NULL
    -- when that version was deleted because it drew on forgotten episodes.
    last_version INTEGER REFERENCES page_versions (id) ON DELETE SET NULL,
    created_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE page_versions (
    id         INTEGER PRIMARY KEY,
    page_id    TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    written_at INTEGER NOT NULL,
    by         TEXT NOT NULL CHECK (by IN ('lax', 'user')),
    block      TEXT NOT NULL -- normalized
) STRICT;

CREATE INDEX page_versions_by_page ON page_versions (page_id);

-- The episodes a version of Lax's was written from. When one is forgotten,
-- the version goes too, so forgotten text doesn't live on in the history
-- (GUARDRAILS §4).
CREATE TABLE page_version_sources (
    version_id INTEGER NOT NULL REFERENCES page_versions (id) ON DELETE CASCADE,
    episode_id INTEGER NOT NULL REFERENCES episodes (id) ON DELETE CASCADE,
    PRIMARY KEY (version_id, episode_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX page_version_sources_by_episode ON page_version_sources (episode_id);

CREATE TRIGGER page_version_sources_after_delete AFTER DELETE ON page_version_sources
BEGIN
    DELETE FROM page_versions WHERE id = old.version_id;
END;
