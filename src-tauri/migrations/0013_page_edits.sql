-- Versions of the user's own text on a page, once the pages are in the
-- database and edited in Lax (ADR 0011): one each time the user opens a
-- page and changes it, the latest kept. They are the user's text, so a
-- forget of episodes leaves them; forgetting the page's entity takes them.
CREATE TABLE page_edits (
    id       INTEGER PRIMARY KEY,
    page_id  TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    saved_at INTEGER NOT NULL,
    text     TEXT NOT NULL
) STRICT;

CREATE INDEX page_edits_by_page ON page_edits (page_id);
