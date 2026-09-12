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

/// Why tm declined to seed a `CLAUDE.md`.
///
/// Why: the refusals need different words. `$HOME` is refused even when it looks
/// like a project (an operator's dotfiles repo is a git checkout), because a
/// file there is an ancestor of every project beneath it. A directory ABOVE
/// `$HOME` is refused for the same reason and more so. A target path with no
/// directory component is refused because the site cannot be judged at all.
/// What: three variants, each rendering its own operator-facing message through
/// [`SeedRefusal::message`].
/// Test: `seeding_into_home_is_refused`,
/// `seeding_above_the_home_directory_is_refused`,
/// `a_path_with_no_directory_component_is_refused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedRefusal {
    /// The target directory is the operator's home directory.
    Home,
    /// The target directory is a strict ancestor of the home directory and
    /// belongs to no git working tree — `/`, `/Users`, `/home`.
    AboveHome,
    /// The target path has no directory component to judge (#7673 review).
    NoDirectory,
}

impl SeedRefusal {
    /// The operator-facing explanation, naming the path.
    ///
    /// Why: a refusal the operator cannot act on is a wedge. Every message names
    /// the exact path and what would make the seed legitimate.
    /// Test: `the_home_refusal_names_the_path`.
    pub fn message(self, path: &Path) -> String {
        match self {
            Self::Home => format!(
                "refusing to seed a CLAUDE.md at the home directory {} — Claude Code loads \
                 every CLAUDE.md from the session cwd up to the filesystem root, so a file \
                 here would be prepended to every session in every project beneath it. Start \
                 the session from the project directory instead.",
                path.display()
            ),
            Self::AboveHome => format!(
                "refusing to seed a CLAUDE.md at {} — it sits above the home directory, so \
                 Claude Code would prepend the file to every session in every project \
                 beneath it. Start the session from the project directory instead.",
                path.display()
            ),
            Self::NoDirectory => format!(
                "refusing to seed a CLAUDE.md at {} — the path has no directory component, \
                 so the seed site cannot be checked against the home directory. Pass the \
                 project's own directory.",
                path.display()
            ),
        }
    }
}

/// May tm write a seed `CLAUDE.md` into `dir`?
///
/// Why: see the module header — this is the root-cause guard for the `$HOME`
/// seed, and the incident WAS `$HOME`. An earlier round of this guard also
/// demanded a `.git`/`.trusty-mpm` marker in `dir` ITSELF, which refused two
/// documented, previously-working surfaces: a session started from a
/// SUBDIRECTORY of a git repo, which `tm session start`'s own
/// `refuse_outside_a_git_project` gate already certifies, and
/// `tm sessions instructions --dir <dir>` on a directory tm has never touched.
/// So the marker test is gone and the rule is home-relative: a site is refused
/// only when a file there would load into sessions that are not this project's.
/// What: `Some(SeedRefusal::Home)` when `dir` resolves to `home`, checked FIRST
/// and unconditionally, so a dotfiles repo at `$HOME` cannot talk its way past
/// it; `Some(SeedRefusal::AboveHome)` when `dir` is a STRICT ancestor of `home`
/// and [`crate::core::harness_root::harness_root_for`] — the codebase's one
/// project-root definition, the same one `refuse_outside_a_git_project` uses —
/// finds no git working tree owning it; `None` otherwise, which is every
/// directory inside a git project at any depth and every first-touch directory.
/// `home` is INJECTED rather than read from the environment so a test can point
/// it at a temp directory without a process-global `$HOME` write (#5544); a
/// `None` home disables both arms, because with no home there is no home to
/// seed above.
/// Test: `seeding_into_home_is_refused`,
/// `seeding_above_the_home_directory_is_refused`,
/// `a_bare_first_touch_directory_is_seeded`,
/// `a_subdirectory_of_a_git_project_is_seeded`.
pub fn refuse_seed_at(dir: &Path, home: Option<&Path>) -> Option<SeedRefusal> {
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let home = resolve(home?);
    let dir = resolve(dir);
    if dir == home {
        return Some(SeedRefusal::Home);
    }
    // A strict ancestor of `$HOME` is at least as bad as `$HOME` itself. The git
    // probe runs ONLY on this arm — a handful of directories per machine — so
    // the ordinary launch path never pays for it.
    if home.starts_with(&dir) && crate::core::harness_root::harness_root_for(&dir).is_none() {
        return Some(SeedRefusal::AboveHome);
    }
    None
}

/// [`refuse_seed_at`] for a `CLAUDE.md` FILE path, failing closed (#7673 review).
///
/// Why: the call site used to derive the directory with
/// `path.parent().filter(non-empty)` inside an `if let`, so a degenerate
/// relative path — what `--dir ""` produces — made the binding fail and SKIPPED
/// the guard entirely, falling through to the seeding write with no check at
/// all. That is the same silent-seed shape this guard exists to close, reached
/// through a different input. A path whose site cannot be determined is refused,
/// never waved through.
/// What: delegates to [`refuse_seed_at`] on `path`'s parent; an absent or empty
/// parent is [`SeedRefusal::NoDirectory`].
/// Test: `a_path_with_no_directory_component_is_refused`,
/// `load_or_create_claude_md_fails_closed_on_a_path_with_no_directory`.
pub fn refuse_seed_for(path: &Path, home: Option<&Path>) -> Option<SeedRefusal> {
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => refuse_seed_at(dir, home),
        None => Some(SeedRefusal::NoDirectory),
    }
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
