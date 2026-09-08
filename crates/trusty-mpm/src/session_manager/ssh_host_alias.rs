//! The machine an SSH config `Host` alias actually names (#7196).
//!
//! Why: a multi-account operator gives each GitHub identity its own `Host`
//! block — `git@gh-work:acme-corp/widgets.git`, where `gh-work`
//! is an alias for `github.com` in `~/.ssh/config`. Git hands that alias to
//! `ssh`, which rewrites it; nothing else on the machine does. So
//! [`super::worktree_repo_slug`] read `gh-work` as the GitHub host,
//! `gh --repo gh-work/acme-corp/widgets` failed with "error connecting
//! to gh-work", and gate 5 of the merged-PR reclaim refused every one of
//! four worktrees whose pull requests had merged. Reproduced six times on
//! 2026-09-08; the effect is that no worktree on any repository behind a
//! per-account alias can ever be reclaimed.
//!
//! What: [`SshHostAliases`] parses the `Host` / `HostName` pairs out of an
//! OpenSSH client config and answers [`SshHostAliases::hostname_for`]. It is a
//! SUBSET of `ssh_config(5)`, chosen because the alternative — shelling to
//! `ssh -G` — puts a subprocess on a gate whose ALLOW deletes a checkout.
//! Supported: `Host` patterns with `*`, `?` and `!` negation, the `key value`
//! and `key = value` spellings, full-line `#` comments, and `%h` in a
//! `HostName`. NOT supported: `Include` and `Match`, both of which are simply
//! not followed.
//!
//! **An unsupported directive costs a REFUSAL, never a wrong answer.** An alias
//! whose `HostName` lives behind an `Include` reads here as "no entry", and
//! `worktree_repo_slug` turns a dotless host with no entry into a deny naming
//! the alias — the [ADR-0045] absent-vs-undeterminable rule. The failure mode
//! is a worktree that is not reclaimed, which is the direction this whole path
//! errs in.
//!
//! [ADR-0045]: ../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md
//!
//! Test: `ssh_host_alias_tests`.

use std::path::{Path, PathBuf};

/// The `~/.ssh/config` path, relative to the operator's home directory.
const USER_SSH_CONFIG: &str = ".ssh/config";

/// One `Host <patterns…>` block that carries a `HostName`.
///
/// A block with no `HostName` is never recorded: it renames nothing, so it can
/// only shadow a later block that does.
#[derive(Debug, Clone)]
struct AliasBlock {
    /// The block's `Host` patterns, in the order they were written.
    patterns: Vec<HostPattern>,
    /// The first `HostName` the block set — ssh takes the first value obtained.
    hostname: String,
}

/// One `Host` pattern, with the `!` negation ssh gives it.
#[derive(Debug, Clone)]
struct HostPattern {
    /// The glob, `!` already stripped.
    glob: String,
    /// True when the pattern was written `!glob`, which EXCLUDES a match.
    negated: bool,
}

/// The `Host` → `HostName` rewrites an SSH client config declares.
///
/// Why: see the module doc. Held as a value rather than read per lookup so one
/// reclaim sweep parses the file once and every worktree in it gets the same
/// answer.
/// What: the blocks that set a `HostName`, in file order.
/// [`SshHostAliases::hostname_for`] is the only question it answers.
/// Test: `an_alias_block_rewrites_its_host`, `the_first_matching_block_wins`.
#[derive(Debug, Clone, Default)]
pub(crate) struct SshHostAliases {
    /// `Host` blocks carrying a `HostName`, in file order.
    blocks: Vec<AliasBlock>,
}

impl SshHostAliases {
    /// The table that rewrites nothing.
    ///
    /// Used by every test that must not depend on the operator's real config,
    /// and by the parse of a config that could not be read.
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// The aliases in `~/.ssh/config`, or [`SshHostAliases::empty`].
    ///
    /// Why: this is where git's own `ssh` reads them, so it is the only file
    /// that can explain an alias a git remote uses. A missing or unreadable
    /// config yields an empty table — see the module doc on why that refuses
    /// rather than guesses.
    /// What: [`SshHostAliases::load`] against `$HOME/.ssh/config`; a home
    /// directory the platform cannot name yields the empty table too.
    /// Test: `a_config_file_on_disk_is_parsed` covers the read and parse. The
    /// `$HOME` join has no test of its own: reading the operator's real
    /// `~/.ssh/config` is exactly what a test must never do, so every test
    /// passes its own path to [`SshHostAliases::load`] instead.
    pub(crate) fn for_current_user() -> Self {
        match user_ssh_config_path() {
            Some(path) => Self::load(&path),
            None => Self::empty(),
        }
    }

    /// The aliases declared in the SSH client config at `path`.
    ///
    /// Why: the path is a parameter so a test can point at a fixture instead of
    /// the operator's own config, which no test may read.
    /// What: reads the file and parses it with [`SshHostAliases::parse`]; an
    /// unreadable file is the empty table.
    /// Test: `a_config_file_on_disk_is_parsed`,
    /// `a_missing_config_file_rewrites_nothing`.
    pub(crate) fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::empty(),
        }
    }

    /// The aliases declared in `text`, an SSH client config's contents.
    ///
    /// Why: the pure half, so the grammar is testable without a filesystem.
    /// What: walks the lines, opening a block on `Host` and recording the first
    /// `HostName` in it. `Match` closes the current block and opens none, so
    /// nothing inside a `Match` is attributed to the `Host` above it. Every
    /// other keyword is ignored.
    /// Test: `equals_separated_directives_parse`, `comments_and_case_are_ignored`,
    /// `a_match_block_is_not_attributed_to_the_host_above_it`.
    pub(crate) fn parse(text: &str) -> Self {
        let mut blocks: Vec<AliasBlock> = Vec::new();
        // The patterns of the block currently open, and whether it already set
        // a `HostName` (ssh keeps the FIRST value, so a second is ignored).
        let mut open: Option<(Vec<HostPattern>, bool)> = None;
        for line in text.lines() {
            let Some((keyword, args)) = directive(line) else {
                continue;
            };
            match keyword.as_str() {
                "host" => {
                    open = Some((args.iter().map(|a| HostPattern::parse(a)).collect(), false));
                }
                // Not followed — see the module doc. Closing the block is what
                // keeps a `HostName` written under a `Match` from being read as
                // the enclosing `Host`'s.
                "match" | "include" => open = None,
                "hostname" => {
                    if let Some((patterns, set)) = open.as_mut()
                        && !*set
                        && let Some(value) = args.first()
                    {
                        *set = true;
                        blocks.push(AliasBlock {
                            patterns: patterns.clone(),
                            hostname: value.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
        Self { blocks }
    }

    /// The real hostname `host` resolves to, or `None` when nothing renames it.
    ///
    /// Why: `gh --repo [HOST/]OWNER/REPO` addresses a GitHub SERVER, and an
    /// alias names no server — this is the step that turns `gh-work` into
    /// `github.com` before the slug is built (#7196).
    /// What: the first block whose patterns match, ssh's own rule (first value
    /// obtained wins). A block matches when `host` matches at least one
    /// positive pattern and no negated one. `%h` in the `HostName` expands to
    /// `host` itself, so `Host gh-*` / `HostName %h.example` behaves as ssh
    /// would. The answer is lowercased, because hostnames are case-insensitive
    /// and the slug is compared against a literal.
    /// Test: `an_alias_block_rewrites_its_host`, `the_first_matching_block_wins`,
    /// `a_negated_pattern_excludes_the_host`, `percent_h_expands_to_the_query`.
    pub(crate) fn hostname_for(&self, host: &str) -> Option<String> {
        let host = host.to_ascii_lowercase();
        self.blocks
            .iter()
            .find(|b| matches_block(&b.patterns, &host))
            .map(|b| b.hostname.replace("%h", &host).to_ascii_lowercase())
    }
}

impl HostPattern {
    /// One `Host` argument, with a leading `!` read as negation.
    fn parse(arg: &str) -> Self {
        match arg.strip_prefix('!') {
            Some(rest) => Self {
                glob: rest.to_ascii_lowercase(),
                negated: true,
            },
            None => Self {
                glob: arg.to_ascii_lowercase(),
                negated: false,
            },
        }
    }
}

/// `$HOME/.ssh/config`, or `None` when the platform names no home directory.
fn user_ssh_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(USER_SSH_CONFIG))
}

/// The lowercased keyword and its arguments, or `None` for a blank or comment.
///
/// Why: ssh accepts `Key value`, `Key=value` and `Key = value` alike, and
/// keywords are case-insensitive, so one splitter has to cover all of it.
/// What: strips a full-line `#` comment, splits the keyword off at the first
/// whitespace or `=`, and returns the rest split on whitespace. A trailing `#`
/// is NOT a comment — ssh does not treat it as one either.
fn directive(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split_at = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let (keyword, rest) = line.split_at(split_at);
    let rest = rest.trim_start().strip_prefix('=').unwrap_or(rest);
    let args: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
    if args.is_empty() {
        return None;
    }
    Some((keyword.to_ascii_lowercase(), args))
}

/// Does `host` match this block's pattern list, by ssh's rules?
///
/// A negated pattern that matches excludes the host outright, whatever else
/// matched; otherwise at least one positive pattern must match.
fn matches_block(patterns: &[HostPattern], host: &str) -> bool {
    if patterns
        .iter()
        .any(|p| p.negated && glob_matches(&p.glob, host))
    {
        return false;
    }
    patterns
        .iter()
        .any(|p| !p.negated && glob_matches(&p.glob, host))
}

/// Does the ssh `Host` glob `pattern` match `text`?
///
/// Why: ssh's patterns are `*` (any run) and `?` (any one character) — not
/// regular expressions and not shell globs with character classes. Written out
/// rather than pulled from a crate because that is the whole grammar.
/// What: a backtracking matcher over `char`s, so a multi-byte host is matched
/// by character and never split mid-codepoint.
/// Test: `wildcard_patterns_match_a_family_of_aliases`.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    // `star` remembers where the last `*` was so a failed tail can resume one
    // character later, which is what makes `*` greedy-but-backtracking.
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut resume) = (None, 0usize);
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                resume = t;
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some(c) if *c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some(s) => {
                    p = s + 1;
                    resume += 1;
                    t = resume;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
#[path = "ssh_host_alias_tests.rs"]
mod ssh_host_alias_tests;
