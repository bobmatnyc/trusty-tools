//! #12028 P1e: one plain run of each shipped state-writer binary.
//!
//! The replay recordings run the library through `examples/replay_port.rs`;
//! these prove the shipped `[[bin]]` targets find their files under the
//! working directory (the checkout, marked by `ARCHITECT.md`), write them,
//! and fail loudly when a write fails.

use std::path::Path;
use std::process::{Command, Output};

fn run(bin: &str, cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env_remove("ALIASES_FILE")
        .output()
        .expect("the binary runs")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn shipped_state_archive_moves_sections_in_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ARCHITECT.md"), "").unwrap(); // #12073: the checkout
    std::fs::create_dir(dir.path().join("local")).unwrap();
    let state = dir.path().join("local/state.md");
    std::fs::write(&state, "## handoff 1\na\n## handoff 2\nb\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_trusty-architect-state-archive");
    let out = run(bin, dir.path(), &["--keep", "1"]);
    // The binary names the archive under the working directory, symlinks resolved.
    let archive = dir
        .path()
        .canonicalize()
        .unwrap()
        .join("local/state-archive.md");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!("moved 1 sections to {}\n", archive.display())
    );
    assert_eq!(
        std::fs::read_to_string(&state).unwrap(),
        "## handoff 1\na\n"
    );
    assert!(
        std::fs::read_to_string(&archive)
            .unwrap()
            .ends_with("## handoff 2\nb\n")
    );

    // A failed archive write exits 1 and leaves state.md as it was.
    std::fs::write(&state, "## handoff 1\na\n## handoff 2\nb\n").unwrap();
    std::fs::remove_file(&archive).unwrap();
    std::fs::create_dir(&archive).unwrap();
    let out = run(bin, dir.path(), &["--keep", "1"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with("IsADirectoryError: [Errno 21]"),
        "{}",
        text(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert_eq!(
        std::fs::read_to_string(&state).unwrap(),
        "## handoff 1\na\n## handoff 2\nb\n"
    );
}

#[test]
fn shipped_aliases_add_writes_the_table_atomically() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ARCHITECT.md"), "").unwrap(); // #12073: the checkout
    std::fs::create_dir(dir.path().join("local")).unwrap();
    let bin = env!("CARGO_BIN_EXE_trusty-architect-aliases");
    let out = run(
        bin,
        dir.path(),
        &["add", "host", "box-one", "--alias", "b1"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "host box-one: 1 aliases\n");
    let table = dir.path().join("local/aliases.json");
    let written = std::fs::read_to_string(&table).unwrap();
    assert!(
        written.starts_with("{\n  \"version\": 1,\n  \"hosts\": [\n"),
        "{written}"
    );
    assert!(!dir.path().join("local/aliases.json.tmp").exists());
    let out = run(bin, dir.path(), &["resolve", "is b1 up"]);
    assert_eq!(text(&out.stdout), "NAMES: b1 = box-one\n");

    // The tmp path is a directory: exit 1, nothing reported written, table unchanged.
    std::fs::create_dir(dir.path().join("local/aliases.json.tmp")).unwrap();
    let out = run(bin, dir.path(), &["add", "host", "box-two"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(std::fs::read_to_string(&table).unwrap(), written);
}

#[test]
fn shipped_brief_id_assigns_in_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ARCHITECT.md"), "").unwrap(); // #12073: the checkout
    std::fs::create_dir_all(dir.path().join("local/briefs")).unwrap();
    let brief = dir.path().join("local/briefs/plan.md");
    std::fs::write(&brief, "# Plan\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_trusty-architect-brief-id");
    let out = run(bin, dir.path(), &["assign", "local/briefs/plan.md"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "MB0000\tassigned\tlocal/briefs/plan.md\n"
    );
    assert_eq!(
        std::fs::read_to_string(&brief).unwrap(),
        "<!-- brief-id: MB0000 -->\n# MB0000: Plan\n"
    );
    let registry = std::fs::read_to_string(dir.path().join("local/briefs/registry.jsonl")).unwrap();
    assert!(registry.starts_with(
        "{\"id\": \"MB0000\", \"path\": \"local/briefs/plan.md\", \"title\": \"Plan\""
    ));
    let out = run(bin, dir.path(), &["check"]);
    assert_eq!(
        (out.status.code(), text(&out.stdout)),
        (Some(0), "0 problem(s)\n".to_owned())
    );
    let out = run(bin, dir.path(), &["next"]);
    assert_eq!(text(&out.stdout), "MB0001\n");
}

// #12073: the binaries find the Architect checkout (the nearest directory at
// or above the working directory holding `ARCHITECT.md`), as the script finds
// it beside itself; a working directory outside every checkout is refused.

/// A checkout with `ARCHITECT.md`, its `local/`, and a `sub/` directory in it.
fn checkout() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ARCHITECT.md"), "# Architect\n").unwrap();
    std::fs::create_dir_all(dir.path().join("local/briefs")).unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    dir
}

fn assert_refused(out: &Output, foreign: &Path) {
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("no Architect checkout (ARCHITECT.md)"),
        "{}",
        text(&out.stderr)
    );
    assert!(!foreign.join("local").exists(), "a stray local/ was made");
}

#[test]
fn shipped_brief_id_finds_the_checkout_from_a_foreign_directory() {
    let co = checkout();
    let row = "{\"id\": \"MB0000\", \"path\": \"local/briefs/a.md\", \"title\": \"A\"}\n";
    std::fs::write(co.path().join("local/briefs/registry.jsonl"), row).unwrap();
    let bin = env!("CARGO_BIN_EXE_trusty-architect-brief-id");
    let out = run(bin, &co.path().join("sub"), &["next"]);
    assert_eq!(
        (out.status.code(), text(&out.stdout)),
        (Some(0), "MB0001\n".to_owned()),
        "{}",
        text(&out.stderr)
    );
    let out = run(bin, &co.path().join("sub"), &["find", "MB0000"]);
    let repo = co.path().canonicalize().unwrap();
    assert_eq!(
        text(&out.stdout),
        format!("{}/local/briefs/a.md\n", repo.display())
    );

    let foreign = tempfile::tempdir().unwrap();
    std::fs::write(foreign.path().join("plan.md"), "# Plan\n").unwrap();
    assert_refused(&run(bin, foreign.path(), &["next"]), foreign.path());
    let out = run(bin, foreign.path(), &["assign", "plan.md"]);
    assert_refused(&out, foreign.path());
    assert_eq!(
        std::fs::read_to_string(foreign.path().join("plan.md")).unwrap(),
        "# Plan\n"
    );
}

#[test]
fn shipped_state_archive_finds_the_checkout_from_a_foreign_directory() {
    let co = checkout();
    let state = co.path().join("local/state.md");
    std::fs::write(&state, "## handoff 1\na\n## handoff 2\nb\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_trusty-architect-state-archive");
    let out = run(bin, &co.path().join("sub"), &["--keep", "1"]);
    let archive = co
        .path()
        .canonicalize()
        .unwrap()
        .join("local/state-archive.md");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!("moved 1 sections to {}\n", archive.display())
    );
    assert_eq!(
        std::fs::read_to_string(&state).unwrap(),
        "## handoff 1\na\n"
    );

    let foreign = tempfile::tempdir().unwrap();
    assert_refused(&run(bin, foreign.path(), &["--keep", "1"]), foreign.path());
    // Both paths given: no default is needed, so no checkout either.
    let (s, a) = (state.to_str().unwrap(), archive.to_str().unwrap());
    let out = run(
        bin,
        foreign.path(),
        &["--state", s, "--archive", a, "--dry-run"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

#[test]
fn shipped_aliases_finds_the_checkout_from_a_foreign_directory() {
    let co = checkout();
    let bin = env!("CARGO_BIN_EXE_trusty-architect-aliases");
    let out = run(
        bin,
        &co.path().join("sub"),
        &["add", "host", "box-one", "--alias", "b1"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(co.path().join("local/aliases.json").is_file());
    assert!(!co.path().join("sub/local").exists());

    let foreign = tempfile::tempdir().unwrap();
    let out = run(bin, foreign.path(), &["add", "host", "box-two"]);
    assert_refused(&out, foreign.path());
    // `ALIASES_FILE` names the table: no checkout is needed.
    let table = co.path().join("local/aliases.json");
    let out = Command::new(bin)
        .args(["resolve", "is b1 up"])
        .current_dir(foreign.path())
        .env("ALIASES_FILE", &table)
        .output()
        .expect("the binary runs");
    assert_eq!(text(&out.stdout), "NAMES: b1 = box-one\n");
}
