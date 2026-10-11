-- What the user decided about the memory (Stage 4 step 2, ADR 0008).

-- 1 when the user chose to keep an entity before it was mentioned on two
-- different days, which is when Lax would keep it on its own.
ALTER TABLE entities ADD COLUMN kept INTEGER NOT NULL DEFAULT 0;

-- Names the user asked Lax not to learn about: dismissed new names, and
-- entities forgotten with "Don't learn about them again". A name here is
-- never recorded again. It holds names, so it is shown in the Memory tab,
-- can be cleared, and goes with "delete everything".
CREATE TABLE dont_learn (
    kind       TEXT NOT NULL,
    key        TEXT NOT NULL, -- normalized, as in aliases
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (kind, key)
) STRICT;
