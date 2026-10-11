-- about-me.md (Stage 4 step 5, ADR 0008): one page about the user, next to
-- the pages about people, organisations and projects.
ALTER TABLE pages ADD COLUMN kind TEXT NOT NULL DEFAULT 'entity'
    CHECK (kind IN ('entity', 'about-me'));
CREATE UNIQUE INDEX pages_one_about_me ON pages (kind) WHERE kind = 'about-me';

-- When the model last wrote a page's facts (Unix ms UTC).
ALTER TABLE pages ADD COLUMN facts_at INTEGER;
