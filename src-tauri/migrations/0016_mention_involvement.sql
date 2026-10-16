-- How the user was involved with a name in an episode (ADR 0013, point 1):
-- 'works_with' (talks to, writes to, works for, takes part in) or
-- 'mentioned' (read about, saw in the news, a name on a page). Only
-- 'works_with' days earn a page. Mentions from before the flag count as
-- 'works_with', so nothing the memory already learned is lost.
ALTER TABLE mentions ADD COLUMN involvement TEXT NOT NULL DEFAULT 'works_with'
    CHECK (involvement IN ('works_with', 'mentioned'));
