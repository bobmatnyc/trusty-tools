//! The input box's whole text, read across wrapped rows: `fleet-poll.py`'s
//! `_norm`, `_box_full_text` and `_own_pointer` (988152c lines 1180-1209).
//!
//! Why: the Architect's pane is narrow (~47 columns), so the pointer the
//! poller types wraps across screen rows, and only the first row carries the
//! `❯` glyph. Confirming the send, and recognising a stale pointer of its own,
//! must read the whole box (#9171 A3b-1).
//! What: [`norm`] drops every Python `\s` character (the box's leading
//! no-break space included); [`box_full_text`] joins the rows between the
//! box's rules, drops `❯`, and normalises; [`own_pointer`] matches the shape
//! of a pointer `wake` typed. All pure.
//! Test: `box_full_text_reads_wrapped_rows`, `own_pointer_needs_the_whole_shape`.

use crate::classify::{find_box, screen_lines};
use crate::pystr::is_space;

/// `_norm`: `text` with every whitespace character removed.
pub(crate) fn norm(text: &str) -> String {
    text.chars().filter(|&c| !is_space(c)).collect()
}

/// `_box_full_text`: the normalised text of the rule-bounded input box, the
/// `❯` row and its wrapped continuation rows; `""` when no box is found.
pub(crate) fn box_full_text(capture: &str) -> String {
    let lines = screen_lines(capture);
    let Some((top, bottom)) = find_box(&lines) else {
        return String::new();
    };
    let joined: String = lines.get(top + 1..bottom).unwrap_or_default().concat();
    norm(&joined.replace('❯', ""))
}

/// `_own_pointer`: true when normalised box text has the shape of a pointer
/// `wake` typed (`[poll HH:MMZ] N new alerts -> path` or the "need you"
/// form). A human draft that only starts with `[poll` stays a draft.
pub(crate) fn own_pointer(text: &str) -> bool {
    text.starts_with("[poll")
        && text.contains("->")
        && ["newalerts", "alertsneedyou", "alertneedsyou"]
            .iter()
            .any(|k| text.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> String {
        "─".repeat(40)
    }

    #[test]
    fn box_full_text_reads_wrapped_rows() {
        let cap = format!(
            "body\n{r}\n❯\u{a0}[poll 08:53Z] 3 new\nalerts -> /x/a.jsonl\n{r}\nfooter\n",
            r = rule()
        );
        assert_eq!(box_full_text(&cap), "[poll08:53Z]3newalerts->/x/a.jsonl");
        assert_eq!(box_full_text("no box here\n"), "");
        assert_eq!(norm(" a\u{a0}b\tc\u{2003}d \n"), "abcd");
    }

    #[test]
    fn own_pointer_needs_the_whole_shape() {
        assert!(own_pointer("[poll11:15Z]4newalerts->/x"));
        assert!(own_pointer("[poll11:15Z]1alertneedsyou(0logged)->/x"));
        assert!(own_pointer("[poll11:15Z]2alertsneedyou(0logged)->/x"));
        assert!(!own_pointer("pollthe team about it"));
        assert!(!own_pointer("[poll]whatnewalertsmean"));
    }
}
