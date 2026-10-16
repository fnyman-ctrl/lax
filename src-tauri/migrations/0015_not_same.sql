-- Pairs of names the user said are not the same (ADR 0013): never suggested
-- as a merge again. The lower id first. A row goes with either entity, so
-- forgetting a name, the memory or a person by name takes it too
-- (GUARDRAILS §4).
CREATE TABLE not_same (
    a INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    b INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    PRIMARY KEY (a, b),
    CHECK (a < b)
) STRICT, WITHOUT ROWID;

CREATE INDEX not_same_by_b ON not_same (b);
