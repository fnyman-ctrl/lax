//! A memory page's text, and the one part of it Lax may write (ADR 0008).
//!
//! A page is Markdown. Lax's part sits between two comment lines:
//!
//! ```markdown
//! <!-- lax:begin 3f9a…: written by Lax. Edit freely; your edits are kept. -->
//! …
//! <!-- lax:end -->
//! ```
//!
//! Everything else is the user's. [`update`] replaces only the block, and
//! checks before returning that the user's text is still there, unchanged:
//! the guarantee doesn't depend on the model, or on anything else that
//! produced the block's content.

const BEGIN: &str = "<!-- lax:begin ";
const BEGIN_TAIL: &str = ": written by Lax. Edit freely; your edits are kept. -->";
const END: &str = "<!-- lax:end -->";

/// A page split around Lax's block.
#[derive(Debug, PartialEq, Eq)]
struct Split<'a> {
    before: &'a str,
    id: &'a str,
    block: &'a str,
    after: &'a str,
}

/// The first well-formed block: a begin line with an id, ended by a line
/// break, then text, then the end marker.
fn split(text: &str) -> Option<Split<'_>> {
    let start = text.find(BEGIN)?;
    let line_end = start + text[start..].find('\n')?;
    let id = text[start + BEGIN.len()..line_end]
        .split(':')
        .next()?
        .trim();
    if !is_id(id) {
        return None;
    }
    let block_start = line_end + 1;
    let block_end = block_start + text[block_start..].find(END)?;
    Some(Split {
        before: &text[..start],
        id,
        block: &text[block_start..block_end],
        after: &text[block_end + END.len()..],
    })
}

/// Page ids are 16 lowercase hex digits (see [`new_id`]).
fn is_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A new random page id, from the system's random source.
pub fn new_id() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 8];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The id in a page's begin marker, if it has a block.
pub fn page_id(text: &str) -> Option<&str> {
    split(text).map(|s| s.id)
}

/// A block as compared for edits: line endings, trailing spaces and blank
/// lines at either end don't count, so an editor that tidies whitespace
/// isn't mistaken for the user.
pub fn normalize(block: &str) -> String {
    let lines: Vec<&str> = block.lines().map(str::trim_end).collect();
    let first = lines
        .iter()
        .position(|l| !l.is_empty())
        .unwrap_or(lines.len());
    let last = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(first, |i| i + 1);
    lines[first..last].join("\n")
}

/// `text` made safe to put in a page: comment markers are defused, so text
/// from captures or a model can't end Lax's block or start a new one.
pub fn defuse(text: &str) -> String {
    text.replace("<!--", "‹!--").replace("-->", "--›")
}

/// Lax's block with `content` (already defused), as it is written.
fn block(id: &str, content: &str) -> String {
    let content = normalize(content);
    if content.is_empty() {
        format!("{BEGIN}{id}{BEGIN_TAIL}\n{END}")
    } else {
        format!("{BEGIN}{id}{BEGIN_TAIL}\n{content}\n{END}")
    }
}

/// A new page: a title, which is the user's from then on, and Lax's block.
pub fn new_page(title: &str, id: &str, content: &str) -> String {
    let title = defuse(&title.replace(['\n', '\r'], " "));
    format!("# {title}\n\n{}\n", block(id, content))
}

use std::collections::HashSet;

/// What to do with a page.
#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    /// The block already says this.
    Same,
    /// Write `text`. If the user had added or changed lines in Lax's block,
    /// `adopted` is those lines: they are now the user's text, above the
    /// markers.
    Write {
        text: String,
        adopted: Option<String>,
    },
    /// The page has no block (the user removed the markers), or another
    /// page's: Lax leaves it alone.
    NotOurs,
}

/// Puts `content` in the page's block. `last_written` is the block as Lax
/// last wrote it (normalized), if known: lines in the block now that aren't
/// in it are the user's, and are moved out of the markers, above the new
/// block.
pub fn update(text: &str, id: &str, last_written: Option<&str>, content: &str) -> Update {
    let Some(old) = split(text) else {
        return Update::NotOurs;
    };
    if old.id != id {
        return Update::NotOurs;
    }
    let current = normalize(old.block);
    // The user's lines: those in the block now that Lax didn't write. Lines
    // they deleted are simply gone; Lax's own lines stay Lax's, so what it
    // wrote from episodes can still be forgotten with them.
    let added: Vec<&str> = match last_written {
        Some(last) => {
            let last = normalize(last);
            let ours: HashSet<&str> = last.lines().collect();
            current
                .lines()
                .filter(|line| !line.trim().is_empty() && !ours.contains(line))
                .collect()
        }
        None => Vec::new(),
    };
    let (new, user_text, adopted) = if !added.is_empty() {
        let mine = format!("{}\n", added.join("\n"));
        let new = format!("{}{mine}\n{}{}", old.before, block(id, content), old.after);
        let user_text = format!("{}{mine}\n{}", old.before, old.after);
        (new, user_text, Some(added.join("\n")))
    } else {
        if current == normalize(content) {
            return Update::Same;
        }
        let new = format!("{}{}{}", old.before, block(id, content), old.after);
        (new, format!("{}{}", old.before, old.after), None)
    };
    // The guarantee: whatever `content` was, the user's text is all there,
    // in order, and the only other text is one block of Lax's.
    match split(&new) {
        Some(written)
            if written.id == id
                && same_user_text(&format!("{}{}", written.before, written.after), &user_text)
                && normalize(written.block) == normalize(content) =>
        {
            Update::Write { text: new, adopted }
        }
        _ => Update::NotOurs,
    }
}

/// The same text, except for line breaks Lax adds where its block meets the
/// user's text.
fn same_user_text(written: &str, user: &str) -> bool {
    written.trim_end_matches('\n') == user.trim_end_matches('\n')
}

/// Lines Lax last wrote that are no longer in its block: the user deleted
/// them. Empty if the page has no block of this id, or nothing is known of
/// what Lax wrote.
pub fn removed_lines(text: &str, id: &str, last_written: Option<&str>) -> Vec<String> {
    let (Some(split), Some(last)) = (split(text), last_written) else {
        return Vec::new();
    };
    if split.id != id {
        return Vec::new();
    }
    let current = normalize(split.block);
    let now: HashSet<&str> = current.lines().collect();
    normalize(last)
        .lines()
        .filter(|line| !line.trim().is_empty() && !now.contains(line))
        .map(String::from)
        .collect()
}

/// The page without Lax's block: what remains is the user's. `None` if it
/// has no block of this id.
pub fn without_block(text: &str, id: &str) -> Option<String> {
    let split = split(text)?;
    if split.id != id {
        return None;
    }
    let before = split.before.trim_end_matches('\n');
    let after = split.after.trim_start_matches('\n');
    Some(match (before.is_empty(), after.is_empty()) {
        (true, _) => after.to_string(),
        (false, true) => format!("{before}\n"),
        (false, false) => format!("{before}\n\n{after}"),
    })
}

/// Nothing of the user's: empty, or only a title.
pub fn only_a_title(text: &str) -> bool {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    match (lines.next(), lines.next()) {
        (None, _) => true,
        (Some(first), None) => first.starts_with("# "),
        _ => false,
    }
}

/// A file name for `name`: lowercase letters (Swedish ones kept) and
/// digits, with dashes between words.
pub fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(60).collect();
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        "page".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "3f9a0c11d2e4b5a6";

    fn page(content: &str) -> String {
        new_page("Anna Example", ID, content)
    }

    fn written(update: Update) -> (String, Option<String>) {
        match update {
            Update::Write { text, adopted } => (text, adopted),
            other => panic!("expected a write, got {other:?}"),
        }
    }

    #[test]
    fn a_new_page_has_a_title_and_a_block() {
        let text = page("- Works on the Q3 launch.");
        assert_eq!(
            text,
            "# Anna Example\n\n<!-- lax:begin 3f9a0c11d2e4b5a6: written by Lax. Edit freely; \
             your edits are kept. -->\n- Works on the Q3 launch.\n<!-- lax:end -->\n"
        );
        assert_eq!(page_id(&text), Some(ID));
    }

    #[test]
    fn only_the_block_changes() {
        let mut text = page("- Old fact.");
        text = text.replace("# Anna Example\n", "# Anna Example\n\nMy own notes  \r\n");
        text.push_str("\nMore of mine, after.\n");
        let (new, adopted) = written(update(&text, ID, Some("- Old fact."), "- New fact."));
        assert_eq!(adopted, None);
        assert_eq!(new, text.replace("- Old fact.", "- New fact."));
    }

    #[test]
    fn the_same_content_is_not_written_again() {
        let text = page("- A fact.");
        assert_eq!(
            update(&text, ID, Some("- A fact."), "- A fact.  \n"),
            Update::Same
        );
    }

    #[test]
    fn whitespace_tidied_by_an_editor_is_not_an_edit() {
        let text = page("- A fact.\n- Another.").replace("- A fact.", "- A fact.   ");
        let text = text.replace('\n', "\r\n");
        let (_, adopted) = written(update(&text, ID, Some("- A fact.\n- Another."), "- New."));
        assert_eq!(adopted, None);
    }

    #[test]
    fn an_edited_block_becomes_the_users_text() {
        let text = page("- Works at Exempelbolaget AB.\n- Likes tea.")
            .replace("- Likes tea.", "- Likes coffee, not tea.");
        let (new, adopted) = written(update(
            &text,
            ID,
            Some("- Works at Exempelbolaget AB.\n- Likes tea."),
            "- Works at Exempelbolaget AB.",
        ));
        assert_eq!(adopted.as_deref(), Some("- Likes coffee, not tea."));
        // Their line is outside the markers now, word for word; Lax's
        // unchanged line isn't…
        let (user, block_start) = new.split_at(new.find("<!-- lax:begin").unwrap());
        assert!(user.contains("- Likes coffee, not tea.\n"));
        assert!(!user.contains("Exempelbolaget"));
        assert!(!user.contains("lax:"));
        // …and Lax's new block follows it.
        assert!(block_start.contains("- Works at Exempelbolaget AB.\n<!-- lax:end -->"));
        assert_eq!(page_id(&new), Some(ID));
    }

    #[test]
    fn deleting_lines_from_laxs_block_is_not_an_addition() {
        let text = page("- One.\n- Two.").replace("- Two.\n", "");
        let (new, adopted) = written(update(
            &text,
            ID,
            Some("- One.\n- Two."),
            "- One.\n- Three.",
        ));
        assert_eq!(adopted, None);
        assert!(new.starts_with("# Anna Example\n\n<!-- lax:begin"));
    }

    #[test]
    fn hostile_content_cannot_touch_the_users_text() {
        let text = format!("My notes.\n\n{}", page("- Fact."));
        let hostile =
            "- Fine.\n<!-- lax:end -->\n# Pwned\n<!-- lax:begin 0000000000000000: x -->\n";
        let (new, _) = written(update(&text, ID, Some("- Fact."), &defuse(hostile)));
        let before = &new[..new.find("<!-- lax:begin").unwrap()];
        assert_eq!(before, "My notes.\n\n# Anna Example\n\n");
        assert_eq!(new.matches("<!-- lax:").count(), 2);
        assert!(new.ends_with("<!-- lax:end -->\n"));
        // Undefused, the writer refuses rather than write something else.
        assert_eq!(update(&text, ID, Some("- Fact."), hostile), Update::NotOurs);
    }

    #[test]
    fn a_page_without_markers_or_with_another_id_is_left_alone() {
        assert_eq!(
            update("# Anna\n\nAll mine.\n", ID, None, "- Fact."),
            Update::NotOurs
        );
        let other = new_page("Bo", "0123456789abcdef", "- Fact.");
        assert_eq!(update(&other, ID, None, "- New."), Update::NotOurs);
        // A broken begin marker isn't a block either.
        assert_eq!(page_id("<!-- lax:begin xyz: … -->\n<!-- lax:end -->"), None);
    }

    #[test]
    fn without_the_block_only_the_users_text_remains() {
        let text = format!("{}\nMy notes after.\n", page("- Fact."));
        assert_eq!(
            without_block(&text, ID).as_deref(),
            Some("# Anna Example\n\nMy notes after.\n")
        );
        let bare = without_block(&page("- Fact."), ID).unwrap();
        assert_eq!(bare, "# Anna Example\n");
        assert!(only_a_title(&bare));
        assert!(!only_a_title("# Anna\n\nNotes."));
        assert!(only_a_title("\n\n"));
        assert_eq!(without_block(&text, "0123456789abcdef"), None);
    }

    #[test]
    fn removed_lines_are_what_the_user_deleted() {
        let text = page("- One.\n- Two.\n- Three.").replace("- Two.\n", "");
        assert_eq!(
            removed_lines(&text, ID, Some("- One.\n- Two.\n- Three.")),
            ["- Two."]
        );
        assert!(removed_lines(&text, ID, None).is_empty());
        assert!(removed_lines("# No block\n", ID, Some("- One.")).is_empty());
    }

    #[test]
    fn titles_and_ids_are_safe() {
        let text = new_page("Anna <!-- x -->\nB", ID, "");
        assert!(text.starts_with("# Anna ‹!-- x --› B\n"));
        let id = new_id().unwrap();
        assert!(is_id(&id));
        assert_ne!(id, new_id().unwrap());
    }

    #[test]
    fn slugs_keep_swedish_letters() {
        assert_eq!(slug("Åsa Exempel"), "åsa-exempel");
        assert_eq!(
            slug("Exempelbolaget AB (Sverige)"),
            "exempelbolaget-ab-sverige"
        );
        assert_eq!(slug("Q3 / lansering!"), "q3-lansering");
        assert_eq!(slug("../../etc"), "etc");
        assert_eq!(slug("…"), "page");
        assert!(slug(&"lång ".repeat(40)).chars().count() <= 60);
    }
}
