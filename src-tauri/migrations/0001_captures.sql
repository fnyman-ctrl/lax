-- Raw captures: what was on screen, after redaction (GUARDRAILS §4).
CREATE TABLE captures (
    id           INTEGER PRIMARY KEY,
    captured_at  INTEGER NOT NULL, -- Unix time in milliseconds, UTC
    app_name     TEXT NOT NULL,
    bundle_id    TEXT NOT NULL,
    window_title TEXT NOT NULL,
    site         TEXT,             -- host only, e.g. figma.com; never a full address
    text         TEXT NOT NULL
) STRICT;

CREATE INDEX captures_by_time ON captures (captured_at);
CREATE INDEX captures_by_app ON captures (bundle_id COLLATE NOCASE);

-- Full-text search over titles and text. Diacritics are kept, because å, ä
-- and ö are separate letters in Swedish ("för" is not "for").
CREATE VIRTUAL TABLE captures_fts USING fts5(
    window_title,
    text,
    content = 'captures',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 0'
);

-- Remove deleted text from the index right away instead of at the next merge,
-- so "forget" (#9) really forgets.
INSERT INTO captures_fts (captures_fts, rank) VALUES ('secure-delete', 1);

CREATE TRIGGER captures_after_insert AFTER INSERT ON captures BEGIN
    INSERT INTO captures_fts (rowid, window_title, text)
    VALUES (new.id, new.window_title, new.text);
END;

CREATE TRIGGER captures_after_delete AFTER DELETE ON captures BEGIN
    INSERT INTO captures_fts (captures_fts, rowid, window_title, text)
    VALUES ('delete', old.id, old.window_title, old.text);
END;

CREATE TRIGGER captures_after_update AFTER UPDATE ON captures BEGIN
    INSERT INTO captures_fts (captures_fts, rowid, window_title, text)
    VALUES ('delete', old.id, old.window_title, old.text);
    INSERT INTO captures_fts (rowid, window_title, text)
    VALUES (new.id, new.window_title, new.text);
END;
