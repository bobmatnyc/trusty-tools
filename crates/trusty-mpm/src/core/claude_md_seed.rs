//! Is a `CLAUDE.md` tm's own seed template, and may tm seed one here? (#7673)
//!
//! Why: on 2026-09-12 a tm-seeded template `CLAUDE.md` was found at `$HOME`.
//! Claude Code loads every `CLAUDE.md` from the session's cwd up to the
//! filesystem root, so that one file rode into every turn of every agent in
//! every project under the home directory — pure token cost, no content, and
//! nothing in the harness said so. Two halves answer it: a SHAPE test, so the
//! file can be recognised as tm's boilerplate rather than the operator's own
//! notes, and a SITE test, so the seeding code path cannot write one above a
//! project again.
//!
//! What: [`is_seed_template`] compares a file's prose against
//! [`crate::core::instruction_pipeline::CLAUDE_MD_STUB`] — the one seed-shape
//! source in the crate, so there is no second spelling of "this is the stub".
//! [`refuse_seed_at`] is the site test, and is an ERROR arm at its call site:
//! it never warns-and-writes-anyway.
//! Test: `claude_md_seed_tests.rs`.

use std::path::Path;

use crate::core::instruction_pipeline::CLAUDE_MD_STUB;

/// Directory entries that mark a directory as a project root (#7673).
///
/// Why: the daemon's project registry is keyed by repo URL, not by local path
/// (`project::record::Project` has no path field), so it cannot answer "is this
/// DIRECTORY a registered project". What can answer it is what is on disk: a
/// git checkout, or tm's own per-project state — either of which tm itself
/// created the first time the operator ran it there.
/// What: `.git` (a repo or a worktree pointer file), `.trusty-mpm` (the harness
/// root a registered project carries), and `.trusty-mpm.toml` (the per-project
/// config). Any one of them is sufficient.
/// Test: `a_git_checkout_is_a_project_root`,
/// `a_harness_root_is_a_project_root`, `a_bare_directory_is_not_a_project_root`.
const PROJECT_ROOT_MARKERS: [&str; 3] = [".git", ".trusty-mpm", ".trusty-mpm.toml"];

/// Why tm declined to seed a `CLAUDE.md` at a directory.
///
/// Why: the two refusals need different words. `$HOME` is refused even when it
/// looks like a project (an operator's dotfiles repo is a git checkout), because
/// a file there is an ancestor of every project beneath it. A non-project
/// directory is refused because nothing establishes that a session there is a
/// project session at all.
/// What: two variants, each rendering its own operator-facing message through
/// [`SeedRefusal::message`].
/// Test: `seeding_into_home_is_refused`,
/// `seeding_into_a_bare_directory_is_refused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedRefusal {
    /// The target directory is the operator's home directory.
    Home,
    /// The target directory carries no [`PROJECT_ROOT_MARKERS`] entry.
    NotAProjectRoot,
}

impl SeedRefusal {
    /// The operator-facing explanation, naming the path.
    ///
    /// Why: a refusal the operator cannot act on is a wedge. Both messages name
    /// the exact directory and what would make the seed legitimate.
    /// What: one sentence per variant, with `dir` interpolated.
    /// Test: `the_home_refusal_names_the_path`.
    pub fn message(self, dir: &Path) -> String {
        match self {
            Self::Home => format!(
                "refusing to seed a CLAUDE.md at the home directory {} — Claude Code loads \
                 every CLAUDE.md from the session cwd up to the filesystem root, so a file \
                 here would be prepended to every session in every project beneath it. Start \
                 the session from the project directory instead.",
                dir.display()
            ),
            Self::NotAProjectRoot => format!(
                "refusing to seed a CLAUDE.md at {} — it is not a project root (no .git, \
                 .trusty-mpm/ or .trusty-mpm.toml). Run tm from the project's own directory, \
                 or register the project there first.",
                dir.display()
            ),
        }
    }
}

/// May tm write a seed `CLAUDE.md` into `dir`?
///
/// Why: see the module header — this is the root-cause guard for the `$HOME`
/// seed. It is deliberately a check on the DIRECTORY rather than on the caller,
/// so every seeding path inherits it by calling the one seeder.
/// What: `Some(SeedRefusal::Home)` when `dir` resolves to `home`;
/// `Some(SeedRefusal::NotAProjectRoot)` when it carries no
/// [`PROJECT_ROOT_MARKERS`] entry; `None` when seeding is allowed. `home` is
/// INJECTED rather than read from the environment so a test can point it at a
/// temp directory without a process-global `$HOME` write (#5544).
/// Test: `seeding_into_home_is_refused`,
/// `seeding_into_a_bare_directory_is_refused`,
/// `a_git_checkout_is_a_project_root`, `a_harness_root_is_a_project_root`.
pub fn refuse_seed_at(dir: &Path, home: Option<&Path>) -> Option<SeedRefusal> {
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if let Some(home) = home
        && resolve(dir) == resolve(home)
    {
        return Some(SeedRefusal::Home);
    }
    if PROJECT_ROOT_MARKERS
        .iter()
        .any(|marker| dir.join(marker).exists())
    {
        return None;
    }
    Some(SeedRefusal::NotAProjectRoot)
}

/// Is `content` tm's seed template with nothing of the operator's added?
///
/// Why: the doctor check's 🔴 verdict and `--fix`'s rename both hinge on "this
/// file is pure cost". Only a file whose prose is byte-for-byte the stub's can
/// carry that verdict — anything else is content someone may want, and is a ⚠️.
/// What: normalises both sides with [`normalized`] (HTML comments dropped, each
/// line trimmed, blank lines dropped) and compares. Comments are dropped
/// because Claude Code strips them when it loads the file, so a seed whose
/// comments were edited is still a seed; everything else must match exactly,
/// which keeps the destructive verdict conservative.
/// Test: `the_stub_is_a_seed_template`,
/// `a_stub_with_the_comments_stripped_is_still_a_seed_template`,
/// `a_stub_with_one_added_line_is_not_a_seed_template`,
/// `an_unrelated_file_is_not_a_seed_template`.
pub fn is_seed_template(content: &str) -> bool {
    normalized(content) == normalized(CLAUDE_MD_STUB)
}

/// The comparison form [`is_seed_template`] uses.
///
/// What: removes every `<!-- … -->` span (an unterminated one takes the rest of
/// the input, which is the safe direction — it can only make two files compare
/// equal when both are truncated the same way), trims each remaining line, and
/// joins the non-empty ones with a newline.
/// Test: see [`is_seed_template`].
fn normalized(content: &str) -> String {
    let mut stripped = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("<!--") {
        stripped.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => rest = "",
        }
    }
    stripped.push_str(rest);
    stripped
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "claude_md_seed_tests.rs"]
mod tests;
