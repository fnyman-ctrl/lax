-- The user's own exclusions (GUARDRAILS §4, tier 3) and the tier-2 defaults
-- (banking, health) they turned off. Matched case-insensitively. Settings
-- only: nothing here says what was on screen.
CREATE TABLE exclusions (
    kind  TEXT NOT NULL CHECK (kind IN ('app', 'site', 'title', 'default_off')),
    value TEXT NOT NULL COLLATE NOCASE,
    PRIMARY KEY (kind, value)
) STRICT;
