//! Canonical trusty-search index-id derivation from a project path.
//!
//! Why: trusty-search derives an index id from the current project (the
//! git-root basename, fallback to the cwd basename) when serving CLI queries,
//! but the MCP `serve` path never reached that logic — so trusty-mpm, which
//! injects a contextless `trusty-search serve` MCP stub, left index selection
//! to the LLM and routinely resolved the WRONG index (issue #1373). To
//! register-and-pin the correct project index, BOTH trusty-mpm (at session
//! launch) and trusty-search (in `detect_project`) must derive the *identical*
//! id from the same project root. Centralising the one rule here in
//! `trusty-common` — which both crates already depend on, and which avoids a
//! trusty-mpm → trusty-search dependency edge (trusty-search pulls the heavy
//! ONNX/usearch stack) — makes it the single source of truth so the two cannot
//! silently diverge.
//!
//! What: [`resolve_project_root`] walks up from a starting directory to the
//! nearest `.git` root (fallback: the start dir itself), and
//! [`derive_index_id`] turns a project root into its index id (the path
//! basename, preserved verbatim for backward-compatibility with already-indexed
//! projects). [`identifies_same_path`] answers "do these two paths name the same
//! directory tree?" for every registration guard that has to compare one.
//! [`refuse_unindexable_root`] answers the question that has to come FIRST —
//! may this root become an index at all? — because the basename rule cannot
//! tell a project root from `$HOME` (#6550). No global state; pure functions,
//! save for the one home-directory lookup.
//!
//! Test: `cargo test -p trusty-common --features unconditional-only --
//! index_id::tests` covers basename derivation, the git-root walk, the
//! no-marker fallback, same-tree identity across case variants, and the
//! unindexable-root refusals.

use std::path::{Path, PathBuf};

/// Walk up from `start` to the nearest directory containing a `.git` entry.
///
/// Why: a trusty-search index is keyed to a project root, and the canonical
/// project root is the git repository root. Both trusty-mpm (resolving the
/// session's project) and trusty-search (`detect_project`) must agree on which
/// directory is "the project root" so they derive the same index id (#1373).
/// What: returns the first ancestor of `start` (inclusive) that contains a
/// `.git` directory or file; when none is found, returns `start` itself
/// (a path with no enclosing git repo is still indexable by its own basename).
///
/// Known limitation: this returns the FIRST (innermost) ancestor with a `.git`,
/// so in a nested-repo / monorepo layout where a parent directory above the
/// intended project also has `.git`, the *inner* repo wins — and if the project
/// itself has no `.git` but a parent does, that parent `.git` would win. This
/// matches trusty-search's prior `detect_project` semantics (the two must agree
/// to derive the same index id), so it is intentional, not a bug; documented
/// here so a future monorepo-aware override is a conscious change, not a surprise.
/// Test: `resolve_project_root_finds_git_root` and
/// `resolve_project_root_falls_back_to_start` in `tests`.
pub fn resolve_project_root(start: &Path) -> PathBuf {
    find_git_root(start).unwrap_or_else(|| start.to_path_buf())
}

/// Walk up from `start` to the nearest `.git` root, returning `None` when no
/// enclosing git repository exists.
///
/// Why: some callers must DISTINGUISH "inside a git repo" from "no repo at all"
/// — [`resolve_project_root`] can't, because it collapses both cases to a
/// `PathBuf` (returning `start` itself on a miss). trusty-code's task-start
/// index hook uses this to cheaply short-circuit the bake-off/scratch case: a
/// throwaway directory with no `.git` has nothing worth registering with
/// trusty-search, so indexing is skipped entirely rather than creating an index
/// keyed to a directory that will be deleted moments later.
/// What: returns `Some(first_ancestor_with_.git)` (inclusive of `start`), or
/// `None` when the walk reaches the filesystem root without finding one. `.git`
/// is matched via `exists()` so both a normal clone (`.git` directory) and a git
/// worktree/submodule (`.git` file) resolve. This is the exact walk
/// [`resolve_project_root`] performs, exposed as an `Option` — the two share one
/// implementation so they can never disagree on which directory is the root.
/// Test: `find_git_root_some_when_repo`, `find_git_root_none_when_no_repo`.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        // `.git` is a directory in a normal clone and a file in a git worktree
        // / submodule; `exists()` matches both so worktrees resolve correctly.
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Derive the trusty-search index id for a project root.
///
/// Why: the index id is the stable handle every search/grep call targets. It
/// MUST be derived identically wherever it is computed (trusty-mpm's
/// register-and-pin at launch, trusty-search's `detect_project`) or a session
/// would create/pin one id while querying another (#1373).
/// What: returns the final path component of `project_root` as a `String`
/// (lossy on non-UTF-8). The basename is preserved verbatim — NOT slugified —
/// so the derived id byte-for-byte matches the ids trusty-search already
/// assigned to existing on-disk indexes (changing the casing/punctuation would
/// orphan every previously-indexed project). An empty / root path yields the
/// empty string; callers that need a non-empty id must guard that case.
/// Test: `derive_index_id_uses_basename` and `derive_index_id_empty_for_root`
/// in `tests`.
pub fn derive_index_id(project_root: &Path) -> String {
    project_root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Version tag mixed into every checkout-id digest preimage.
///
/// Why: a future change to this derivation must move every id wholesale into a
/// disjoint space rather than re-pointing an already-registered id at different
/// content — the same property [`crate::project_index_id`] mixes its own tag in
/// for. What: the literal `"checkout-v1"`. Test:
/// `derive_checkout_index_id_is_pinned_for_a_known_input`.
const CHECKOUT_SCHEME_VERSION: &str = "checkout-v1";

/// Hex digits of digest appended to a checkout index id.
///
/// 8 (32 bits) keeps the id readable in a log line and a URL path segment while
/// leaving collision probability negligible for the tens of checkouts one
/// machine holds. Uniqueness lives entirely here; the label is cosmetic.
const CHECKOUT_DIGEST_HEX: usize = 8;

/// Label used when a checkout's basename slugifies to nothing at all.
const CHECKOUT_FALLBACK_LABEL: &str = "checkout";

/// Derive a collision-resistant trusty-search index id for ONE checkout (#6149).
///
/// Why: [`derive_index_id`] is the bare basename, so two checkouts of one
/// repository — an audit engagement's clone and the operator's working tree —
/// collide on a single id. trusty-search's registry is one-`root_path`-per-id,
/// so the second checkout is silently served the FIRST one's content: the graded
/// self-audit of 2026-08-21 measured complexity against a tree it never audited.
/// The id is a cross-process contract that `trusty-audit`, `trusty-review` and
/// `tga` each derive independently, and this is the one implementation all three
/// call, so they cannot drift.
///
/// It is deliberately PURE — a function of the path and nothing else. No git
/// shell-out, no environment, no daemon. [`crate::derive_project_index_id`]
/// partitions on richer inputs, but two of them (`origin`, `operator`) are live
/// git state resolved per process, and three separate processes must agree on
/// this id; a component that varies with a process's `HOME` would reintroduce
/// exactly the divergence being fixed.
///
/// What: `"<slugified basename>-<8 hex>"` over the CANONICAL path, so a
/// symlinked or `..`-laden spelling of one tree derives one id. `None` for a
/// path with no final component, e.g. `/` — every caller already treats that as
/// "no index id could be derived". A path that cannot be canonicalised (it does
/// not exist yet) is normalised through [`Path::components`] instead, which
/// still collapses a trailing separator, a repeated separator and a `.`
/// component — `tga` anchors a `path = "."` config entry as `<base>/.`, and
/// that must not derive a second id for the tree `<base>` already names.
///
/// Known limitation: the id changes when the directory MOVES, because the path
/// is the whole partitioning input. That orphans the old index rather than
/// merging two trees — the strictly weaker of the two failures, and the same
/// tradeoff [`crate::project_index_id`] documents at length.
///
/// # Postconditions
/// Non-empty; a single URL path segment matching `[a-z0-9][a-z0-9-]*`; and a
/// pure function of the canonical path — identical input, identical id, in every
/// process and across daemon restarts.
///
/// Test: `derive_checkout_index_id_distinguishes_same_named_checkouts`,
/// `derive_checkout_index_id_is_stable_across_calls`,
/// `derive_checkout_index_id_is_a_single_url_safe_segment`,
/// `derive_checkout_index_id_is_pinned_for_a_known_input`.
#[must_use]
pub fn derive_checkout_index_id(checkout: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(checkout)
        .unwrap_or_else(|_| checkout.components().collect::<PathBuf>());
    let name = canonical.file_name()?.to_string_lossy().into_owned();
    let slug = crate::slug::slugify_string(&name);
    let label = if slug.is_empty() {
        CHECKOUT_FALLBACK_LABEL
    } else {
        slug.as_str()
    };
    let mut preimage: Vec<u8> = Vec::with_capacity(128);
    crate::project_index_id::push_field(&mut preimage, CHECKOUT_SCHEME_VERSION.as_bytes());
    crate::project_index_id::push_field(
        &mut preimage,
        &crate::project_index_id::path_bytes(&canonical),
    );
    let digest = format!("{:016x}", crate::project_index_id::fnv1a_64(&preimage));
    Some(format!("{label}-{}", &digest[..CHECKOUT_DIGEST_HEX]))
}

/// Decide whether `a` and `b` name the same on-disk directory tree.
///
/// Why: a trusty-search index identifies a searchable DIRECTORY TREE, so every
/// registration guard has to answer "is this the tree I already have?" — and on
/// macOS APFS (case-insensitive, case-preserving) the obvious answer is wrong.
/// `canonicalize` preserves the case each path was spelled with rather than
/// normalising it, so `/Users/bob/Duetto/CTO` and `/Users/bob/Duetto/cto`
/// canonicalize to two different strings over ONE inode and string equality
/// misses the match entirely. Two guards need this same answer — trusty-search's
/// `find_root_path_collision` (same tree, different id) and trusty-common's
/// `best_effort_create_index` (same id, different tree) — so per this
/// workspace's common-entry-point rule it is one implementation here, not two.
/// Filesystem-only by construction: no git remote, no repo identity, so a
/// non-git tree (trusty-agents indexes an OKF store this way) compares exactly
/// like a checkout.
/// What: compares `(dev, ino)` from `std::fs::metadata` when BOTH paths exist,
/// which also catches symlink aliases, bind mounts and hard-linked directories.
/// When either path cannot be stat'd — it was deleted, a volume was unmounted,
/// or the target is not unix — falls back to plain `Path` equality. That
/// fallback is deliberately the weaker pre-existing behaviour rather than a
/// refusal: a caller comparing against a root that has since vanished should
/// still get an answer, and the dominant case (both trees present) never
/// reaches it.
/// Test: `same_path_spelled_two_ways_is_the_same_tree` (macOS-gated),
/// `distinct_trees_are_not_the_same`, `missing_paths_fall_back_to_equality`.
pub fn identifies_same_path(a: &Path, b: &Path) -> bool {
    match same_filesystem_entry(a, b) {
        Some(same) => same,
        None => a == b,
    }
}

/// Why a resolved root must never become a trusty-search index (#6550).
///
/// Why: [`resolve_project_root`] falls back to the START path when no `.git`
/// ancestor exists and [`derive_index_id`] then takes its basename, so a
/// registration handed `/Users/masa` produced index `masa` — a well-formed id
/// naming the operator rather than any project. The derivation cannot detect
/// that itself: a basename looks equally plausible for a real project root and
/// for a directory that identifies no project at all. So the caller refuses
/// before it registers or pins one. trusty-mpm's `AutoInitRefusal` refuses
/// `git init` in the same two directories for the same reason.
/// What: the two directories that never name a project.
/// Test: `refuse_unindexable_root_refuses_the_home_directory`,
/// `refuse_unindexable_root_refuses_the_filesystem_root`,
/// `refuse_unindexable_root_permits_an_ordinary_project_root`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IndexRootRefusal {
    /// The root is the operator's home directory.
    HomeDirectory,
    /// The root is the filesystem root.
    FilesystemRoot,
}

impl std::fmt::Display for IndexRootRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::HomeDirectory => "the operator's home directory",
            Self::FilesystemRoot => "the filesystem root",
        })
    }
}

/// Should `root` be refused as a trusty-search index root (#6550)?
///
/// Why: see [`IndexRootRefusal`]. Two crates ask this question — trusty-common's
/// [`crate::search_index::ensure_project_indexed_reporting`] before it registers,
/// and trusty-search's `detect_project` before it derives — so per this
/// workspace's common-entry-point rule it is one implementation here.
/// What: resolves the home directory through `dirs::home_dir` and delegates to
/// [`refuse_unindexable_root_against`]. `None` means the root is indexable.
/// Test: `refuse_unindexable_root_refuses_the_filesystem_root`, plus the
/// caller-side `ensure_project_indexed_refuses_the_real_home_directory` and
/// `detect_project_refuses_the_real_home_directory`.
#[must_use]
pub fn refuse_unindexable_root(root: &Path) -> Option<IndexRootRefusal> {
    refuse_unindexable_root_against(root, dirs::home_dir().as_deref())
}

/// [`refuse_unindexable_root`] with `home` supplied by the caller.
///
/// Why: the process-wide home directory is the one input a test cannot vary
/// safely — `set_var("HOME")` races every other thread in the binary — so the
/// decision is a pure function of its two arguments and the wrapper above owns
/// the lookup. Same split as trusty-mpm's `plan_auto_init`.
/// What: `FilesystemRoot` when `root` has no parent (`/`, and the empty path);
/// `HomeDirectory` when `root` and `home` name one directory tree per
/// [`identifies_same_path`], which sees through a symlinked `$HOME` and through
/// APFS case-insensitivity. `None` otherwise. A `home` of `None` — a stripped
/// environment with no resolvable home — refuses nothing beyond the root, since
/// there is no directory to compare against.
/// Test: `refuse_unindexable_root_refuses_the_home_directory`,
/// `refuse_unindexable_root_refuses_the_filesystem_root`,
/// `refuse_unindexable_root_permits_an_ordinary_project_root`,
/// `refuse_unindexable_root_without_a_home_still_refuses_the_root`.
#[must_use]
pub fn refuse_unindexable_root_against(
    root: &Path,
    home: Option<&Path>,
) -> Option<IndexRootRefusal> {
    if root.parent().is_none() {
        return Some(IndexRootRefusal::FilesystemRoot);
    }
    if home.is_some_and(|home| identifies_same_path(root, home)) {
        return Some(IndexRootRefusal::HomeDirectory);
    }
    None
}

/// Compare `a` and `b` by `(dev, ino)`, or `None` when either cannot be stat'd.
///
/// Why/What/Test: see [`identifies_same_path`], the only caller.
#[cfg(unix)]
fn same_filesystem_entry(a: &Path, b: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let meta_a = std::fs::metadata(a).ok()?;
    let meta_b = std::fs::metadata(b).ok()?;
    Some(meta_a.dev() == meta_b.dev() && meta_a.ino() == meta_b.ino())
}

/// Non-unix targets have no wired-up `(dev, ino)` equivalent, so
/// [`identifies_same_path`] always falls back to path equality there.
#[cfg(not(unix))]
fn same_filesystem_entry(_a: &Path, _b: &Path) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("trusty-index-id-{tag}-{pid}-{nanos}"));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn derive_index_id_uses_basename() {
        assert_eq!(
            derive_index_id(Path::new("/Users/me/code/trusty-tools")),
            "trusty-tools"
        );
        // Casing and punctuation are preserved verbatim (NOT slugified) so the
        // id matches what trusty-search already stored for existing indexes.
        assert_eq!(
            derive_index_id(Path::new("/Users/me/code/MyProject")),
            "MyProject"
        );
        assert_eq!(
            derive_index_id(Path::new("/srv/Repo_With_Underscores")),
            "Repo_With_Underscores"
        );
    }

    #[test]
    fn derive_index_id_empty_for_root() {
        assert_eq!(derive_index_id(Path::new("/")), "");
    }

    /// #6149, the defect itself: the engagement clone and the operator's own
    /// checkout share a basename, and under [`derive_index_id`] they share an
    /// id — which is how a report measured a tree nobody audited. This assertion
    /// fails against the pre-fix derivation.
    #[test]
    fn derive_checkout_index_id_distinguishes_same_named_checkouts() {
        let engagement = Path::new("/w/dogfood-audit-final/repos/local/trusty-tools");
        let working = Path::new("/Users/masa/Projects/trusty-tools");

        let a = derive_checkout_index_id(engagement).expect("has a basename");
        let b = derive_checkout_index_id(working).expect("has a basename");

        assert_eq!(
            derive_index_id(engagement),
            derive_index_id(working),
            "the basename rule is what collides"
        );
        assert_ne!(a, b, "the checkout rule must not: {a} vs {b}");
        assert!(a.starts_with("trusty-tools-"), "still readable: {a}");
        assert!(b.starts_with("trusty-tools-"), "still readable: {b}");
    }

    /// The id is a cross-process contract, so it has to be the same value on
    /// every call, in every process — including for two spellings of one tree.
    #[test]
    fn derive_checkout_index_id_is_stable_across_calls() {
        let tmp = scratch_dir("checkout-stable");
        let real = tmp.join("repos/acme-api");
        fs::create_dir_all(&real).unwrap();

        let first = derive_checkout_index_id(&real).expect("id");
        assert_eq!(derive_checkout_index_id(&real), Some(first.clone()));
        assert_eq!(
            derive_checkout_index_id(&tmp.join("repos/../repos/acme-api")),
            Some(first.clone()),
            "a `..`-laden spelling names the same tree and must derive one id"
        );
        assert!(first.starts_with("acme-api-"), "{first}");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// A path that does not exist cannot be canonicalised, and the spellings
    /// that reach this function are the ones a config file wrote: a trailing
    /// separator, and the `<base>/.` a `path = "."` entry anchors to. All three
    /// name one tree, so all three must derive one id.
    #[test]
    fn derive_checkout_index_id_normalises_an_uncanonicalisable_path() {
        let plain = derive_checkout_index_id(Path::new("/w/repos/acme-api")).expect("id");
        for spelling in [
            "/w/repos/acme-api/",
            "/w/repos/acme-api/.",
            "/w//repos/acme-api",
        ] {
            assert_eq!(
                derive_checkout_index_id(Path::new(spelling)).as_deref(),
                Some(plain.as_str()),
                "{spelling}"
            );
        }
    }

    /// An index id is a URL path segment and a filename component; a basename
    /// that slugifies to nothing must still produce a well-formed id, and `/`
    /// must still say "no id", which is what every caller branches on.
    #[test]
    fn derive_checkout_index_id_is_a_single_url_safe_segment() {
        for path in [
            "/w/repos/My Project!",
            "/w/repos/Repo_With_Underscores",
            "/w/repos/…",
        ] {
            let id = derive_checkout_index_id(Path::new(path)).expect("id");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{path} → {id}"
            );
            assert!(!id.starts_with('-') && !id.ends_with('-'), "{path} → {id}");
        }
        assert!(
            derive_checkout_index_id(Path::new("/w/repos/…"))
                .expect("id")
                .starts_with("checkout-"),
            "a basename with no slug-able character falls back to a label"
        );
        assert_eq!(derive_checkout_index_id(Path::new("/")), None);
    }

    /// The digest rule is a wire format: changing it re-points every registered
    /// id at nothing. This pins one input so a silent change fails here.
    #[test]
    fn derive_checkout_index_id_is_pinned_for_a_known_input() {
        assert_eq!(
            derive_checkout_index_id(Path::new("/w/repos/acme-api")).as_deref(),
            Some("acme-api-05d116f5")
        );
    }

    #[test]
    fn resolve_project_root_finds_git_root() {
        let tmp = scratch_dir("git");
        fs::create_dir_all(tmp.join(".git")).unwrap();
        let nested = tmp.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();

        let root = resolve_project_root(&nested);
        assert_eq!(root, tmp);
        // And the derived id is the git-root basename, not the nested dir.
        assert_eq!(derive_index_id(&root), derive_index_id(&tmp));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_project_root_falls_back_to_start() {
        let tmp = scratch_dir("no-git");
        fs::create_dir_all(&tmp).unwrap();

        let root = resolve_project_root(&tmp);
        assert_eq!(root, tmp);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_git_root_some_when_repo() {
        let tmp = scratch_dir("fgr-git");
        fs::create_dir_all(tmp.join(".git")).unwrap();
        let nested = tmp.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_git_root(&nested), Some(tmp.clone()));

        let _ = fs::remove_dir_all(&tmp);
    }

    /// macOS APFS is case-insensitive but case-preserving, so `canonicalize`
    /// returns the spelling it was GIVEN — two cases of one directory produce
    /// two unequal strings over one inode. This is the exact pair that made a
    /// string-equality guard useless; `identifies_same_path` must see through it.
    #[cfg(target_os = "macos")]
    #[test]
    fn same_path_spelled_two_ways_is_the_same_tree() {
        let tmp = scratch_dir("Case-Variant");
        fs::create_dir_all(&tmp).unwrap();

        let name = tmp.file_name().unwrap().to_str().unwrap();
        let flipped: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        let variant = tmp.with_file_name(flipped);

        assert_ne!(variant, tmp, "the two spellings must differ as strings");
        assert!(
            identifies_same_path(&tmp, &variant),
            "{} and {} are one inode and must compare equal",
            tmp.display(),
            variant.display()
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Two genuinely different trees must not be conflated — the guard has to
    /// stay usable, not just safe.
    #[test]
    fn distinct_trees_are_not_the_same() {
        let a = scratch_dir("distinct-a");
        let b = scratch_dir("distinct-b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();

        assert!(!identifies_same_path(&a, &b));

        let _ = fs::remove_dir_all(&a);
        let _ = fs::remove_dir_all(&b);
    }

    /// When a path cannot be stat'd (deleted root, unmounted volume) there is no
    /// `(dev, ino)` to compare, so the answer degrades to path equality rather
    /// than to a refusal.
    #[test]
    fn missing_paths_fall_back_to_equality() {
        let gone = scratch_dir("never-created");
        let other = scratch_dir("also-never-created");

        assert!(identifies_same_path(&gone, &gone));
        assert!(!identifies_same_path(&gone, &other));
    }

    /// #6550, the defect itself: `/Users/masa` has no `.git`, so
    /// `resolve_project_root` returns it unchanged and `derive_index_id` names
    /// the index after the operator. This is the guard that stops it.
    #[test]
    fn refuse_unindexable_root_refuses_the_home_directory() {
        let home = scratch_dir("home");
        fs::create_dir_all(&home).unwrap();

        assert_eq!(
            derive_index_id(&home),
            home.file_name().unwrap().to_string_lossy(),
            "the basename rule is what produces the wrong id"
        );
        assert_eq!(
            refuse_unindexable_root_against(&home, Some(&home)),
            Some(IndexRootRefusal::HomeDirectory)
        );

        let _ = fs::remove_dir_all(&home);
    }

    /// A path with no final component derives the empty id, which callers
    /// already treated as "no index" — the refusal states the reason instead of
    /// leaving each caller to infer it from an empty string.
    #[test]
    fn refuse_unindexable_root_refuses_the_filesystem_root() {
        assert_eq!(
            refuse_unindexable_root_against(Path::new("/"), None),
            Some(IndexRootRefusal::FilesystemRoot)
        );
        assert_eq!(
            refuse_unindexable_root(Path::new("/")),
            Some(IndexRootRefusal::FilesystemRoot)
        );
    }

    /// The guard has to stay usable: an ordinary checkout under the home
    /// directory is refused nowhere.
    #[test]
    fn refuse_unindexable_root_permits_an_ordinary_project_root() {
        let home = scratch_dir("home-with-project");
        let project = home.join("code/acme-api");
        fs::create_dir_all(&project).unwrap();

        assert_eq!(refuse_unindexable_root_against(&project, Some(&home)), None);
        assert_eq!(refuse_unindexable_root(&project), None);

        let _ = fs::remove_dir_all(&home);
    }

    /// A stripped environment resolves no home directory. That must not turn
    /// the guard off for the filesystem root, and must not refuse a real root.
    #[test]
    fn refuse_unindexable_root_without_a_home_still_refuses_the_root() {
        assert_eq!(
            refuse_unindexable_root_against(Path::new(""), None),
            Some(IndexRootRefusal::FilesystemRoot)
        );
        assert_eq!(
            refuse_unindexable_root_against(Path::new("/w/repos/acme-api"), None),
            None
        );
    }

    #[test]
    fn find_git_root_none_when_no_repo() {
        // A scratch dir with no `.git` anywhere up the chain: the tcode
        // short-circuit relies on this returning None so nothing is indexed.
        let tmp = scratch_dir("fgr-no-git");
        fs::create_dir_all(&tmp).unwrap();

        assert_eq!(find_git_root(&tmp), None);

        let _ = fs::remove_dir_all(&tmp);
    }
}
