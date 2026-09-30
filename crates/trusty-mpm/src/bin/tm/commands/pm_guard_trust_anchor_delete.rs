//! The delete arm of the trust-anchor floor (#8878 Q2 delete ruling) and its
//! PM/agent split (owner ruling "Keep the split", #8878).
//!
//! Why: split out of `pm_guard_trust_anchor.rs` (line cap). The fail-closed
//! delete denials — an unplaced path, a glob judged by its directory, a
//! `find` start point or `rmdir -p` parent above an anchor — bind the PM
//! only; a subagent is denied a delete only when its path reaches a real
//! anchor.
//! What: [`Anchors::judge_delete`] judges one [`AnchorWrite::Delete`] word by
//! its [`Reach`]. For the PM the entry, where it sits and where it leads,
//! denies on [`Anchors::is_anchor`]; an unplaced word is judged by its glob
//! directory or [`Anchors::could_be`]. For a subagent an `Entry` delete uses
//! the same test, a `Tree` start point [`Anchors::is_within`], a `Parent`
//! nothing, and an unplaced word only the directory entries its
//! last-component glob matches.
//! Test: `pm_guard_trust_anchor_delete_tests.rs`,
//! `pm_guard_trust_anchor_split_tests.rs`.
//!
//! [`AnchorWrite::Delete`]: crate::commands::pm_guard_bash::AnchorWrite::Delete

use std::path::Path;

use super::{Anchors, anchor_reason, unknown_reason, unresolvable_reason};
use crate::commands::pm_guard_bash::Reach;
use crate::commands::pm_guard_trust_anchor_paths::{Placed, Resolved, place, resolve};

impl Anchors {
    /// [`Anchors::judge`] for a delete; `strict` is the PM's reading.
    ///
    /// What: see the module doc. A parent that does not resolve denies every
    /// session: the guard cannot see where the entry lands.
    /// Test: `each_delete_verb_on_each_anchor_is_denied`,
    /// `an_unplaceable_delete_is_denied`, `ordinary_deletes_stay_allowed`,
    /// `expansion_and_xargs_deletes_are_denied_to_the_pm_only`,
    /// `an_agent_is_denied_a_delete_reaching_a_real_anchor`.
    pub(super) fn judge_delete(
        &self,
        word: &str,
        reach: Reach,
        base: Option<&Path>,
        home: &Path,
        strict: bool,
    ) -> Option<String> {
        // #8878 split: `rmdir -p` removes a parent only when empty.
        if !strict && reach == Reach::Parent {
            return None;
        }
        let path = match place(word, base, home, true) {
            Placed::At(path) => path,
            Placed::Unknown if strict => return self.judge_unplaced_delete(word, base, home),
            // #8878 split: an agent's unplaced delete denies only through a glob match.
            Placed::Unknown => return self.judge_agent_glob(word, reach, base, home),
        };
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            match resolve(parent) {
                Resolved::Path(parent) if self.reaches(&parent.join(name), reach, strict) => {
                    return Some(anchor_reason(word));
                }
                Resolved::Path(_) => {}
                Resolved::Unresolvable => return Some(unresolvable_reason(word)),
            }
        }
        match resolve(&path) {
            Resolved::Path(resolved) => self
                .reaches(&resolved, reach, strict)
                .then(|| anchor_reason(word)),
            Resolved::Unresolvable => Some(unresolvable_reason(word)),
        }
    }

    /// Whether deleting `resolved` with `reach` removes an anchor: for the PM
    /// or an `Entry` delete an anchor or a directory above one, otherwise an
    /// anchor or anything inside one.
    fn reaches(&self, resolved: &Path, reach: Reach, strict: bool) -> bool {
        if strict || reach == Reach::Entry {
            self.is_anchor(resolved)
        } else {
            self.is_within(resolved)
        }
    }

    /// The PM's verdict on a delete word the guard cannot place.
    ///
    /// What: a glob confined to the last component ([`glob_dir`]) could remove
    /// any entry of its directory, so the directory is judged; any other word
    /// denies when [`Anchors::could_be`] says so.
    fn judge_unplaced_delete(
        &self,
        word: &str,
        base: Option<&Path>,
        home: &Path,
    ) -> Option<String> {
        let Some((dir, _)) = glob_dir(word) else {
            return self.could_be(word).then(|| unknown_reason(word));
        };
        match place(dir, base, home, true) {
            Placed::At(dir) => match resolve(&dir) {
                Resolved::Path(dir) => self.is_anchor(&dir).then(|| anchor_reason(word)),
                Resolved::Unresolvable => Some(unresolvable_reason(word)),
            },
            Placed::Unknown => self.could_be(dir).then(|| unknown_reason(word)),
        }
    }

    /// A subagent's verdict on a delete word the guard cannot place (#8878
    /// "Keep the split").
    ///
    /// What: only a glob in the last component of a placeable directory is
    /// judged. The directory denies when it is itself an anchor or inside
    /// one; otherwise each entry the glob matches is judged where it sits and
    /// where it leads. A missing directory matches nothing. FAIL-CLOSED: a
    /// directory, listing or matched entry that cannot be read denies.
    /// Test: `an_agent_is_denied_a_delete_reaching_a_real_anchor`,
    /// `expansion_and_xargs_deletes_are_denied_to_the_pm_only`.
    fn judge_agent_glob(
        &self,
        word: &str,
        reach: Reach,
        base: Option<&Path>,
        home: &Path,
    ) -> Option<String> {
        let (dir, pattern) = glob_dir(word)?;
        let Placed::At(dir) = place(dir, base, home, true) else {
            return None;
        };
        let Resolved::Path(dir) = resolve(&dir) else {
            return Some(unresolvable_reason(word));
        };
        if self.is_within(&dir) {
            return Some(anchor_reason(word));
        }
        let entries = match std::fs::read_dir(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) => return Some(unresolvable_reason(word)),
            Ok(entries) => entries,
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return Some(unresolvable_reason(word));
            };
            let name = entry.file_name();
            if !glob_matches(pattern, &name.to_string_lossy()) {
                continue;
            }
            let path = dir.join(&name);
            let Resolved::Path(leads) = resolve(&path) else {
                return Some(unresolvable_reason(word));
            };
            if self.reaches(&path, reach, false) || self.reaches(&leads, reach, false) {
                return Some(anchor_reason(word));
            }
        }
        None
    }
}

/// The directory and pattern of a glob in `word`'s last component, or `None`
/// when the last component has no glob, or carries a `$`, backtick or brace,
/// whose expansion could hold a `/` (#8878 Q2 delete ruling).
fn glob_dir(word: &str) -> Option<(&str, &str)> {
    let word = word.trim_end_matches('/');
    let (dir, name) = match word.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some(split) => split,
        None => (".", word),
    };
    (name.contains(['*', '?', '[']) && !name.contains(['$', '`', '{'])).then_some((dir, name))
}

/// Whether a shell glob `pattern` matches the file name `name` (#8878 split).
///
/// What: `*`, `?` and `[…]` (with `!`/`^` negation and ranges); a leading
/// `.` must be matched literally, as the shell requires by default. Letters
/// compare ASCII case-insensitively, an over-match that only denies more.
/// An unclosed `[` is a literal.
/// Test: `an_agent_is_denied_a_delete_reaching_a_real_anchor`.
fn glob_matches(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        let step = match p.get(pi) {
            Some('*') => {
                star = Some((pi, ni));
                pi += 1;
                continue;
            }
            Some('?') => Some(1),
            Some('[') => match class_matches(&p[pi..], n[ni]) {
                Some((true, len)) => Some(len),
                Some((false, _)) => None,
                None => (n[ni] == '[').then_some(1),
            },
            Some(c) if c.eq_ignore_ascii_case(&n[ni]) => Some(1),
            _ => None,
        };
        match (step, star) {
            (Some(len), _) => {
                pi += len;
                ni += 1;
            }
            (None, Some((sp, sn))) => {
                pi = sp + 1;
                ni = sn + 1;
                star = Some((sp, sn + 1));
            }
            (None, None) => return false,
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Whether the bracket class opening `p` matches `c`, and the class's length;
/// `None` when the class is not closed.
fn class_matches(p: &[char], c: char) -> Option<(bool, usize)> {
    let mut i = 1;
    let negate = matches!(p.get(i), Some('!' | '^'));
    if negate {
        i += 1;
    }
    let start = i;
    let mut hit = false;
    let same = |x: char| x.eq_ignore_ascii_case(&c);
    while let Some(&x) = p.get(i) {
        if x == ']' && i > start {
            return Some((hit != negate, i + 1));
        }
        if p.get(i + 1) == Some(&'-') && p.get(i + 2).is_some_and(|y| *y != ']') {
            let y = p[i + 2];
            let lower = c.to_ascii_lowercase();
            let upper = c.to_ascii_uppercase();
            hit |= (x..=y).contains(&c) || (x..=y).contains(&lower) || (x..=y).contains(&upper);
            i += 3;
        } else {
            hit |= same(x);
            i += 1;
        }
    }
    None
}
