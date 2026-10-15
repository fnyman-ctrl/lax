-- The page files moved into the database (ADR 0011), with a fingerprint of
-- what each held when it was moved (FNV-1a of its bytes, and its length),
-- so "Delete the old files" deletes only a file that still holds exactly
-- that. Not the text itself: a copy here would outlive a forget. Rows go
-- once the old files are dealt with.
CREATE TABLE moved_page_files (
    path  TEXT PRIMARY KEY, -- relative to ~/Lax
    hash  INTEGER NOT NULL,
    bytes INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
