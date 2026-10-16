-- The model's verdicts on pairs of names that may be the same (ADR 0013).
-- The lower id first. A "yes" is shown as a merge suggestion with its
-- reason; "no" and "unsure" are judged again once either side has twice the
-- mentions, or after 30 days. The reason comes from captured text through a
-- model, so a verdict goes with either entity, and with any mention of
-- either, since a forgotten entry may be what the reason drew on
-- (GUARDRAILS §4).
CREATE TABLE pair_verdicts (
    a          INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    b          INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    verdict    TEXT NOT NULL CHECK (verdict IN ('yes', 'no', 'unsure')),
    reason     TEXT NOT NULL,
    judged_at  INTEGER NOT NULL, -- Unix ms UTC
    mentions_a INTEGER NOT NULL,
    mentions_b INTEGER NOT NULL,
    PRIMARY KEY (a, b),
    CHECK (a < b)
) STRICT, WITHOUT ROWID;

CREATE INDEX pair_verdicts_by_b ON pair_verdicts (b);

CREATE TRIGGER pair_verdicts_after_mention_delete AFTER DELETE ON mentions
BEGIN
    DELETE FROM pair_verdicts WHERE a = old.entity_id OR b = old.entity_id;
END;
