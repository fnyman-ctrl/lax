-- Everything sent to a remote model provider, exactly as it was sent, after
-- redaction (GUARDRAILS §3). Kept on this Mac only; the user can browse,
-- search and delete it.
CREATE TABLE outbox (
    id             INTEGER PRIMARY KEY,
    sent_at        INTEGER NOT NULL, -- Unix time in milliseconds, UTC
    provider       TEXT NOT NULL,    -- the provider's host, e.g. api.example.com
    task           TEXT NOT NULL,    -- what it was for, e.g. summarize
    model          TEXT NOT NULL,
    payload        TEXT NOT NULL,    -- the request body as sent
    status         INTEGER NOT NULL, -- HTTP status, 0 if nothing came back
    response_bytes INTEGER NOT NULL, -- size of the answer
    -- The captures the request was made from, so forgetting a time range or
    -- an app also removes what was sent about it (GUARDRAILS §4).
    covers_from    INTEGER,          -- earliest capture time in it, ms; NULL if none
    covers_to      INTEGER,          -- latest capture time in it, ms
    apps           TEXT NOT NULL DEFAULT '' -- bundle ids in it, one per line
) STRICT;

CREATE INDEX outbox_by_time ON outbox (sent_at);

CREATE VIRTUAL TABLE outbox_fts USING fts5(
    payload,
    content = 'outbox',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 0'
);

INSERT INTO outbox_fts (outbox_fts, rank) VALUES ('secure-delete', 1);

CREATE TRIGGER outbox_after_insert AFTER INSERT ON outbox BEGIN
    INSERT INTO outbox_fts (rowid, payload) VALUES (new.id, new.payload);
END;

CREATE TRIGGER outbox_after_delete AFTER DELETE ON outbox BEGIN
    INSERT INTO outbox_fts (outbox_fts, rowid, payload) VALUES ('delete', old.id, old.payload);
END;
