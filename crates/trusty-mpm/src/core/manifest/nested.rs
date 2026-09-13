//! Bounded NESTED-MANIFEST discovery for marker detection (#7781).
//!
//! Why: probing the project root plus the members a root manifest DECLARES
//! (`super::workspace`) still misses every stack a repository keeps one
//! directory down without declaring it. trusty-tools itself is the worked
//! example: a Cargo workspace whose Svelte/TypeScript UIs live in
//! `crates/*/ui`, declared by no root `workspaces` key, so root-only detection
//! answered `rust-engineer` and nothing else and the PM was primed to route
//! front-end work to a Rust specialist. Owner ruling 2026-09-13: root-only
//! stack detection is not enough.
//! What: [`nested_probe_roots`] walks the project tree breadth-first to
//! [`MAX_NESTED_DEPTH`] and returns every directory carrying a file that any
//! declared marker names — the ANCHOR set, derived from the manifest itself
//! ([`MarkerAnchors::from_categories`]) so a new marker needs no edit here.
//!
//! **Bounds, and why these numbers.** Session launch waits on this walk, so
//! every dimension is capped and every cap fails closed — an unscanned
//! directory is simply a directory with no markers, never an error. Failing
//! closed SILENTLY was the round-1 review finding, so every bound now also sets
//! [`NestedProbe::truncated`] and logs which one tripped: "no nested stack" and
//! "the walk gave up" are no longer the same answer.
//!
//! | Bound | Value | Rationale |
//! |---|---|---|
//! | Depth | [`MAX_NESTED_DEPTH`] = 4 | `crates/<crate>/ui`, `apps/<app>/web`, `packages/<pkg>/src-tauri` all sit at depth ≤ 3. Depth 4 covers them with one level of headroom. |
//! | Directories scanned | [`MAX_SCANNED_DIRS`] = 4096 | One `read_dir` each. trusty-tools, the largest repo on hand, has 617 directories within depth 4. |
//! | Probe roots | [`super::workspace::MAX_WORKSPACE_MEMBERS`] | Shared with declared members, so the two discovery paths cannot together exceed the cap either one respects alone. |
//!
//! Skipped outright: dependency, build output, and fixture trees
//! ([`SKIP_DIR_NAMES`]), every dot-directory (`.git`, `.claude/worktrees`,
//! `.venv`, a nested `.trusty-mpm`), and every plain directory name the root
//! `.gitignore` lists.
//! Test: `crates/trusty-mpm/src/core/manifest/nested_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::schema::{AgentCategories, matches_any};
use super::workspace::{MAX_WORKSPACE_MEMBERS, ProbeBudget, read_bounded};

/// Deepest directory level the nested walk descends to, project root = 0.
pub(crate) const MAX_NESTED_DEPTH: usize = 4;

/// Maximum directories the nested walk reads for one detection call.
pub(crate) const MAX_SCANNED_DIRS: usize = 4096;

/// Directory names never descended into, whatever the project declares.
///
/// Why: two kinds of tree carry marker files that are not this project's stack.
/// Dependency and build output holds a vendored third party's manifests —
/// `node_modules` alone can hold tens of thousands of `package.json` files. A
/// FIXTURE tree holds manifests written to be parsed by a test: #7781 measured
/// this repo and `crates/trusty-analyze/testdata/csharp` alone put
/// `dotnet-engineer` in the roster of a Rust workspace, which is precisely the
/// delegation-surface noise the gates exist to remove (#1941).
/// What: matched by exact directory name at every depth. Dot-directories are
/// excluded separately by [`SkipDirs::skips`], which covers `.git`,
/// `.claude/worktrees`, and every other hidden tree in one rule.
const SKIP_DIR_NAMES: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "vendor",
    "testdata",
    "test-data",
    "fixtures",
];

/// The file names a project's declared markers anchor on.
///
/// Why: the walk must decide "is this directory a project?" without a second,
/// hand-maintained copy of the marker table — the exact drift #4765 removed
/// from `project_lang`. Deriving the anchors from the SAME [`AgentCategories`]
/// the detection then evaluates means a marker added to
/// `framework-manifest.toml` is discovered nested with no code change here.
/// What: the first path segment of every declared marker, with any `::needle`
/// content-probe tail stripped — `package.json::"svelte"` anchors on
/// `package.json`, `src-tauri/tauri.conf.json` on `src-tauri`. Names carrying a
/// `*` are kept as globs and matched with the manifest's own glob vocabulary.
/// Test: `anchors_cover_every_bundled_marker`, `anchor_globs_match_dotnet`.
pub(crate) struct MarkerAnchors {
    /// Exact entry names (`Cargo.toml`, `package.json`, `src-tauri`).
    names: BTreeSet<String>,
    /// Glob anchors (`*.csproj`), matched via [`matches_any`].
    globs: Vec<String>,
}

impl MarkerAnchors {
    /// Derive the anchor set from every gated category of `categories`.
    pub(crate) fn from_categories(categories: &AgentCategories) -> Self {
        let mut names = BTreeSet::new();
        let mut globs = BTreeSet::new();
        let declared = categories
            .language
            .iter()
            .chain(categories.framework.iter())
            .chain(categories.platform.iter());
        for marker in declared.flat_map(|entry| entry.markers.iter()) {
            // A content probe's `::needle` tail says nothing about the file NAME.
            let path = marker.split("::").next().unwrap_or(marker);
            // Only the first segment names an entry of the candidate directory.
            let Some(head) = path.split('/').find(|s| !s.is_empty()) else {
                continue;
            };
            if head.contains('*') {
                globs.insert(head.to_string());
            } else {
                names.insert(head.to_string());
            }
        }
        Self {
            names,
            globs: globs.into_iter().collect(),
        }
    }

    /// Whether a directory entry named `name` anchors a nested project.
    fn matches(&self, name: &str) -> bool {
        self.names.contains(name) || matches_any(name, &self.globs)
    }
}

/// Directory names the walk refuses to descend into for this project.
///
/// Why: the fixed list covers what every ecosystem generates; the root
/// `.gitignore` covers what THIS project generates, which no fixed list can
/// know. Reading it is one bounded file read and removes a whole class of
/// false positives (a build tree whose copied `package.json` is not a project).
/// What: [`SKIP_DIR_NAMES`] plus every plain directory name the root
/// `.gitignore` lists. Only bare names are taken — an entry with a path
/// separator, a glob, or a `!` negation is left to git, because honouring it
/// would mean implementing gitignore semantics rather than reading a name.
/// Test: `gitignored_directory_is_skipped`, `gitignore_path_rules_are_ignored`.
struct SkipDirs(BTreeSet<String>);

impl SkipDirs {
    /// The skip set for `project_dir`, including its root `.gitignore` names.
    fn for_project(project_dir: &Path, budget: &ProbeBudget) -> Self {
        let mut names: BTreeSet<String> = SKIP_DIR_NAMES.iter().map(|s| (*s).to_string()).collect();
        if let Some(body) = read_bounded(&project_dir.join(".gitignore"), budget) {
            names.extend(gitignore_dir_names(&body));
        }
        Self(names)
    }

    /// Whether a child directory named `name` is skipped.
    fn skips(&self, name: &str) -> bool {
        name.starts_with('.') || self.0.contains(name)
    }
}

/// The bare directory names a `.gitignore` body lists.
///
/// Why: factored out so the deliberately narrow parse is testable on its own.
/// What: one name per line, after dropping blanks, comments, and negations, and
/// after stripping a leading and trailing `/`. A leading `**/` is stripped too:
/// `**/target/` is gitignore's spelling of "this bare name, at any depth",
/// which is exactly what this skip set means, and it is the spelling most
/// `.gitignore` files actually use (#7781). A remaining `/`, or any glob
/// metacharacter, still disqualifies the line — those are real path and glob
/// semantics, left to git.
/// Test: `gitignore_path_rules_are_ignored`.
fn gitignore_dir_names(body: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in body.lines() {
        let rule = line.trim();
        if rule.is_empty() || rule.starts_with('#') || rule.starts_with('!') {
            continue;
        }
        let rule = rule.trim_end_matches('/').trim_start_matches('/');
        // #7781: `**/<name>/` means the bare name at any depth — the same rule
        // this skip set applies — so the prefix is dropped before the tests below.
        let rule = rule.strip_prefix("**/").unwrap_or(rule);
        if rule.is_empty() || rule.contains('/') || rule.contains(['*', '?', '[']) {
            continue;
        }
        names.push(rule.to_string());
    }
    names
}

/// One directory's scan: does it anchor a project, and where does the walk go next.
struct DirScan {
    /// True when the directory carries an entry any declared marker anchors on.
    anchored: bool,
    /// The directory's child directories, sorted, minus the skipped ones.
    children: Vec<PathBuf>,
}

/// Read `dir` once, answering both questions the walk asks of it.
///
/// Why: one `read_dir` per directory is the whole cost model of the walk;
/// asking "is it anchored?" and "what is below it?" separately would double it.
/// What: `None` when the directory cannot be read — fail-closed, identical to an
/// empty directory. Children are sorted so a capped walk is reproducible. A
/// SYMLINK to a directory is not a child: `file_type` does not follow links, so
/// the walk cannot leave the project tree, matching the no-escape rule
/// `super::workspace::expand_member_glob` applies to a declared member pattern.
/// Test: `walk_is_deterministic`, `missing_root_is_not_an_error`,
/// `unreadable_subdirectory_is_skipped_not_an_error`,
/// `directory_symlink_is_not_traversed`.
fn scan_dir(dir: &Path, anchors: &MarkerAnchors, skip: &SkipDirs) -> Option<DirScan> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut anchored = false;
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if anchors.matches(&name) {
            anchored = true;
        }
        // #7781: `file_type()` reports the LINK, where `path().is_dir()` follows
        // it — a directory symlink would otherwise let the walk escape the
        // project (or loop) and report an outside tree as this project's stack.
        if !skip.skips(&name) && entry.file_type().is_ok_and(|t| t.is_dir()) {
            children.push(entry.path());
        }
    }
    children.sort();
    Some(DirScan { anchored, children })
}

/// The outcome of one nested walk: what it found, and whether it saw everything.
///
/// Why: every bound in this module fails closed by returning fewer roots, and a
/// short result is byte-identical to a genuinely shallow tree. Round-1 review of
/// #7781 called that out: a consumer cannot tell "this repo has no nested stack"
/// from "the walk gave up before reaching it", so a truncated detection silently
/// becomes a confident, wrong answer about the project's stack.
/// What: the probe roots, plus one flag set by whichever bound stopped the walk
/// — the depth limit leaving unwalked children, [`MAX_SCANNED_DIRS`], or the
/// shared [`MAX_WORKSPACE_MEMBERS`] ceiling. Each sets it beside a
/// `tracing::warn!` naming the bound, so the reason is in the log even where the
/// flag is only rendered as one line.
/// Test: `scanned_dirs_bound_reports_truncation`,
/// `a_small_tree_is_not_truncated`, `depth_bound_stops_the_walk`,
/// `member_cap_is_shared`.
pub(crate) struct NestedProbe {
    /// Nested directories below the root that carry a declared marker.
    pub(crate) roots: Vec<PathBuf>,
    /// True when a bound stopped the walk before the tree was exhausted.
    pub(crate) truncated: bool,
}

/// Log the bound that truncated a nested walk (#7781).
///
/// Why: the flag on [`NestedProbe`] says detection is incomplete; only the log
/// says WHICH ceiling to raise, and raising the wrong one fixes nothing.
/// What: one WARN naming the bound and the project it tripped on.
/// Test: `scanned_dirs_bound_reports_truncation` asserts the WARN reaches a
/// subscriber.
fn warn_truncated(project_dir: &Path, bound: &str) {
    tracing::warn!(
        bound,
        project = %project_dir.display(),
        "#7781: nested stack detection was truncated by a scan bound; a nested \
         project past it is undetected"
    );
}

/// The nested directories under `project_dir` that carry a declared marker.
///
/// Why: the one entry point [`super::project_lang::MarkerProbe`] calls for
/// undeclared nested projects, so "how deep, how wide, what is skipped" is
/// stated in exactly one place (#7781).
/// What: a breadth-first walk to [`MAX_NESTED_DEPTH`], returning every visited
/// directory below the root that [`MarkerAnchors`] matches, in deterministic
/// (breadth-first, sorted-per-directory) order, plus the
/// [`NestedProbe::truncated`] flag described there. `project_dir` itself is
/// never returned — the caller already probes it. `already` carries the roots
/// resolved so far so the shared [`MAX_WORKSPACE_MEMBERS`] ceiling counts both
/// discovery paths together and declared members are not returned twice.
/// Test: `nested_ui_package_is_found`, `depth_bound_stops_the_walk`,
/// `skipped_directories_are_never_probed`, `member_cap_is_shared`,
/// `scanned_dirs_bound_reports_truncation`, `a_small_tree_is_not_truncated`.
pub(crate) fn nested_probe_roots(
    project_dir: &Path,
    anchors: &MarkerAnchors,
    budget: &ProbeBudget,
    already: &[PathBuf],
) -> NestedProbe {
    let skip = SkipDirs::for_project(project_dir, budget);
    let mut found: Vec<PathBuf> = Vec::new();
    let mut frontier = vec![project_dir.to_path_buf()];
    let mut scanned = 0usize;
    // #7781: set once, warned once after the walk — a per-directory warn would
    // fire for every deep directory in a large repo.
    let mut depth_truncated = false;

    for depth in 0..=MAX_NESTED_DEPTH {
        let mut next = Vec::new();
        for dir in std::mem::take(&mut frontier) {
            scanned += 1;
            if scanned > MAX_SCANNED_DIRS {
                warn_truncated(project_dir, "MAX_SCANNED_DIRS");
                return NestedProbe {
                    roots: found,
                    truncated: true,
                };
            }
            let Some(scan) = scan_dir(&dir, anchors, &skip) else {
                continue;
            };
            // Depth 0 is the project root: already a probe root, never re-added.
            if depth > 0 && scan.anchored && !already.contains(&dir) && !found.contains(&dir) {
                if already.len() + found.len() > MAX_WORKSPACE_MEMBERS {
                    warn_truncated(project_dir, "MAX_WORKSPACE_MEMBERS");
                    return NestedProbe {
                        roots: found,
                        truncated: true,
                    };
                }
                found.push(dir);
            }
            if depth < MAX_NESTED_DEPTH {
                next.extend(scan.children);
            } else if !scan.children.is_empty() {
                depth_truncated = true;
            }
        }
        frontier = next;
    }
    if depth_truncated {
        warn_truncated(project_dir, "MAX_NESTED_DEPTH");
    }
    NestedProbe {
        roots: found,
        truncated: depth_truncated,
    }
}

#[cfg(test)]
#[path = "nested_tests.rs"]
mod tests;
