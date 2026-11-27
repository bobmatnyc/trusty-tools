//! Deterministic scrub of local paths and session names, and the refusal check.
//!
//! Why: this repository is public (#12026 AC3). A recording names the sandbox
//! root, the script's own directory and the owner's home in tracebacks and
//! output, and fleet session names in ledger text.
//! What: [`Scrubber`] replaces each registered needle with its token in one
//! left-to-right pass, longest needle first, so a token is never rescanned and
//! the same input always gives the same output. Session names match only at a
//! name boundary and become `session-<n>`, numbered in sorted order. [`check`]
//! refuses a home or temp path shape, a listed session name or a
//! secret-shaped value, and names the field, never the value.
//! Test: `scrub_is_deterministic_and_order_independent`,
//! `scrub_replaces_paths_and_bounded_session_names`,
//! `check_refuses_an_unscrubbed_home_path_session_and_secret`.

use std::path::Path;

use super::Recording;
use crate::il::ilio::secret::looks_secret;

/// Path shapes that only a local machine produces.
const LOCAL_SHAPES: [&str; 6] = [
    "/Users/",
    "/home/",
    "/root/",
    "\\Users\\",
    "/var/folders/",
    "/private/var/",
];

/// What a refused recording still holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    /// A home or temp directory path.
    LocalPath,
    /// A listed fleet session name.
    Session,
    /// A value shaped like a credential.
    Secret,
}

/// Why [`check`] refused a recording: the field and the kind, never the value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field} holds an unscrubbed {kind:?}")]
pub struct Refusal {
    /// The field, e.g. `stdout` or `files#2` (an index, so a path key is not shown).
    pub field: String,
    /// What it holds.
    pub kind: RefusalKind,
}

#[derive(Debug, Clone)]
struct Rule {
    needle: String,
    token: String,
    bounded: bool,
}

/// Replaces local paths and session names with stable tokens.
#[derive(Debug, Clone, Default)]
pub struct Scrubber {
    rules: Vec<Rule>,
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// True when `needle` occurs in `s` at `i` and, for a bounded rule, is not
/// part of a longer name.
fn matches_at(s: &str, i: usize, rule: &Rule) -> bool {
    if !s[i..].starts_with(&rule.needle) {
        return false;
    }
    if !rule.bounded {
        return true;
    }
    let before = s[..i].chars().next_back().is_some_and(is_name_char);
    let after = s[i + rule.needle.len()..]
        .chars()
        .next()
        .is_some_and(is_name_char);
    !before && !after
}

impl Scrubber {
    /// An empty scrubber (replaces nothing).
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace `path`, in its given and canonical forms, with `token`.
    /// A path of `/` or an empty one is ignored: it would match everywhere.
    pub fn path(mut self, path: &Path, token: &str) -> Self {
        let canonical = std::fs::canonicalize(path).ok();
        for p in std::iter::once(path).chain(canonical.as_deref()) {
            let needle = p.to_string_lossy().trim_end_matches('/').to_owned();
            if !needle.is_empty() {
                self.push(needle, token.to_owned(), false);
            }
        }
        self
    }

    /// Replace each session name with `session-<n>`, `n` from its rank in
    /// sorted order, so the caller's list order does not change the output.
    pub fn sessions(mut self, names: &[String]) -> Self {
        let mut sorted: Vec<&String> = names.iter().filter(|n| !n.is_empty()).collect();
        sorted.sort();
        sorted.dedup();
        for (n, name) in sorted.into_iter().enumerate() {
            self.push(name.clone(), format!("session-{}", n + 1), true);
        }
        self
    }

    fn push(&mut self, needle: String, token: String, bounded: bool) {
        if self.rules.iter().any(|r| r.needle == needle) {
            return;
        }
        self.rules.push(Rule {
            needle,
            token,
            bounded,
        });
        // Longest needle first, then by text: one fixed order for any insertion order.
        self.rules.sort_by(|a, b| {
            b.needle
                .len()
                .cmp(&a.needle.len())
                .then(a.needle.cmp(&b.needle))
        });
    }

    /// `s` with every rule applied in one pass.
    pub fn text(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < s.len() {
            if let Some(rule) = self.rules.iter().find(|r| matches_at(s, i, r)) {
                out.push_str(&rule.token);
                i += rule.needle.len();
                continue;
            }
            // `i` always sits on a char boundary: needles are whole strings.
            let c = s[i..].chars().next().unwrap_or('\0');
            out.push(c);
            i += c.len_utf8().max(1);
        }
        out
    }

    /// `rec` with every string, map key included, scrubbed.
    pub fn recording(&self, rec: Recording) -> Recording {
        let t = |s: &str| self.text(s);
        Recording {
            version: rec.version,
            argv: rec.argv.iter().map(|a| t(a)).collect(),
            stdin: t(&rec.stdin),
            env: rec.env.iter().map(|(k, v)| (t(k), t(v))).collect(),
            files: rec.files.iter().map(|(k, v)| (t(k), t(v))).collect(),
            reads: rec.reads.map(|r| r.iter().map(|p| t(p)).collect()),
            stdout: t(&rec.stdout),
            stderr: t(&rec.stderr),
            exit: rec.exit,
            writes: rec
                .writes
                .iter()
                .map(|(k, v)| (t(k), v.as_deref().map(t)))
                .collect(),
        }
    }
}

/// True when `s` holds a [`LOCAL_SHAPES`] prefix followed by a name, as in
/// `/Users/alice`; a bare `/Users/` or a placeholder (`/Users/<name>`,
/// `{HOME}`) names nobody and passes.
fn has_local_path(s: &str) -> bool {
    LOCAL_SHAPES.iter().any(|shape| {
        s.match_indices(shape).any(|(i, m)| {
            s[i + m.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '.' || c == '_')
        })
    })
}

/// Every string of `rec` with a field label; map entries are labelled by index.
fn fields(rec: &Recording) -> Vec<(String, &str)> {
    let mut out: Vec<(String, &str)> = vec![
        ("stdin".into(), &rec.stdin),
        ("stdout".into(), &rec.stdout),
        ("stderr".into(), &rec.stderr),
    ];
    out.extend(
        rec.argv
            .iter()
            .enumerate()
            .map(|(i, a)| (format!("argv#{i}"), a.as_str())),
    );
    for (i, (k, v)) in rec.env.iter().enumerate() {
        out.push((format!("env#{i}"), k));
        out.push((format!("env#{i}"), v));
    }
    for (i, (k, v)) in rec.files.iter().enumerate() {
        out.push((format!("files#{i}"), k));
        out.push((format!("files#{i}"), v));
    }
    for (i, p) in rec.reads.iter().flatten().enumerate() {
        out.push((format!("reads#{i}"), p));
    }
    for (i, (k, v)) in rec.writes.iter().enumerate() {
        out.push((format!("writes#{i}"), k));
        out.extend(v.as_deref().map(|v| (format!("writes#{i}"), v)));
    }
    out
}

/// Refuses `rec` when any field still holds a local path shape, one of
/// `sessions` at a name boundary, or a secret-shaped value.
///
/// Why: the last gate before a public fixture (#12026 AC3).
/// What: scans every string [`fields`] lists; the first hit is the error.
/// Test: `check_refuses_an_unscrubbed_home_path_session_and_secret`,
/// `committed_replay_fixtures_pass_the_refusal_check`.
pub fn check(rec: &Recording, sessions: &[String]) -> Result<(), Refusal> {
    let session_rules: Vec<Rule> = sessions
        .iter()
        .filter(|n| !n.is_empty())
        .map(|n| Rule {
            needle: n.clone(),
            token: String::new(),
            bounded: true,
        })
        .collect();
    for (field, s) in fields(rec) {
        let kind = if has_local_path(s) {
            Some(RefusalKind::LocalPath)
        } else if s
            .char_indices()
            .any(|(i, _)| session_rules.iter().any(|r| matches_at(s, i, r)))
        {
            Some(RefusalKind::Session)
        } else if looks_secret(s) {
            Some(RefusalKind::Secret)
        } else {
            None
        };
        if let Some(kind) = kind {
            return Err(Refusal { field, kind });
        }
    }
    Ok(())
}
