-- The memory pages move into the database (ADR 0011): each page's whole
-- Markdown, the user's text and Lax's block between its markers, as the
-- file held it. NULL until the page has moved in, or while it is missing.
-- Encrypted with the rest of the database (ADR 0010), and deleted with the
-- page.
ALTER TABLE pages ADD COLUMN text TEXT;
