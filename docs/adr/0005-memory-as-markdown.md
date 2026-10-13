# 0005. Memory pages stored as Markdown files

- **Status:** Accepted; superseded in part by [0011](0011-pages-in-the-database.md)
- **Date:** 2026-10-10

## Context

The core value of Lax is a memory that learns who the user is, which projects they
work on and who they work with. Users must be able to see and correct what Lax
believes about them.

## Decision

What Lax learns is stored as plain **Markdown files** in a folder the user owns:
one page per project, person and organisation, plus `about-me.md`. Raw captures,
episodes and search indexes live in SQLite next to them.

User edits are ground truth: the model may add to a page but must not contradict
or remove what the user wrote. Every model change is versioned so it can be reviewed
and undone.

## Consequences

- Users can read, edit, search, back up and sync their memory with any tool
  (a text editor, Obsidian, git).
- Lax must handle files being edited outside the app.
- Keeping SQLite and Markdown in sync needs care.

## Alternatives considered

- **Database only:** simpler to implement, but the memory becomes opaque and locked into Lax.
