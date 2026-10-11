-- What the model learned about each page's entity (Stage 4 step 4, ADR 0008).

-- The facts, one per line; NULL until the model has written them, and again
-- once an episode they drew on is forgotten (then they are rebuilt from the
-- episodes that remain).
ALTER TABLE pages ADD COLUMN facts TEXT;

-- The episodes the facts were written from. A page whose entity has
-- mentions that aren't here is out of date.
CREATE TABLE page_fact_sources (
    page_id    TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    episode_id INTEGER NOT NULL REFERENCES episodes (id) ON DELETE CASCADE,
    PRIMARY KEY (page_id, episode_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX page_fact_sources_by_episode ON page_fact_sources (episode_id);

-- Forgetting an episode the facts drew on forgets the facts (GUARDRAILS §4).
CREATE TRIGGER page_fact_sources_after_delete AFTER DELETE ON page_fact_sources
BEGIN
    UPDATE pages SET facts = NULL WHERE id = old.page_id;
    DELETE FROM page_fact_sources WHERE page_id = old.page_id;
END;

-- Facts the user deleted from a page: never written there again.
CREATE TABLE rejected_facts (
    page_id    TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    key        TEXT NOT NULL, -- normalized
    fact       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (page_id, key)
) STRICT;
