//! The `git push` argv parser of the #8878 D4 force-push floor.
//!
//! Why: split out of `force_push.rs` (#8878 S5 critic round 1) to keep that
//! file under the 500-SLOC cap; the rule decides, this module only reads.
//! What: [`parse`] turns the words after `push` into a [`Push`]: the force
//! flags, `--mirror` / `--all`, the `--repo` value, the operands, and the
//! shell syntax that `shlex` left inside the words (redirects, a glued `)` or
//! `&`), which is kept apart so it is never a remote or the only refspec.
//! Test: `a_redirect_is_not_a_refspec`, `no_force_does_not_clear_a_lease`,
//! `a_long_option_prefix_is_resolved`.

use super::super::{RedirectRole, redirect_role};

/// `git push` options whose value is a separate token.
// #8878: `--recurse-submodules check` takes its value as a token too.
const PUSH_VALUE_OPTS: &[&str] = &[
    "push-option",
    "repo",
    "receive-pack",
    "exec",
    "recurse-submodules",
];

/// Every `git push` long option (git-push(1)), for unique-prefix resolution.
const PUSH_LONG_OPTS: &[&str] = &[
    "all",
    "branches",
    "prune",
    "mirror",
    "dry-run",
    "porcelain",
    "delete",
    "tags",
    "follow-tags",
    "no-follow-tags",
    "signed",
    "no-signed",
    "atomic",
    "no-atomic",
    "push-option",
    "receive-pack",
    "exec",
    "force-with-lease",
    "no-force-with-lease",
    "force",
    "no-force",
    "force-if-includes",
    "no-force-if-includes",
    "repo",
    "set-upstream",
    "thin",
    "no-thin",
    "quiet",
    "verbose",
    "progress",
    "no-progress",
    "recurse-submodules",
    "no-recurse-submodules",
    "verify",
    "no-verify",
    "ipv4",
    "ipv6",
];

/// The `git push` long option `name` names: exact, or its one unique-prefix
/// match. `None` when it matches none or several.
/// Test: `a_long_option_prefix_is_resolved`.
fn resolve_long(name: &str) -> Option<&'static str> {
    if let Some(exact) = PUSH_LONG_OPTS.iter().find(|o| **o == name) {
        return Some(exact);
    }
    let mut matches = PUSH_LONG_OPTS.iter().filter(|o| o.starts_with(name));
    match (matches.next(), matches.next()) {
        (Some(only), None) if !name.is_empty() => Some(only),
        _ => None,
    }
}

/// The parsed shape of one `git push`.
#[derive(Default)]
pub(super) struct Push {
    pub(super) force: bool,
    /// `--force-with-lease`, tracked apart: `--no-force` does not clear it.
    pub(super) lease: bool,
    /// An unknown or ambiguous long option, which could be a force.
    pub(super) unknown: bool,
    /// The `--repo` value, the repository when no operand names one.
    pub(super) repo: Option<String>,
    pub(super) mirror: bool,
    pub(super) all: bool,
    pub(super) operands: Vec<String>,
    /// Shell syntax read as words — redirects (`2>&1`, `>`, `log.txt`), and a
    /// word with a redirect, `&` or `)` glued on: never a remote or the only
    /// refspec, but still judged as refspecs, in case quoting made one.
    pub(super) shell_words: Vec<String>,
}

impl Push {
    /// The repository the push names: its first operand, else `--repo`.
    pub(super) fn remote(&self) -> Option<&String> {
        self.operands.first().or(self.repo.as_ref())
    }
}

/// Parse the argv after `push`.
///
/// Test: `a_redirect_is_not_a_refspec`, `no_force_does_not_clear_a_lease`.
pub(super) fn parse(tail: &[String]) -> Push {
    let mut push = Push::default();
    let mut i = 0;
    let mut options_end = false;
    while i < tail.len() {
        let raw = tail[i].as_str();
        i += 1;
        // #8878 S5: a redirect is the shell's, not a refspec — read as one, it
        // hid the implicit current-branch destination of a forced push.
        if let Some(follows) = redirect_word(raw) {
            push.shell_words.push(raw.to_string());
            if follows && let Some(target) = tail.get(i) {
                push.shell_words.push(target.clone());
                i += 1;
            }
            continue;
        }
        let tok = shell_free(raw);
        if tok != raw {
            push.shell_words.push(raw.to_string());
        }
        if tok.is_empty() {
            continue;
        }
        if options_end {
            push.operands.push(tok.to_string());
            continue;
        }
        if tok == "--" {
            options_end = true;
            continue;
        }
        if let Some(long) = tok.strip_prefix("--") {
            let (name, attached) = long
                .split_once('=')
                .map_or((long, false), |(n, _)| (n, true));
            // #8878: git takes any unique prefix of a long option.
            let value = long.split_once('=').map(|(_, v)| v.to_string());
            match resolve_long(name) {
                Some("force") => push.force = true,
                // #8878 S5: `--no-force` does not clear a lease.
                Some("force-with-lease") => push.lease = true,
                Some("no-force-with-lease") => push.lease = false,
                Some("no-force") => push.force = false,
                Some("repo") => {
                    push.repo = value.or_else(|| tail.get(i).cloned());
                    i += usize::from(!attached);
                }
                Some("mirror") => push.mirror = true,
                Some("all" | "branches") => push.all = true,
                Some(opt) if !attached && PUSH_VALUE_OPTS.contains(&opt) => i += 1,
                Some(_) => {}
                // Unknown or ambiguous: it could be a force — fail closed.
                None => push.unknown = true,
            }
            continue;
        }
        match tok {
            "-o" => i += 1,
            t if t.starts_with('-') && t.len() > 1 => {
                // A short cluster: `-f` anywhere before `-o`, whose value ends it.
                let cluster = &t[1..];
                let before_o = cluster.split('o').next().unwrap_or_default();
                push.force |= before_o.contains('f');
                if cluster.ends_with('o') {
                    i += 1;
                }
            }
            _ => push.operands.push(tok.to_string()),
        }
    }
    push
}

/// `word` with the shell syntax glued to its end removed: everything from its
/// first `>` or `<` (a redirect), then one trailing `&` (background), then
/// every `)` closing a subshell — the word bash passes to git.
///
/// Why: #8878 S5 critic round 1, HIGH 3 — a redirect or `&` written with no
/// space after a refspec stayed in the word, so the refspec never matched.
/// #8878 S5 round 2, row 6: nested subshells close with `))`.
/// Test: `a_redirect_is_not_a_refspec`, `a_nested_subshell_still_reads_the_refspec`.
fn shell_free(word: &str) -> &str {
    let word = word.find(['>', '<']).map_or(word, |at| &word[..at]);
    let word = word.strip_suffix('&').unwrap_or(word);
    let bare = word.trim_end_matches(')');
    if bare.contains('(') { word } else { bare }
}

/// Whether `word` is a shell redirect: `Some(true)` when its file is the next
/// word (`>`, `2>`, `<`, `<<<`), `Some(false)` when attached or a descriptor
/// operation (`>log`, `2>&1`, `<in`, `<<EOF`), `None` when it is not one.
///
/// Why: #8878 S5 (R7) — a redirect after the remote was read as a refspec.
/// What: output redirects through [`redirect_role`]; an input redirect is an
/// optional descriptor number, then `<`, `<<`, `<<<`, `<<-`, `<&` or `<>`.
/// Test: `a_redirect_is_not_a_refspec`.
fn redirect_word(word: &str) -> Option<bool> {
    match redirect_role(word) {
        RedirectRole::TargetFollows => return Some(true),
        RedirectRole::Target(_) | RedirectRole::FileDescriptor => return Some(false),
        RedirectRole::None => {}
    }
    let input = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = input.strip_prefix('<')?;
    let rest = ["<<", "<-", "<", "&", ">"]
        .iter()
        .find_map(|op| rest.strip_prefix(op))
        .unwrap_or(rest);
    Some(rest.is_empty())
}
