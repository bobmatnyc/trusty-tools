//! End-to-end tests for `import kuzu --dry-run --export-refusals` (#8624).
//!
//! The umask test runs its scenario in a child copy of this test binary,
//! because `umask(2)` is process-global and would race every other test's
//! creates.

use std::cell::RefCell;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::super::tests::{
    FakeDaemon, FakeRunner, args_for, env, fixture, memory_row, store_on_disk,
};
use super::super::{ImportSource, KuzuImportError, run_import_in};
use super::export::{DIR_MODE, FILE_MODE, MASKED_FILE, OsFs, ReviewFs, write_run};
use super::export_tests::{leaked_windows, mixed_token, provider_token, sample_record, tree};
use super::{ReviewError, prepare};

/// Names the child scenario's scratch directory; set only by the parent.
const CHILD_ENV: &str = "TRUSTY_MEMORY_8624_UMASK_CHILD";
const CHILD_TEST: &str = "commands::kuzu_import::review::tests::umask_000_child_scenario";

/// A secret-shaped `Memory.id`, distinct from the two content tokens.
fn id_token() -> String {
    format!("{}{}", "Pk3Jw9Rv2Hx7", "Bm5Nc8Qd4Lt6Fy1Z")
}

/// A store with one clean, one exportable, one auto-refused content row and
/// one row whose id is secret-shaped.
fn secret_fixture() -> serde_json::Value {
    let mut v = fixture();
    v["memories"] = json!([
        memory_row(
            "m-1",
            "synthetic note about the widget factory layout",
            "h1"
        ),
        memory_row(
            "m-2",
            &format!("deploy note: key {} rotated", mixed_token()),
            "h2"
        ),
        memory_row(
            "m-3",
            &format!("ci token {} in env", provider_token()),
            "h3"
        ),
        memory_row(&id_token(), "an ordinary synthetic note", "h4"),
    ]);
    v
}

/// [`OsFs`] that records each new entry's mode the moment its create returns.
#[derive(Default)]
struct RecordingFs {
    seen: RefCell<Vec<(PathBuf, u32, u32)>>,
}

impl RecordingFs {
    fn record(&self, path: &Path, want: u32) -> io::Result<()> {
        let got = OsFs.mode(path)?;
        self.seen.borrow_mut().push((path.to_path_buf(), got, want));
        Ok(())
    }
}

impl ReviewFs for RecordingFs {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        OsFs.create_dir(path)?;
        self.record(path, DIR_MODE)
    }
    fn create_file(&self, path: &Path) -> io::Result<File> {
        let f = OsFs.create_file(path)?;
        self.record(path, FILE_MODE)?;
        Ok(f)
    }
    fn mode(&self, path: &Path) -> io::Result<u32> {
        OsFs.mode(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        OsFs.rename(from, to)
    }
}

/// The child half of `umask_000_end_to_end_export_is_owner_only_and_leaks_nothing`.
///
/// Why: runs only when that test spawns it with [`CHILD_ENV`] set; in a
/// normal run it returns at once. Under umask 000 a create that leaves its
/// mode to a later chmod is caught at the moment of creation.
/// Test: `umask_000_end_to_end_export_is_owner_only_and_leaks_nothing`.
#[tokio::test(flavor = "current_thread")]
async fn umask_000_child_scenario() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let tmp = PathBuf::from(dir);
    // SAFETY: this child process runs this one test on one thread.
    unsafe { libc::umask(0) };
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .try_init();

    let rec = RecordingFs::default();
    let rec_dir = tmp.join("rec/kuzu-review/run");
    write_run(&rec, &rec_dir, &[sample_record()], &|_| Ok(())).expect("recorded run");
    let seen = rec.seen.take();
    assert_eq!(seen.len(), 4, "three directories and the staging file");
    for (path, got, want) in seen.iter() {
        assert_eq!(got, want, "{} at creation: {got:o}", path.display());
    }

    let store = store_on_disk(&tmp, "proj");
    let runner = FakeRunner::writing(&secret_fixture());
    let daemon = FakeDaemon(None);
    let data_root = tmp.join("data");
    let mut args = args_for(&store, "kuzu-review-e2e", true);
    args.export_refusals = true;
    let root = tmp.join("home/.trusty-tools/kuzu-review");
    run_import_in(
        &args,
        &[store],
        &env(&runner, &daemon, &data_root),
        Some(&root),
    )
    .await
    .expect("dry run with export");

    let made = tree(&tmp.join("home"));
    let mut files = 0;
    for p in &made {
        let mode = OsFs.mode(p).expect("mode");
        if p.is_dir() {
            assert_eq!(mode, DIR_MODE, "{}", p.display());
        } else {
            assert_eq!(mode, FILE_MODE, "{}", p.display());
            assert_eq!(p.file_name().and_then(|n| n.to_str()), Some(MASKED_FILE));
            files += 1;
        }
    }
    assert_eq!(files, 1, "one masked.jsonl and no staging file");
    let run_dir = std::fs::read_dir(&root)
        .expect("root")
        .flatten()
        .next()
        .expect("run");
    let body = std::fs::read_to_string(run_dir.path().join(MASKED_FILE)).expect("read");
    let lines: Vec<serde_json::Value> = body
        .lines()
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect();
    assert_eq!(lines.len(), 1, "only the exportable row");
    assert_eq!(lines[0]["id"], "m-2");
    assert_eq!(lines[0]["spans"][0]["rule"], "mixed_case_alnum");
    assert!(
        lines[0]["masked"]
            .as_str()
            .is_some_and(|m| m.starts_with("deploy note: key ⟦S0:mixed_case_alnum:")),
        "{body}"
    );
}

/// Prefixes of the random-named temp directories the code under test makes.
const RANDOM_NAME_PREFIXES: [&str; 2] = ["trusty-kuzu-import-", "trusty-kuzu-dry-run-"];

/// `seen` with the uncontrolled random characters of temp-dir names removed.
///
/// Why (#10549): a 4-char window of a token matched the random suffix of a
/// temp-dir name (`Pe2R`) in a path the run printed. That is chance, not a
/// leak, and a strict window scan cannot tell them apart.
/// What: replaces each `roots` path with `<root>`, then drops the alphanumeric
/// run after each [`RANDOM_NAME_PREFIXES`] entry. Everything else the run
/// produced stays in the scan, so a real leak is still caught.
/// Test: `scrub_removes_random_names_but_keeps_real_content`.
fn scrub_random_names(seen: &str, roots: &[&Path]) -> String {
    let mut out = seen.to_string();
    for r in roots {
        let canon = std::fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf());
        for form in [r.to_string_lossy(), canon.to_string_lossy()] {
            out = out.replace(form.as_ref(), "<root>");
        }
    }
    for prefix in RANDOM_NAME_PREFIXES {
        let mut kept = String::new();
        let mut rest = out.as_str();
        while let Some(at) = rest.find(prefix) {
            let end = at + prefix.len();
            kept.push_str(&rest[..end]);
            rest = rest[end..].trim_start_matches(|c: char| c.is_ascii_alphanumeric());
        }
        kept.push_str(rest);
        out = kept;
    }
    out
}

/// Why (#10549): the scrub must drop random names yet keep any token text.
/// Test: itself.
#[test]
fn scrub_removes_random_names_but_keeps_real_content() {
    let tmp = Path::new("/tmp/.tmpPe2Rxy");
    let seen = format!(
        "run directory: {}/home/x\n/var/tmp/trusty-kuzu-import-Pe2Rxy/out.json\n\
         /var/tmp/trusty-kuzu-dry-run-Xq7WAb/db",
        tmp.display()
    );
    let scrubbed = scrub_random_names(&seen, &[tmp]);
    for tok in [mixed_token(), provider_token()] {
        assert!(!leaked_windows(&tok, &seen).is_empty(), "raw scan trips");
        assert!(leaked_windows(&tok, &scrubbed).is_empty(), "{scrubbed}");
    }
    let real = scrub_random_names(&format!("{seen} leak {}", mixed_token()), &[tmp]);
    assert!(!leaked_windows(&mixed_token(), &real).is_empty());
}

/// Why (#8624 rules 1, 3, 5, 6, 9): under umask 000 the run directory comes
/// out 0700 and `masked.jsonl` 0600 at creation; only the exportable row is
/// written; and no 4-char window of any token reaches a byte under the run
/// directory, stdout, stderr or a TRACE-level log.
/// Test: itself, through `umask_000_child_scenario`.
#[test]
fn umask_000_end_to_end_export_is_owner_only_and_leaks_nothing() {
    // #10549: a root named with token windows makes the unscrubbed scan fail
    // every run, so the scrub is exercised, not left to chance.
    let (m, p) = (mixed_token(), provider_token());
    let tmp = tempfile::Builder::new()
        .prefix(&format!("w-{}-{}-", &m[..4], &p[4..8]))
        .tempdir()
        .expect("tmp");
    let exe = std::env::current_exe().expect("test binary");
    let out = std::process::Command::new(exe)
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, tmp.path())
        .output()
        .expect("spawn child");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "child failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("1 passed"),
        "the child scenario ran:\n{stdout}"
    );
    assert!(
        stdout.contains(
            "review export: 1 row(s) exported masked, 2 auto-refused \
             (provider_prefix 1, secret_shaped_id 1)"
        ),
        "{stdout}"
    );
    assert!(stdout.contains("run directory: "), "{stdout}");
    let mut seen = stdout + &stderr;
    let written = [tmp.path().join("home"), tmp.path().join("rec")];
    for p in written.iter().flat_map(|d| tree(d)) {
        if p.is_file() {
            seen.push_str(&String::from_utf8_lossy(&std::fs::read(&p).expect("read")));
        }
    }
    // #10549: temp-dir names are random; scrub them before the window scan.
    let seen = scrub_random_names(&seen, &[tmp.path()]);
    for tok in [mixed_token(), provider_token(), id_token()] {
        assert_eq!(leaked_windows(&tok, &seen), Vec::<String>::new());
    }
}

/// Why (#8624 rule 1): clap refuses `--export-refusals` without `--dry-run`.
/// Test: itself.
#[test]
fn export_refusals_requires_dry_run_at_parse() {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        source: ImportSource,
    }
    let parse = |args: &[&str]| Cli::try_parse_from(["t"].iter().chain(args.iter()).copied());
    let err = parse(&["kuzu", "--from", "x", "--export-refusals"])
        .err()
        .expect("needs --dry-run");
    assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    let ImportSource::Kuzu(a) = parse(&["kuzu", "--from", "x", "--dry-run", "--export-refusals"])
        .expect("parse")
        .source;
    assert!(a.export_refusals && a.dry_run);
    let ImportSource::Kuzu(a) = parse(&["kuzu", "--from", "x"]).expect("parse").source;
    assert!(!a.export_refusals);
}

/// Why (#8624 rule 1): a caller that skips clap still cannot export from a
/// writing run; it is refused before any store is read.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_refusals_needs_dry_run_even_without_clap() {
    let tmp = tempfile::tempdir().expect("tmp");
    assert!(matches!(
        prepare(true, false, Some(tmp.path()), tmp.path()),
        Err(ReviewError::NeedsDryRun)
    ));
    let store = store_on_disk(tmp.path(), "proj");
    let runner = FakeRunner::writing(&secret_fixture());
    let daemon = FakeDaemon(None);
    let data_root = tmp.path().join("data");
    let mut args = args_for(&store, "kuzu-review-write", false);
    args.export_refusals = true;
    let root = tmp.path().join("home/kuzu-review");
    let err = run_import_in(
        &args,
        &[store],
        &env(&runner, &daemon, &data_root),
        Some(&root),
    )
    .await
    .expect_err("refused");
    assert!(
        matches!(err, KuzuImportError::Io(ref m) if m.contains("--dry-run")),
        "{err}"
    );
    assert!(runner.temp_dir().is_none(), "no store was exported");
    assert!(!data_root.exists() && !root.exists());
}

/// Why (#8624 rules 4, 7): a destination the guard refuses stops the run
/// before any store is read, and nothing is created.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guard_error_refuses_the_export_before_any_store_is_read() {
    let tmp = tempfile::tempdir().expect("tmp");
    let store = store_on_disk(tmp.path(), "proj");
    let runner = FakeRunner::writing(&secret_fixture());
    let daemon = FakeDaemon(None);
    let data_root = tmp.path().join("data");
    let mut args = args_for(&store, "kuzu-review-guard", true);
    args.export_refusals = true;
    let root = data_root.join("kuzu-review");
    let err = run_import_in(
        &args,
        &[store],
        &env(&runner, &daemon, &data_root),
        Some(&root),
    )
    .await
    .expect_err("refused");
    assert!(
        matches!(err, KuzuImportError::Io(ref m) if m.contains("palace data root")),
        "{err}"
    );
    assert!(runner.temp_dir().is_none(), "no store was exported");
    assert!(!data_root.exists());
    for tok in [mixed_token(), provider_token()] {
        assert!(leaked_windows(&tok, &err.to_string()).is_empty());
    }
}

/// Why (#8624 rule 9): without the flag a dry run writes no review export.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_flag_nothing_is_exported() {
    let tmp = tempfile::tempdir().expect("tmp");
    let store = store_on_disk(tmp.path(), "proj");
    let runner = FakeRunner::writing(&secret_fixture());
    let daemon = FakeDaemon(None);
    let data_root = tmp.path().join("data");
    let args = args_for(&store, "kuzu-review-off", true);
    let root = tmp.path().join("home/kuzu-review");
    let reports = run_import_in(
        &args,
        &[store],
        &env(&runner, &daemon, &data_root),
        Some(&root),
    )
    .await
    .expect("dry run");
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].counts.refused_ids.len(),
        3,
        "the screen still refuses"
    );
    assert!(!root.exists());
}
