-- The model's verdicts on pairs of projects (ADR 0013, point 5): the same,
-- one part of the other (`whole` says which), neither, or unsure. The lower
-- id first. "same" and "part_of" are shown as suggestions; "neither" and
-- "unsure" are judged again under the rules of pair_verdicts. Like those,
-- a verdict goes with either project and with any mention of either
-- (GUARDRAILS §4).
CREATE TABLE project_verdicts (
    a          INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    b          INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    relation   TEXT NOT NULL CHECK (relation IN ('same', 'part_of', 'neither', 'unsure')),
    whole      INTEGER CHECK (whole IS NULL OR whole IN (a, b)),
    reason     TEXT NOT NULL,
    judged_at  INTEGER NOT NULL, -- Unix ms UTC
    mentions_a INTEGER NOT NULL,
    mentions_b INTEGER NOT NULL,
    PRIMARY KEY (a, b),
    CHECK (a < b),
    CHECK ((relation = 'part_of') = (whole IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX project_verdicts_by_b ON project_verdicts (b);

CREATE TRIGGER project_verdicts_after_mention_delete AFTER DELETE ON mentions
BEGIN
    DELETE FROM project_verdicts WHERE a = old.entity_id OR b = old.entity_id;
END;

-- Pairs of projects the user said are not one part of the other: never
-- suggested as parts again. The lower id first; goes with either project.
CREATE TABLE not_part (
    a INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    b INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    PRIMARY KEY (a, b),
    CHECK (a < b)
) STRICT, WITHOUT ROWID;

CREATE INDEX not_part_by_b ON not_part (b);
