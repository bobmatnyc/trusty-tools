//! Tests for the git-churn collector (#6079).
//!
//! Why: the collector is fail-open, so every arm that produces NOTHING has to be
//! pinned by a test that reads the gap it produced instead. A regression here
//! does not fail a build — it ships a report whose empty Change Hotspots section
//! reads as a codebase nobody is rewriting, which is the false clean claim epic
//! #6074 exists to remove. The two arms #6079 names explicitly are the ones
//! whose git output is INDISTINGUISHABLE from a quiet repository: an empty
//! history and a shallow clone both print nothing on stdout and exit zero.
//! What: a real fixture repository built in a tempdir with real commits (never a
//! checked-in `.git`), the reduction, the documented bands, every failure arm
//! through the [`Run`] seam, the manifest write-back, and the optional-input
//! contract on the ranking lane.
//! Test: this file.

use super::*;
use std::path::PathBuf;

/// A manifest with the one `[report]` table the write-back edits.
const MANIFEST: &str = "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"acme-api\"\npath = \"/tmp/acme-api\"\n";

/// The stderr a git with a pending upgrade notice emits first (#6720).
const NOISY_STDERR: &str = "warning: your git is out of date\nfatal: bad revision 'HEAD'\n";

/// A `--numstat` stream: three commits, two authors, one binary file, and one
/// generated file that must never reach an output.
const LOG: &str = "\u{1}alice@example.invalid\n\
                   \n\
                   10\t2\tsrc/api.rs\n\
                   4\t0\tsrc/db.rs\n\
                   -\t-\tassets/logo.png\n\
                   \u{1}bob@example.invalid\n\
                   \n\
                   7\t3\tsrc/api.rs\n\
                   120\t4\tCargo.lock\n\
                   \u{1}alice@example.invalid\n\
                   \n\
                   1\t1\tsrc/api.rs\n";

/// A stream in which one file clears the floor: [`MIN_COMMITS`] commits on
/// `src/api.rs`, so a partial read still produces a hotspot.
const HOT_LOG: &str = "\u{1}a@b.invalid\n\n1\t0\tsrc/api.rs\n\
                       \u{1}a@b.invalid\n\n1\t0\tsrc/api.rs\n\
                       \u{1}a@b.invalid\n\n1\t0\tsrc/api.rs\n\
                       \u{1}a@b.invalid\n\n1\t0\tsrc/api.rs\n\
                       \u{1}a@b.invalid\n\n1\t0\tsrc/api.rs\n";

/// A run that never happened: the seam every failure-arm test injects.
fn refuses(reason: &'static str) -> impl FnOnce(&Path) -> Result<Run, String> {
    move |_| Err(reason.to_string())
}

/// A run that completed, with the stream and disposition the test wants.
fn returns(
    success: bool,
    log: &'static str,
    stderr: &'static str,
) -> impl FnOnce(&Path) -> Result<Run, String> {
    move |_| {
        Ok(Run {
            success,
            log: log.to_string(),
            stderr: stderr.to_string(),
            shallow: false,
        })
    }
}

/// A run against a shallow clone, which prints history but a truncated one.
fn shallow() -> impl FnOnce(&Path) -> Result<Run, String> {
    move |_| {
        Ok(Run {
            success: true,
            log: LOG.to_string(),
            stderr: String::new(),
            shallow: true,
        })
    }
}

/// A checkout the leg applies to: a directory with a `.git` at its root, which
/// is the whole applicability ladder before any child is spawned.
fn checkout_at(tmp: &Path) -> PathBuf {
    let checkout = tmp.join("repos").join("acme-api");
    std::fs::create_dir_all(checkout.join(".git")).expect("mkdir checkout");
    checkout
}

/// A manifest file the write-back can edit.
fn manifest_at(tmp: &Path) -> PathBuf {
    let path = tmp.join("manifest.toml");
    std::fs::write(&path, MANIFEST).expect("write manifest");
    path
}

/// A file with `commits` commits and one author, for a band assertion.
fn touched(path: &str, commits: u32) -> ChurnFile {
    ChurnFile {
        path: path.to_owned(),
        commits,
        authors: 1,
        added: 1,
        deleted: 0,
    }
}

// ─── Reduction ──────────────────────────────────────────────────────────────

/// The per-commit stream becomes per-file counts, with distinct authors counted
/// once each — the three numbers #6079's row carries.
#[test]
fn the_log_reduces_to_per_file_counts() {
    let measured = parse(LOG);

    let api = measured
        .iter()
        .find(|f| f.path == "src/api.rs")
        .expect("src/api.rs is in the stream");
    assert_eq!(api.commits, 3, "three commits touched it");
    assert_eq!(api.authors, 2, "alice twice and bob once is two authors");
    assert_eq!((api.added, api.deleted), (18, 6));
}

/// A binary file has no line counts git can report, so it counts its commits
/// and zero lines rather than being dropped or counted as a parse failure.
#[test]
fn a_binary_file_counts_its_commits_and_no_lines() {
    let logo = parse(LOG)
        .into_iter()
        .find(|f| f.path == "assets/logo.png")
        .expect("the `-` columns are read, not skipped");

    assert_eq!((logo.commits, logo.added, logo.deleted), (1, 0, 0));
}

/// A lockfile changes on every dependency bump, so its churn measures the
/// resolver. It must not reach a row or the ranking lane.
#[test]
fn a_generated_file_never_becomes_a_hotspot() {
    assert!(
        !parse(LOG).iter().any(|f| f.path == "Cargo.lock"),
        "GENERATED is excluded at the reduction, before any band applies"
    );
    for name in GENERATED {
        assert!(
            is_generated(&format!("nested/dir/{name}")),
            "{name} is excluded by basename, at any depth"
        );
    }
}

/// Arbitrary text is ignored rather than counted, so a future git header or a
/// commit message that happens to contain tabs cannot invent a file.
#[test]
fn junk_is_ignored_rather_than_counted() {
    assert!(parse("").is_empty());
    assert!(parse("not a numstat line at all\n").is_empty());
    assert!(
        parse("\u{1}a@b\nx\ty\tsrc/a.rs\n").is_empty(),
        "non-numeric columns are not a stat line"
    );
    assert!(
        parse("\u{1}a@b\n1\t2\t3\tsrc/a.rs\n").is_empty(),
        "a fourth column is not the shape --numstat emits"
    );
}

// ─── Bands ──────────────────────────────────────────────────────────────────

/// The three bands are exactly the constants the module documents — the
/// deterministic-threshold closure condition, asserted against the constants
/// rather than against literals, so tuning one edit stays one edit.
#[test]
fn the_bands_are_the_documented_thresholds() {
    assert_eq!(touched("a", MIN_COMMITS - 1).severity(), None);
    assert_eq!(touched("a", MIN_COMMITS).severity(), Some(Severity::Amber));
    assert_eq!(
        touched("a", RED_COMMITS - 1).severity(),
        Some(Severity::Amber)
    );
    assert_eq!(touched("a", RED_COMMITS).severity(), Some(Severity::Red));
}

/// Ordering is total: commits, then total lines changed, then path. Two runs
/// over one history must produce byte-identical rows.
#[test]
fn hotspots_are_ranked_deterministically() {
    let ranked = hotspots(vec![
        ChurnFile {
            path: "z.rs".into(),
            commits: MIN_COMMITS,
            authors: 1,
            added: 5,
            deleted: 0,
        },
        ChurnFile {
            path: "a.rs".into(),
            commits: MIN_COMMITS,
            authors: 1,
            added: 5,
            deleted: 0,
        },
        ChurnFile {
            path: "big.rs".into(),
            commits: MIN_COMMITS,
            authors: 1,
            added: 900,
            deleted: 0,
        },
        touched("quiet.rs", MIN_COMMITS - 1),
    ]);

    let paths: Vec<&str> = ranked.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["big.rs", "a.rs", "z.rs"],
        "lines break a commit-count tie, then the path breaks that"
    );
    assert!(
        !paths.contains(&"quiet.rs"),
        "a file below MIN_COMMITS is not a hotspot"
    );
}

// ─── The fixture repository ─────────────────────────────────────────────────

/// Run `git -C <cwd> <args>`, failing the test with git's own message.
fn run_git_in(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git is on PATH for this suite");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository with real commits, built here rather than checked in.
///
/// `src/hot.rs` gets [`MIN_COMMITS`] + 2 commits by two authors, `src/warm.rs`
/// exactly [`MIN_COMMITS`], and `src/cold.rs` two — so the expected hotspot rows
/// are the first two, in that order, and the third proves the floor.
fn fixture_repo(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    run_git_in(root, &["init", "-q"]);
    run_git_in(root, &["config", "user.email", "alice@example.invalid"]);
    run_git_in(root, &["config", "user.name", "Alice"]);

    let commit = |file: &str, body: String, author: &str| {
        std::fs::write(root.join(file), body).expect("write the fixture file");
        run_git_in(root, &["add", "-A"]);
        run_git_in(
            root,
            &[
                "-c",
                &format!("user.email={author}"),
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
        );
    };

    for round in 0..(MIN_COMMITS + 2) {
        let author = if round % 2 == 0 {
            "alice@example.invalid"
        } else {
            "bob@example.invalid"
        };
        commit("src/hot.rs", format!("hot {round}\n"), author);
    }
    for round in 0..MIN_COMMITS {
        commit(
            "src/warm.rs",
            format!("warm {round}\n"),
            "alice@example.invalid",
        );
    }
    for round in 0..2 {
        commit(
            "src/cold.rs",
            format!("cold {round}\n"),
            "alice@example.invalid",
        );
    }
}

/// 🔴 The end-to-end proof: a real repository, a real `git log`, the expected
/// hotspot rows in the expected order — #6079's fixture closure condition.
#[test]
fn a_fixture_repository_ranks_its_hotspots() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("acme-api");
    fixture_repo(&checkout);

    let Outcome::Measured(files) = measure(&checkout) else {
        panic!("a repository with real commits measures");
    };

    let rows: Vec<(&str, u32, usize)> = files
        .iter()
        .map(|f| (f.path.as_str(), f.commits, f.authors))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("src/hot.rs", MIN_COMMITS + 2, 2),
            ("src/warm.rs", MIN_COMMITS, 1),
        ],
        "the two files over the floor, worst first; src/cold.rs is under it"
    );
    assert_eq!(files[0].severity(), Some(Severity::Amber));
}

/// A repository with no commit at all is a NAMED gap, never an empty clean
/// result — the arm whose git output is identical to a quiet repository's.
#[test]
fn a_repository_with_no_history_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = tmp.path().join("empty");
    std::fs::create_dir_all(&checkout).expect("mkdir");
    run_git_in(&checkout, &["init", "-q"]);

    let (files, gaps) = ground(&checkout, "acme-api");

    assert!(files.is_empty());
    assert_eq!(gaps.len(), 1, "one line, not a stream");
    assert!(
        gaps[0].contains(COLLECTOR)
            && gaps[0].contains("no history")
            && gaps[0].contains("unmeasured"),
        "the gap names the collector and refuses the clean reading: {}",
        gaps[0]
    );
}

/// A shallow clone's counts stop at the graft point, so they understate every
/// hotspot. That is a named gap rather than a smaller measurement.
#[test]
fn a_shallow_clone_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());

    let (files, gaps) = ground_with(&checkout, "acme-api", shallow());

    assert!(files.is_empty(), "truncated counts are not reported at all");
    assert_eq!(gaps.len(), 1);
    assert!(
        gaps[0].contains("shallow clone") && gaps[0].contains("understate"),
        "the gap says which state and why it is not a smaller number: {}",
        gaps[0]
    );
}

/// A path holding no repository has no change history a collector could have
/// missed, so it is a declared SKIP rather than a gap — the rule `cve` applies
/// to a checkout declaring no dependency manifest. Both rungs of the ladder are
/// driven, and neither spawns a child.
#[test]
fn a_path_holding_no_repository_is_a_declared_skip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let absent = tmp.path().join("nowhere");
    let plain = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&plain).expect("mkdir");

    assert!(matches!(
        measure_with(&absent, refuses("must not be reached")),
        Outcome::NotApplicable(ref why) if why.contains("is not a directory")
    ));
    assert!(matches!(
        measure_with(&plain, refuses("must not be reached")),
        Outcome::NotApplicable(ref why) if why.contains("holds no git repository")
    ));
    assert_eq!(
        ground_with(&plain, "acme-api", refuses("must not be reached")),
        (Vec::new(), Vec::new()),
        "nothing was missed, so there is nothing to name"
    );
}

// ─── Failure arms ───────────────────────────────────────────────────────────

/// A git that could not be run at all reaches the report as the caller's own
/// line, unchanged.
#[test]
fn a_run_that_never_happened_reports_its_own_reason() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let leaked = "git-churn: `git` is not installed, so no change history was read";

    let (_, gaps) = ground_with(&checkout, "acme-api", refuses(leaked));

    assert!(gaps[0].contains(leaked), "{}", gaps[0]);
}

/// 🔴 #6720: this module's own diagnosis leads, and the child's first stderr
/// line is only ever the parenthetical. A git whose stderr opens with an
/// unrelated upgrade notice must not have that notice reported as the cause.
#[test]
fn a_noisy_stderr_never_replaces_the_diagnosis() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());

    let (_, gaps) = ground_with(&checkout, "acme-api", returns(false, "", NOISY_STDERR));

    let gap = &gaps[0];
    let diagnosis = gap
        .find("exited non-zero and printed no history")
        .expect("the collector's own diagnosis is present");
    let notice = gap
        .find("your git is out of date")
        .expect("the stderr line rides along as the parenthetical");
    assert!(
        diagnosis < notice,
        "the diagnosis must lead the stderr line: {gap}"
    );
}

/// A non-zero exit that still printed history is read: `git log` can fail on a
/// later ref after streaming the commits that matter.
#[test]
fn a_partial_stream_is_read_rather_than_discarded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());

    let Outcome::Measured(files) = measure_with(&checkout, returns(false, HOT_LOG, NOISY_STDERR))
    else {
        panic!("a stream with commits in it is measured");
    };
    assert_eq!(files.len(), 1, "src/api.rs clears the floor");
    assert_eq!(files[0].commits, MIN_COMMITS);
}

/// A window that ran clean and holds no commit names the window, rather than
/// reporting the repository as having no hotspot.
#[test]
fn an_empty_window_names_the_window() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());

    let (files, gaps) = ground_with(&checkout, "acme-api", returns(true, "", ""));

    assert!(files.is_empty());
    assert!(
        gaps[0].contains(&format!("no commit in the last {WINDOW_DAYS} days")),
        "{}",
        gaps[0]
    );
}

// ─── The manifest write-back ────────────────────────────────────────────────

/// The hotspots reach `[report].findings` under the `churn` category, which is
/// the key trusty-review renders Change Hotspots from.
#[test]
fn the_hotspots_land_in_the_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = manifest_at(tmp.path());
    let files = vec![touched("src/api.rs", RED_COMMITS)];

    let gaps = write_into(&manifest, &files, "acme-api");

    assert!(gaps.is_empty(), "{gaps:?}");
    let written = std::fs::read_to_string(&manifest).expect("read back");
    assert!(written.contains("category = \"churn\""), "{written}");
    assert!(written.contains("id = \"churn-hotspot\""), "{written}");
    assert!(written.contains("package = \"src/api.rs\""), "{written}");
    assert!(written.contains("severity = \"RED\""), "{written}");
    assert!(
        written.contains("title = \"Acme\""),
        "the document's own keys survive the format-preserving edit"
    );
}

/// A resumed sweep re-measures the same repository and must not restate its
/// hotspots — including when the counts have moved on, because a file with one
/// more commit is the same finding rather than a second one.
#[test]
fn a_resumed_sweep_does_not_restate_a_hotspot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = manifest_at(tmp.path());

    let _ = write_into(&manifest, &[touched("src/api.rs", MIN_COMMITS)], "acme-api");
    let _ = write_into(&manifest, &[touched("src/api.rs", RED_COMMITS)], "acme-api");

    let written = std::fs::read_to_string(&manifest).expect("read back");
    assert_eq!(
        written.matches("package = \"src/api.rs\"").count(),
        1,
        "identity is id + package, not the counts: {written}"
    );
}

/// A manifest that cannot be written costs this leg's rows and says so, naming
/// how many findings the report is therefore missing.
#[test]
fn a_manifest_that_cannot_be_written_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("no").join("such").join("manifest.toml");

    let gaps = write_into(&missing, &[touched("src/api.rs", RED_COMMITS)], "acme-api");

    assert_eq!(gaps.len(), 1);
    assert!(
        gaps[0].contains("could not be read") && gaps[0].contains("1 change hotspot(s)"),
        "{}",
        gaps[0]
    );
}

/// Nothing measured writes nothing and opens no file — a quiet repository's
/// scope line is the caller's, not an empty table in the report.
#[test]
fn no_hotspots_writes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("absent.toml");

    assert!(write_into(&missing, &[], "acme-api").is_empty());
    assert!(!missing.exists(), "an empty write must not create the file");
}

/// A repository whose history is readable but quiet states what its count does
/// not cover, rather than saying nothing at all.
#[test]
fn a_quiet_repository_states_its_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let checkout = checkout_at(tmp.path());
    let quiet = "\u{1}a@b.invalid\n\n1\t0\tsrc/a.rs\n";

    let (files, gaps) = ground_with(&checkout, "acme-api", returns(true, quiet, ""));

    assert!(files.is_empty());
    assert_eq!(gaps.len(), 1);
    assert!(
        gaps[0].contains("no file was touched by") && gaps[0].contains("squash or a filter"),
        "the scope line names the floor and what the window misses: {}",
        gaps[0]
    );
}

// ─── The ranking lane ───────────────────────────────────────────────────────

/// The lane is the top hotspots, capped, carrying a reason and no dimension —
/// churn is evidence for no single due-diligence dimension.
#[test]
fn the_lane_is_the_top_hotspots_with_no_dimension() {
    let files: Vec<ChurnFile> = (0..(MAX_RANKED + 3))
        .map(|i| touched(&format!("src/f{i}.rs"), RED_COMMITS))
        .collect();

    let ranked = lane(&files);

    assert_eq!(ranked.len(), MAX_RANKED, "capped at MAX_RANKED");
    assert_eq!(ranked[0].path, "src/f0.rs");
    assert!(ranked[0].dimension.is_none(), "churn claims no dimension");
    assert!(
        ranked[0]
            .reason
            .as_deref()
            .is_some_and(|r| r.contains(COLLECTOR) && r.contains("commits by")),
        "the reason names the measurement: {:?}",
        ranked[0].reason
    );
    assert!(lane(&[]).is_empty(), "no churn, no lane");
}

/// 🔴 The live probe against this workspace ranked seven `Cargo.toml`s, a `.tsv`
/// allowlist and a `CLAUDE.md` in its top ten — an analyst's whole budget spent
/// on dependency bumps. The findings still report them; the ranking declines
/// them, and an extensionless file is presumed to be code.
#[test]
fn the_lane_declines_a_manifest_the_findings_still_report() {
    let files = vec![
        touched("Cargo.toml", RED_COMMITS + 9),
        touched("docs/CLAUDE.md", RED_COMMITS + 8),
        touched("scripts/deploy", RED_COMMITS + 7),
        touched("src/api.rs", RED_COMMITS),
    ];

    let ranked = lane(&files);
    let paths: Vec<&str> = ranked.iter().map(|p| p.path.as_str()).collect();

    assert_eq!(paths, vec!["scripts/deploy", "src/api.rs"]);

    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest = manifest_at(tmp.path());
    let gaps = write_into(&manifest, &files, "acme-api");

    assert!(gaps.is_empty(), "{gaps:?}");
    let written = std::fs::read_to_string(&manifest).expect("read back");
    assert!(
        written.contains("package = \"Cargo.toml\"")
            && written.contains("package = \"docs/CLAUDE.md\""),
        "the findings report the manifest and the prose the ranking declined: {written}"
    );
}
