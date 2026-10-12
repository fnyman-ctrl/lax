-- Vectors of journal episodes, for finding them by meaning (Stage 5), from
-- the embedding model chosen on this Mac. Derived from the episode, so
-- deleted with it (GUARDRAILS §4).
CREATE TABLE episode_vectors (
    episode_id INTEGER PRIMARY KEY REFERENCES episodes (id) ON DELETE CASCADE,
    model      TEXT NOT NULL,
    vector     BLOB NOT NULL -- f32, little-endian
) STRICT;
