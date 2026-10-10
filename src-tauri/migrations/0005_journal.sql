-- The journal (Stage 3): short episodes written by the chosen model from
-- raw captures. Episodes are what the rest of Lax reads, and they are kept
-- until the user deletes them; forgetting captures deletes the episodes made
-- from them (GUARDRAILS §4).
CREATE TABLE episodes (
    id         INTEGER PRIMARY KEY,
    started_at INTEGER NOT NULL, -- first capture in it, Unix ms UTC
    ended_at   INTEGER NOT NULL, -- last capture in it
    summary    TEXT NOT NULL,
    apps       TEXT NOT NULL DEFAULT '', -- bundle ids, one per line
    app_names  TEXT NOT NULL DEFAULT '', -- their names, one per line
    model      TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX episodes_by_time ON episodes (started_at);

-- Which episode a capture was summarized into; NULL until then.
ALTER TABLE captures ADD COLUMN episode_id INTEGER REFERENCES episodes (id) ON DELETE SET NULL;
CREATE INDEX captures_unsummarized ON captures (captured_at) WHERE episode_id IS NULL;
CREATE INDEX captures_by_episode ON captures (episode_id);

CREATE VIRTUAL TABLE episodes_fts USING fts5(
    summary,
    content = 'episodes',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 0'
);

INSERT INTO episodes_fts (episodes_fts, rank) VALUES ('secure-delete', 1);

CREATE TRIGGER episodes_after_insert AFTER INSERT ON episodes BEGIN
    INSERT INTO episodes_fts (rowid, summary) VALUES (new.id, new.summary);
END;

CREATE TRIGGER episodes_after_delete AFTER DELETE ON episodes BEGIN
    INSERT INTO episodes_fts (episodes_fts, rowid, summary) VALUES ('delete', old.id, old.summary);
END;
