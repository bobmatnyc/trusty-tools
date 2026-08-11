//! Smoke test for the `trusty-memory kg-rebuild` CLI subcommand wiring.
//!
//! Why (related to #124): PR #103's rebase silently dropped the
//! `pub mod kg_rebuild;` declaration in `commands/mod.rs` AND the
//! `KgRebuild { palace: Option<String> }` clap variant from `main.rs`,
//! leaving the source file orphaned and the subcommand unreachable. This
//! test invokes the real binary with `kg-rebuild --palace <id>` and asserts
//! the argument is accepted by clap — i.e. the subcommand is wired in.
//!
//! What: spawns `trusty-memory kg-rebuild --help` and `trusty-memory
//! kg-rebuild --palace test`. The first call must exit 0 (clap renders the
//! per-subcommand help). The second call may fail at runtime (no daemon
//! state, missing palace, etc.), but it must NOT fail with a clap parse
//! error — that would indicate the subcommand or `--palace` flag is missing.
//!
//! Test: `cargo test -p trusty-memory --test kg_rebuild_cli`. Requires Cargo
//! to have built the binary via `CARGO_BIN_EXE_trusty-memory`.

use std::path::PathBuf;
use std::process::Command;

/// Locate the binary Cargo built for this crate.
fn locate_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

#[test]
fn kg_rebuild_help_is_accepted() {
    let bin = locate_binary();
    let out = Command::new(&bin)
        .arg("kg-rebuild")
        .arg("--help")
        .output()
        .expect("spawn trusty-memory kg-rebuild --help");
    assert!(
        out.status.success(),
        "kg-rebuild --help must exit 0; got status {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--palace") || stdout.contains("-palace"),
        "kg-rebuild --help must advertise --palace flag, got:\n{stdout}"
    );
}

/// Why: #4678's purge is destructive against real palaces, so the two switches
/// that gate it have to be reachable and self-describing at the CLI.
/// What: `--help` advertises both `--purge-stale-subjects` and `--dry-run`.
/// Test: This test.
#[test]
fn kg_rebuild_help_advertises_purge_flags() {
    let bin = locate_binary();
    let out = Command::new(&bin)
        .arg("kg-rebuild")
        .arg("--help")
        .output()
        .expect("spawn trusty-memory kg-rebuild --help");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--purge-stale-subjects"),
        "kg-rebuild --help must advertise --purge-stale-subjects, got:\n{stdout}"
    );
    assert!(
        stdout.contains("--dry-run"),
        "kg-rebuild --help must advertise --dry-run, got:\n{stdout}"
    );
}

/// Why: `--dry-run` on its own would read as "rebuild without writing", which
/// this command does not offer. Letting it parse would hand an operator a
/// silent full rebuild when they asked for a preview.
/// What: `--dry-run` without `--purge-stale-subjects` is a clap parse error
/// (exit code 2).
/// Test: This test.
#[test]
fn kg_rebuild_dry_run_requires_the_purge_flag() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = locate_binary();
    let out = Command::new(&bin)
        .arg("kg-rebuild")
        .arg("--dry-run")
        .env("TRUSTY_DATA_DIR_OVERRIDE", tmp.path())
        .env("RUST_LOG", "warn")
        .output()
        .expect("spawn trusty-memory kg-rebuild --dry-run");
    assert_eq!(
        out.status.code(),
        Some(2),
        "--dry-run alone must be refused by clap; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Why: the safe preview path is the one an operator reaches for first against
/// live data, so it must actually run end to end rather than parse and die.
/// What: `--purge-stale-subjects --dry-run` against an empty data dir exits 0
/// and announces the dry run on stdout.
/// Test: This test.
#[test]
fn kg_rebuild_purge_dry_run_runs_clean() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = locate_binary();
    let out = Command::new(&bin)
        .arg("kg-rebuild")
        .arg("--purge-stale-subjects")
        .arg("--dry-run")
        .env("TRUSTY_DATA_DIR_OVERRIDE", tmp.path())
        .env("RUST_LOG", "warn")
        .output()
        .expect("spawn trusty-memory kg-rebuild purge dry run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "purge dry run must exit 0; status {:?}, stderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("DRY RUN"),
        "purge dry run must announce itself, got:\n{stdout}"
    );
}

#[test]
fn kg_rebuild_palace_arg_is_accepted_by_clap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = locate_binary();
    // Pin a tempdir so the command cannot touch the real data dir. We expect
    // the command to *run* (possibly with no palaces / no errors), or to
    // error out at the *runtime* layer — but it must not fail at the clap
    // parse layer. Clap parse errors have a specific exit code (2) and
    // emit "error:" on stderr, so we use the stderr-doesn't-contain-clap-
    // error heuristic to distinguish.
    let out = Command::new(&bin)
        .arg("kg-rebuild")
        .arg("--palace")
        .arg("nonexistent-palace-id")
        .env("TRUSTY_DATA_DIR_OVERRIDE", tmp.path())
        .env("RUST_LOG", "warn")
        .output()
        .expect("spawn trusty-memory kg-rebuild --palace");

    let stderr = String::from_utf8_lossy(&out.stderr);
    // The hallmark of a clap parse error is "error: unrecognized subcommand"
    // or "error: unexpected argument". Neither must appear in stderr.
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "kg-rebuild subcommand is missing from clap; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("unexpected argument"),
        "--palace argument is missing from kg-rebuild clap variant; stderr:\n{stderr}"
    );
    // Exit code 2 is clap's parse-error code. The handler itself returns 0
    // on success or non-zero (non-2) on real failures. We only refuse 2.
    assert_ne!(
        out.status.code(),
        Some(2),
        "exit code 2 indicates a clap parse error; stderr:\n{stderr}"
    );
}
