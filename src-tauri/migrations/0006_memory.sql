-- The memory (Stage 4, ADR 0008): the people, organisations and projects
-- named in journal episodes. All of it is derived from episodes: forgetting
-- an episode removes its mentions, and an entity left without mentions is
-- deleted with it (GUARDRAILS §4).
CREATE TABLE entities (
    id         INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('person', 'organisation', 'project')),
    name       TEXT NOT NULL, -- as first seen
    created_at INTEGER NOT NULL
) STRICT;

-- Every name an entity has been seen under. `key` is the name normalized
-- (case-folded, whitespace collapsed, diacritics kept), and a name belongs
-- to one entity of its kind.
CREATE TABLE aliases (
    kind      TEXT NOT NULL,
    key       TEXT NOT NULL,
    name      TEXT NOT NULL, -- as first seen
    entity_id INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    PRIMARY KEY (kind, key)
) STRICT;

CREATE INDEX aliases_by_entity ON aliases (entity_id);

CREATE TABLE mentions (
    entity_id  INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    episode_id INTEGER NOT NULL REFERENCES episodes (id) ON DELETE CASCADE,
    PRIMARY KEY (entity_id, episode_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX mentions_by_episode ON mentions (episode_id);

-- An entity exists only through its mentions: when the last one goes (its
-- episodes were forgotten), so does the entity, with its aliases.
CREATE TRIGGER mentions_after_delete AFTER DELETE ON mentions
WHEN NOT EXISTS (SELECT 1 FROM mentions WHERE entity_id = old.entity_id)
BEGIN
    DELETE FROM entities WHERE id = old.entity_id;
END;

-- When the memory read an episode (Unix ms UTC); NULL until it has.
ALTER TABLE episodes ADD COLUMN recognized_at INTEGER;
CREATE INDEX episodes_unrecognized ON episodes (id) WHERE recognized_at IS NULL;
