//! Deterministic scrub of local paths and session names, and the refusal check.
//!
//! Why: this repository is public (#12026 AC3). A recording names the sandbox
//! root, the script's own directory and the owner's home in tracebacks and
//! output, and fleet session names in ledger text.
//! What: [`Scrubber`] replaces each registered needle with its token in one
//! left-to-right pass, longest needle first, so a token is never rescanned and
//! the same input always gives the same output. Session names match only at a
//! name boundary and become `session-<n>`, numbered in sorted order. A path
//! matches only where a path component ends. [`check`] refuses a home or temp
//! path shape, a listed session name, any `tm-<x>` token that is not a
//! [`SKILL_NAMES`] entry, a secret-shaped value or a secret-named env key, and
//! names the field, never the value.
//! Test: `scrub_is_deterministic_and_order_independent`,
//! `scrub_replaces_paths_and_bounded_session_names`,
//! `scrub_matches_a_path_only_at_a_component_boundary`,
//! `check_refuses_an_unscrubbed_home_path_session_and_secret`,
//! `check_refuses_an_unlisted_tm_session_name_but_not_a_skill_name`,
//! `check_refuses_a_secret_named_env_key`,
//! `check_refuses_a_segment_or_camel_case_credential_key_and_a_url_password`.

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

/// The `tm-` skill names under the repo's `content/skills/`. Any other
/// `tm-<x>` token is a session name (Architect ruling, #12026): a skill added
/// later is refused until it is listed here, so the list fails closed.
/// Test: `skill_names_match_the_repo_skill_list`.
pub const SKILL_NAMES: [&str; 28] = [
    "tm-adr",
    "tm-agent-architecture",
    "tm-architect-setup",
    "tm-bug-reporting",
    "tm-capabilities",
    "tm-circuit-breaker",
    "tm-cli-operations",
    "tm-delegation-patterns",
    "tm-doctor",
    "tm-epic",
    "tm-estimation",
    "tm-git-file-tracking",
    "tm-init",
    "tm-issues-prune",
    "tm-memory-import",
    "tm-owner-scenarios",
    "tm-postmortem",
    "tm-prose-style",
    "tm-secrets",
    "tm-session-management",
    "tm-session-pause",
    "tm-session-resume",
    "tm-slack",
    "tm-teaching-templates",
    "tm-ticketing",
    "tm-tool-usage-guide",
    "tm-verification-protocols",
    "tm-workflow",
];

/// Env key fragments that name a credential, matched caselessly anywhere in
/// the key.
const SECRET_KEY_PARTS: [&str; 9] = [
    "PASSWORD",
    "PASSWD",
    "SECRET",
    "CREDENTIAL",
    "API_KEY",
    "APIKEY",
    "API-KEY",
    "PRIVATE_KEY",
    "ACCESS_KEY",
];

/// Env key segments that name a credential, matched whole so `TOKENIZERS_*`,
/// `KEYBOARD_*`, `PATH` and `BYPASS_*` pass (see [`key_segments`]).
const SECRET_KEY_SEGMENTS: [&str; 9] = [
    "TOKEN", "TOKENS", "KEY", "KEYS", "PAT", "PASS", "PW", "COOKIE", "COOKIES",
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
    bound: Bound,
}

/// Where a [`Rule`] may match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    /// A path: not followed by a name character, so a `<home>/al` needle
    /// never matches inside `<home>/alice`.
    Path,
    /// A session name: a name character on neither side.
    Name,
}

/// Replaces local paths and session names with stable tokens.
#[derive(Debug, Clone, Default)]
pub struct Scrubber {
    rules: Vec<Rule>,
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// True when `needle` occurs in `s` at `i` and is not part of a longer path
/// component or name.
fn matches_at(s: &str, i: usize, rule: &Rule) -> bool {
    if !s[i..].starts_with(&rule.needle) {
        return false;
    }
    // #12026: a path needle ends at a component boundary too.
    let after = s[i + rule.needle.len()..]
        .chars()
        .next()
        .is_some_and(is_name_char);
    let before = rule.bound == Bound::Name && s[..i].chars().next_back().is_some_and(is_name_char);
    !before && !after
}

/// True when `s` holds a `tm-<x>` token at a name boundary that is not a
/// [`SKILL_NAMES`] entry.
fn has_session_token(s: &str) -> bool {
    s.match_indices("tm-").any(|(i, _)| {
        if s[..i].chars().next_back().is_some_and(is_name_char) {
            return false;
        }
        let rest = &s[i + 3..];
        let len = rest.find(|c| !is_name_char(c)).unwrap_or(rest.len());
        len > 0 && !SKILL_NAMES.contains(&&s[i..i + 3 + len])
    })
}

/// `k` split at every non-alphanumeric character and at each camelCase
/// boundary (`githubToken`, `GHToken`), each segment upper-cased.
fn key_segments(k: &str) -> Vec<String> {
    let chars: Vec<char> = k.chars().collect();
    let mut segs = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            segs.push(std::mem::take(&mut cur));
            continue;
        }
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        let camel = c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase() || p.is_ascii_digit() || (p.is_uppercase() && next_lower)
            });
        if camel && !cur.is_empty() {
            segs.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_uppercase());
    }
    segs.push(cur);
    segs.retain(|s| !s.is_empty());
    segs
}

/// True when env key `k` names a credential: a [`SECRET_KEY_PARTS`] fragment
/// anywhere, a [`SECRET_KEY_SEGMENTS`] segment, or a final `AUTH` segment
/// (`NPM_AUTH`; `SSH_AUTH_SOCK` passes).
fn secret_env_key(k: &str) -> bool {
    let upper = k.to_ascii_uppercase();
    let segs = key_segments(k);
    SECRET_KEY_PARTS.iter().any(|p| upper.contains(p))
        || segs
            .iter()
            .any(|s| SECRET_KEY_SEGMENTS.contains(&s.as_str()))
        || segs.last().is_some_and(|s| s == "AUTH")
}

/// True when `v` holds a URL with a password, `scheme://user:pw@host`.
fn has_url_password(v: &str) -> bool {
    v.match_indices("://").any(|(i, m)| {
        let rest = &v[i + m.len()..];
        let end = rest
            .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .unwrap_or(rest.len());
        let authority = &rest[..end];
        authority.rfind('@').is_some_and(|at| {
            authority[..at]
                .split_once(':')
                .is_some_and(|(_, pw)| !pw.is_empty())
        })
    })
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
                self.push(needle, token.to_owned(), Bound::Path);
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
            self.push(name.clone(), format!("session-{}", n + 1), Bound::Name);
        }
        self
    }

    fn push(&mut self, needle: String, token: String, bound: Bound) {
        if self.rules.iter().any(|r| r.needle == needle) {
            return;
        }
        self.rules.push(Rule {
            needle,
            token,
            bound,
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

/// True when `s` holds what [`check`] refuses with no listed session: a local
/// path, an unlisted `tm-<x>` token or a secret shape.
/// Test: `replay_withholds_a_secret_line_whose_keyword_falls_outside_the_excerpt`.
pub(super) fn leaks(s: &str) -> bool {
    leak_kind(s, &[]).is_some()
}

/// The first [`RefusalKind`] `s` holds, with `sessions` as listed names.
fn leak_kind(s: &str, sessions: &[Rule]) -> Option<RefusalKind> {
    if has_local_path(s) {
        Some(RefusalKind::LocalPath)
    } else if has_session_token(s)
        || s.char_indices()
            .any(|(i, _)| sessions.iter().any(|r| matches_at(s, i, r)))
    {
        Some(RefusalKind::Session)
    } else if looks_secret(s) {
        Some(RefusalKind::Secret)
    } else {
        None
    }
}

/// Refuses `rec` when any field still holds a local path shape, one of
/// `sessions` or an unlisted `tm-<x>` token at a name boundary, a
/// secret-shaped value, or an env entry whose key names a credential.
///
/// Why: the last gate before a public fixture (#12026 AC3).
/// What: scans every string [`fields`] lists, then each env entry as
/// `KEY=VALUE`, by key name and for a URL password; the first hit is the error.
/// Test: `check_refuses_an_unscrubbed_home_path_session_and_secret`,
/// `check_refuses_an_unlisted_tm_session_name_but_not_a_skill_name`,
/// `check_refuses_a_secret_named_env_key`,
/// `check_refuses_a_segment_or_camel_case_credential_key_and_a_url_password`,
/// `committed_replay_fixtures_pass_the_refusal_check`.
pub fn check(rec: &Recording, sessions: &[String]) -> Result<(), Refusal> {
    let session_rules: Vec<Rule> = sessions
        .iter()
        .filter(|n| !n.is_empty())
        .map(|n| Rule {
            needle: n.clone(),
            token: String::new(),
            bound: Bound::Name,
        })
        .collect();
    for (field, s) in fields(rec) {
        if let Some(kind) = leak_kind(s, &session_rules) {
            return Err(Refusal { field, kind });
        }
    }
    // #12026: a key and its value are separate fields, so `GITHUB_TOKEN` and
    // its value each pass alone; the pair and the key name are checked here.
    for (i, (k, v)) in rec.env.iter().enumerate() {
        if secret_env_key(k) || has_url_password(v) || looks_secret(&format!("{k}={v}")) {
            return Err(Refusal {
                field: format!("env#{i}"),
                kind: RefusalKind::Secret,
            });
        }
    }
    Ok(())
}
