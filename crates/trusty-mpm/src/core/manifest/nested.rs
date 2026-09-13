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
//! closed SILENTLY was the round-1 review finding, so every bound now reports
//! itself. #7781 round-3 splits that report in two, because the two kinds of
//! bound mean different things: a RESOURCE cap stopping the walk is an
//! exceptional, act-on-it event ([`NestedProbe::truncated`], WARN), while
//! stopping at the declared depth is this design's ordinary scope and trips on
//! most real repositories ([`NestedProbe::depth_limited`], DEBUG). One flag
//! covering both was true nearly always, so a consumer could not fail closed
//! on it.
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
use super::workspace::{MAX_WORKSPACE_MEMBERS, ProbeBudget, read_bounded, warn_truncated};

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

/// The outcome of one nested walk: what it found, and why it stopped.
///
/// Why: every bound in this module fails closed by returning fewer roots, and a
/// short result is byte-identical to a genuinely shallow tree. Round-1 review of
/// #7781 called that out: a consumer cannot tell "this repo has no nested stack"
/// from "the walk gave up before reaching it". Round-2 answered it with ONE
/// flag, which the depth bound then set on most real repositories — a signal
/// that is nearly always true cannot be failed closed on (#7751), so round-3
/// splits the exceptional case from the designed one.
/// What: the probe roots, plus [`Self::truncated`] for the RESOURCE caps
/// ([`MAX_SCANNED_DIRS`], the shared [`MAX_WORKSPACE_MEMBERS`] ceiling), each
/// beside a `tracing::warn!` naming the bound, and [`Self::depth_limited`] for
/// [`MAX_NESTED_DEPTH`], logged at DEBUG because a deep repository trips it as
/// designed. The two are independent: either, both, or neither can be set.
/// Test: `scanned_dirs_bound_reports_truncation`,
/// `a_small_tree_is_not_truncated`, `depth_bound_stops_the_walk`,
/// `depth_limited_tree_is_not_truncated`, `member_cap_is_shared`,
/// `both_bounds_can_trip_together`, `depth_limit_is_logged_at_debug`.
pub(crate) struct NestedProbe {
    /// Nested directories below the root that carry a declared marker.
    pub(crate) roots: Vec<PathBuf>,
    /// True when a RESOURCE cap abandoned the walk with the tree unexhausted.
    pub(crate) truncated: bool,
    /// True when children were left unread at [`MAX_NESTED_DEPTH`].
    pub(crate) depth_limited: bool,
}

/// Log that the walk stopped at its declared depth (#7781).
///
/// Why: round-3 — [`MAX_NESTED_DEPTH`] is tripped by ordinary repositories, so
/// warning on it trained operators to ignore the warning that means a resource
/// cap really was exhausted. DEBUG keeps the fact available without claiming
/// anything went wrong.
/// What: one DEBUG naming the depth and the project. The flag it accompanies is
/// [`NestedProbe::depth_limited`], which is informational, never fail-closed.
/// Test: `depth_limit_is_logged_at_debug` captures it through a `LogBuffer`.
fn debug_depth_limited(project_dir: &Path) {
    tracing::debug!(
        depth = MAX_NESTED_DEPTH,
        project = %project_dir.display(),
        "#7781: nested stack detection stopped at its declared depth; manifests \
         below it were not probed"
    );
}

/// The nested directories under `project_dir` that carry a declared marker.
///
/// Why: the one entry point [`super::project_lang::MarkerProbe`] calls for
/// undeclared nested projects, so "how deep, how wide, what is skipped" is
/// stated in exactly one place (#7781).
/// What: a breadth-first walk to [`MAX_NESTED_DEPTH`], returning every visited
/// directory below the root that [`MarkerAnchors`] matches, in deterministic
/// (breadth-first, sorted-per-directory) order, plus the two independent
/// [`NestedProbe`] flags described there. `project_dir` itself is never returned
/// — the caller already probes it. `already` carries the roots resolved so far
/// so the shared [`MAX_WORKSPACE_MEMBERS`] ceiling counts both discovery paths
/// together and declared members are not returned twice.
/// Test: `nested_ui_package_is_found`, `depth_bound_stops_the_walk`,
/// `skipped_directories_are_never_probed`, `member_cap_is_shared`,
/// `scanned_dirs_bound_reports_truncation`, `a_small_tree_is_not_truncated`,
/// `depth_limited_tree_is_not_truncated`, `both_bounds_can_trip_together`.
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
    // #7781: both are set once and logged once, after the walk — a per-directory
    // log would fire for every deep directory in a large repo, and an early
    // return that logged only its own bound dropped the other one's report.
    let mut depth_limited = false;
    let mut truncated_by: Option<&'static str> = None;

    'walk: for depth in 0..=MAX_NESTED_DEPTH {
        let mut next = Vec::new();
        for dir in std::mem::take(&mut frontier) {
            scanned += 1;
            if scanned > MAX_SCANNED_DIRS {
                truncated_by = Some("MAX_SCANNED_DIRS");
                break 'walk;
            }
            let Some(scan) = scan_dir(&dir, anchors, &skip) else {
                continue;
            };
            // Depth 0 is the project root: already a probe root, never re-added.
            if depth > 0 && scan.anchored && !already.contains(&dir) && !found.contains(&dir) {
                if already.len() + found.len() > MAX_WORKSPACE_MEMBERS {
                    truncated_by = Some("MAX_WORKSPACE_MEMBERS");
                    break 'walk;
                }
                found.push(dir);
            }
            if depth < MAX_NESTED_DEPTH {
                next.extend(scan.children);
            } else if !scan.children.is_empty() {
                // #7781 round-3: the declared depth is this design's scope, not
                // a resource cap, so it never sets `truncated`.
                depth_limited = true;
            }
        }
        frontier = next;
    }
    finish(project_dir, found, truncated_by, depth_limited)
}

/// The one exit of [`nested_probe_roots`]: log what stopped it, then report it.
///
/// Why: #7781 round-3 review — the walk had three exits, and the two early ones
/// logged their own resource cap but skipped the depth DEBUG, so a walk that hit
/// BOTH bounds reported `depth_limited` in the returned flag and nowhere in the
/// log. One epilogue makes "flag set" and "line logged" impossible to drift
/// apart, whichever bound ended the walk.
/// What: one [`warn_truncated`] when a resource cap ended the walk, naming that
/// cap, and one [`debug_depth_limited`] when children were left unread at the
/// declared depth. Both, either, or neither fire; the returned flags mirror them
/// exactly.
/// Test: `both_bounds_can_trip_together`, `depth_limit_is_logged_at_debug`,
/// `scanned_dirs_bound_reports_truncation`.
fn finish(
    project_dir: &Path,
    roots: Vec<PathBuf>,
    truncated_by: Option<&'static str>,
    depth_limited: bool,
) -> NestedProbe {
    if let Some(bound) = truncated_by {
        warn_truncated(project_dir, bound);
    }
    if depth_limited {
        debug_depth_limited(project_dir);
    }
    NestedProbe {
        roots,
        truncated: truncated_by.is_some(),
        depth_limited,
    }
}

#[cfg(test)]
#[path = "nested_tests.rs"]
mod tests;
