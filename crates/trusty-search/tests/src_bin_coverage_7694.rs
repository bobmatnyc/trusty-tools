//! `src/bin/**` is source, not build output (#7694).
//!
//! Why: `walker::SKIP_DIRS` is matched on basename at any depth, so `bin` —
//! listed for Java/Gradle build output — also pruned Cargo's `src/bin/`
//! convention. The whole `tm` CLI (298 tracked `.rs` files in this workspace)
//! was absent from every index, and an MCP `grep` over
//! `crates/trusty-mpm/src/bin/**` returned zero files.
//! What: four pins — a `src/bin/` fixture tree is walked, build-output
//! locations still prune, `.gitignore` still prunes, and the walker yields
//! every tracked `.rs` file in THIS repository that is not under a documented
//! exclusion.
//! Test: this file is the test.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_search::service::walker::{walk_source_files, WalkOptions};

/// Walk `root` and return every yielded file as a root-relative slash path.
fn walk_rel(root: &Path) -> BTreeSet<String> {
    let canonical = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    walk_source_files(root)
        .files
        .iter()
        .filter_map(|p| p.strip_prefix(&canonical).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect()
}

fn write_file(path: PathBuf, body: &str) {
    fs::create_dir_all(path.parent().expect("file has a parent")).expect("create dirs");
    fs::write(path, body).expect("write fixture file");
}

/// A Cargo `[[bin]]` tree under `src/` is source and must be indexed in full.
#[test]
fn src_bin_tree_is_walked() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    write_file(root.join("crates/x/src/lib.rs"), "pub fn lib_marker() {}");
    write_file(
        root.join("crates/x/src/bin/tool/main.rs"),
        "fn main() { println!(\"bin_marker\"); }",
    );
    write_file(
        root.join("crates/x/src/bin/tool/commands/run.rs"),
        "pub fn nested_marker() {}",
    );
    write_file(root.join("crates/x/src/bin/single.rs"), "fn main() {}");

    let walked = walk_rel(root);

    for expected in [
        "crates/x/src/lib.rs",
        "crates/x/src/bin/tool/main.rs",
        "crates/x/src/bin/tool/commands/run.rs",
        "crates/x/src/bin/single.rs",
    ] {
        assert!(
            walked.contains(expected),
            "#7694: `{expected}` must be indexed — walked set was {walked:?}"
        );
    }
}

/// A `bin`/`build`/`dist` directory that is NOT under a source root, and the
/// unconditional build directories, still prune.
#[test]
fn root_build_output_dirs_are_still_pruned() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    write_file(root.join("src/main.rs"), "fn main() {}");
    // Repo-root `bin/` — the Java/Gradle build-output shape SKIP_DIRS exists for.
    write_file(root.join("bin/generated.rs"), "// build output");
    // A build dir one level down, not under a source root.
    write_file(root.join("app/build/Generated.java"), "// build output");
    write_file(root.join("pkg/dist/bundle.js"), "// build output");
    // Unconditional entries keep pruning at any depth, source root or not.
    write_file(root.join("target/debug/gen.rs"), "// build output");
    write_file(root.join("src/node_modules/dep/index.js"), "// vendored");

    let walked = walk_rel(root);

    assert!(
        walked.contains("src/main.rs"),
        "source file must survive: {walked:?}"
    );
    for pruned in [
        "bin/generated.rs",
        "app/build/Generated.java",
        "pkg/dist/bundle.js",
        "target/debug/gen.rs",
        "src/node_modules/dep/index.js",
    ] {
        assert!(
            !walked.contains(pruned),
            "#7694 must not widen the walk: `{pruned}` is build output — {walked:?}"
        );
    }
}

/// `.gitignore` still prunes, including a `src/bin` tree the project ignores.
#[test]
fn gitignored_dir_is_still_pruned() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    fs::write(
        root.join(".gitignore"),
        "ignored_tree/\ncrates/x/src/bin/gen/\n",
    )
    .expect("write .gitignore");
    write_file(root.join("crates/x/src/bin/keep.rs"), "fn main() {}");
    write_file(root.join("crates/x/src/bin/gen/skip.rs"), "fn main() {}");
    write_file(root.join("ignored_tree/skip.rs"), "fn main() {}");

    let walked = walk_rel(root);

    assert!(
        walked.contains("crates/x/src/bin/keep.rs"),
        "un-ignored src/bin source must be indexed: {walked:?}"
    );
    for pruned in ["crates/x/src/bin/gen/skip.rs", "ignored_tree/skip.rs"] {
        assert!(
            !walked.contains(pruned),
            ".gitignore must still prune `{pruned}`: {walked:?}"
        );
    }
}

/// Path segments under which a tracked `.rs` file is expected to be absent from
/// the index. Spelled out here rather than read from `walker::SKIP_DIRS` so the
/// pin below states an independent expectation instead of restating the
/// production constant back to itself.
const EXPECTED_UNINDEXED_SEGMENTS: &[&str] = &[
    "fixtures",
    "__fixtures__",
    "testdata",
    "test-data",
    "test_data",
    "testresources",
    "test_resources",
];

/// Repository root, derived from this crate's manifest directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The "as good as ripgrep" pin: every tracked `.rs` file in THIS repository is
/// yielded by the walker unless it sits under a documented exclusion.
///
/// This walks the real checkout rather than a fixture, because the defect was
/// invisible in every fixture the suite had — `SKIP_DIRS` only bites on a tree
/// that actually uses Cargo's `src/bin/` convention. `git ls-files` at test
/// time is the reference set, so the assertion tracks the repository instead of
/// a hard-coded count. Skipped when the checkout has no git metadata (a
/// published `.crate` tarball), which is the only case where the reference set
/// cannot be built.
#[test]
fn walker_yields_every_tracked_rust_file_in_this_repo() {
    let root = repo_root();
    let canonical = fs::canonicalize(&root).expect("repo root must canonicalize");
    if !canonical.join(".git").exists() {
        eprintln!(
            "no .git in {} — skipping repo-parity pin",
            canonical.display()
        );
        return;
    }

    let out = Command::new("git")
        .args(["ls-files", "*.rs"])
        .current_dir(&canonical)
        .output()
        .expect("git ls-files must launch");
    assert!(
        out.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tracked: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .filter(|p| canonical.join(p).is_file())
        .collect();
    assert!(
        tracked.len() > 1_000,
        "expected a large tracked .rs set, got {}",
        tracked.len()
    );

    let walked = walk_rel(&canonical);

    let expected_unindexed = |p: &str| {
        p.split('/')
            .any(|seg| EXPECTED_UNINDEXED_SEGMENTS.contains(&seg))
    };

    let missing: Vec<&String> = tracked
        .iter()
        .filter(|p| !expected_unindexed(p) && !walked.contains(*p))
        .collect();

    assert!(
        missing.is_empty(),
        "#7694: {} tracked .rs file(s) the walker does not yield; first 20: {:?}",
        missing.len(),
        missing.iter().take(20).collect::<Vec<_>>()
    );

    // The specific cohort the defect hid. Named separately so a regression
    // reports the cause, not just a count.
    let src_bin: Vec<&String> = tracked.iter().filter(|p| p.contains("/src/bin/")).collect();
    assert!(
        src_bin.len() > 100,
        "expected this workspace to carry many src/bin sources, got {}",
        src_bin.len()
    );
    for p in &src_bin {
        assert!(walked.contains(*p), "#7694: src/bin source not walked: {p}");
    }
}

/// `WalkOptions::default()` is the configuration the pins above rely on.
#[test]
fn default_walk_options_respect_gitignore() {
    assert!(
        WalkOptions::default().respect_gitignore,
        "the repo-parity pin assumes .gitignore is honoured by default"
    );
}
