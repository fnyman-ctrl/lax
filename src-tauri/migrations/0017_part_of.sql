-- Projects in parts (ADR 0013, point 5): a project can be part of one other
-- project, one level only (the store checks that). Forgetting the whole
-- leaves its parts as projects of their own.
ALTER TABLE entities ADD COLUMN part_of INTEGER REFERENCES entities (id) ON DELETE SET NULL;

CREATE INDEX entities_by_whole ON entities (part_of) WHERE part_of IS NOT NULL;
