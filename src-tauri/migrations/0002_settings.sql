-- Small user settings that belong with the data, such as how long raw
-- captures are kept. Never secrets: those live in the macOS Keychain.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
