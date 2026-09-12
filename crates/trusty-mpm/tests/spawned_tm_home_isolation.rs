//! A spawned `tm` never reaches the operator's `$HOME` (#7568).
//!
//! Why: an integration target that runs the built binary hands the CHILD an
//! environment, and a child that inherits `$HOME` resolves the operator's own
//! `~/.trusty-mpm`. Measured on `origin/main` by running each spawn-site target
//! under a scratch `$HOME`: nine of them created `<HOME>/.trusty-mpm/`, and
//! `tm_compress_pipe` additionally appended to `compression.jsonl` — the
//! savings ledger #7514 fixed for the unit-test path. Redirecting `$HOME`
//! inside the test PROCESS is not the answer either: that is the #5544 hazard,
//! a process-global write visible to every parallel sibling in the same binary.
//! The child's own environment block is the injection point.
//!
//! What: three live tests and two source rules.
//!   - [`an_unisolated_spawn_writes_into_whatever_home_it_inherits`] pins the
//!     hazard — the pre-fix shape, proving the marker file this file asserts on
//!     is a real signal rather than something that appears unconditionally.
//!   - [`a_spawned_tm_resolves_the_helper_home_and_leaves_the_operators_alone`]
//!     is the regression guard: the same spawn through `common::isolate_spawned_tm`
//!     writes under the confined home, writes nothing under the home it would
//!     have inherited, and leaves the operator's framework root untouched.
//!   - [`the_helper_clears_every_state_pointing_var`] covers the escape hatches
//!     a `$HOME` redirect alone does not close.
//!   - [`no_test_source_names_the_bin_env_outside_the_helper`] — HARD rule, no
//!     allowlist. `tests/common/mod.rs` is the only source that may name the
//!     cargo-provided binary-path variable, so every other spawn site must come
//!     through the helper to reach the binary at all.
//!   - [`raw_binary_path_use_does_not_grow`] — RATCHET. Two targets need the
//!     PATH rather than a `Command` (a `sh -c` pipeline, a `PATH` shim) and one
//!     is a deliberately live `#[ignore]` test; each is counted against
//!     [`RAW_BIN_BUDGET`] so the population cannot grow unreviewed.
//!
//! The scan is line-based over `crates/trusty-mpm/tests/**`, not a Rust parser.
//! The lib target cannot host this defect at all — cargo sets the binary-path
//! variable only for integration and bench targets — so `src/**` is out of
//! scope by construction rather than by omission.
//! Test: this file IS the test module.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// An invocation that reaches `main`'s body and writes the framework root.
///
/// Why: `--help` exits inside clap, before the one-time skill migration that
/// creates `<framework root>/migrations.json` — so it proves nothing about
/// which `$HOME` the child resolved. A real subcommand against a dead port
/// runs that block, fails fast on the transport, and never needs a daemon.
const PROBE_ARGS: &[&str] = &["--url", "http://127.0.0.1:1", "sessions", "list"];

/// The file a `tm` run leaves in whichever framework root it resolved.
const MIGRATION_MARKER: &str = "migrations.json";

/// Run the probe and return the framework root it should have written.
fn framework_root_of(home: &Path) -> PathBuf {
    home.join(".trusty-mpm")
}

/// The operator's own framework root, when this process has a `$HOME` at all.
fn operator_framework_root() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| framework_root_of(Path::new(&home)))
}

/// Direct children of `root`, plus the migration marker's size and mtime.
///
/// Why: the harm #7568 reports is new FILES under the operator's framework root
/// — `migrations.json`, `compression.jsonl`, a `usage/` marker tree. A name set
/// catches exactly that. A recursive mtime comparison would not: on a developer
/// machine a live daemon writes under `~/.trusty-mpm/` throughout the run, so
/// mtime equality would report a flake rather than a finding. The marker file
/// is exempt from that reasoning — it is written at most once per machine, so
/// its size and mtime are a stable equality check.
/// What: `(sorted direct child names, Some((len, mtime)) for the marker)`. A
/// missing root is an empty snapshot, which is the CI case.
/// Test: the two live tests below take it before and after.
fn snapshot(root: &Path) -> (Vec<String>, Option<(u64, SystemTime)>) {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let marker = std::fs::metadata(root.join(MIGRATION_MARKER))
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)));
    (names, marker)
}

/// The hazard, pinned: a spawned `tm` writes into the `$HOME` it is given.
///
/// Why: this is the pre-fix shape every spawn site had, and it is what makes
/// the regression guard below meaningful — without it, a `migrations.json` that
/// never appeared for an unrelated reason would read as a pass.
/// What: spawns the binary with `$HOME` pointed at a decoy directory standing
/// in for the operator's, with no isolation, and asserts the child created the
/// decoy's framework root.
/// Test: this function IS the test.
#[test]
fn an_unisolated_spawn_writes_into_whatever_home_it_inherits() {
    let decoy = tempfile::tempdir().expect("decoy $HOME");

    // #7568: deliberately NOT routed through `common::tm_command_in` — this
    // test exists to show what that helper prevents.
    let output = Command::new(common::tm_bin())
        .args(PROBE_ARGS)
        .env("HOME", decoy.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn the tm binary");

    assert!(
        framework_root_of(decoy.path())
            .join(MIGRATION_MARKER)
            .is_file(),
        "an unisolated `tm` must write its framework root under the `$HOME` it was \
         handed — if this stops holding, the regression guard below is asserting on \
         a file nothing creates any more and must be re-pointed. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The helper confines the child and leaves the operator's root untouched.
///
/// Why: the closure condition of #7568. The child must resolve the home the
/// helper named, not the one the test process would have passed down.
/// What: sets a decoy `$HOME` on the child FIRST — indistinguishable, from the
/// child's side, from an inherited one — then applies `isolate_spawned_tm`,
/// which must win. Asserts the confined home gained the marker, the decoy gained
/// nothing at all, and the operator's framework root has no new direct child and
/// an unchanged marker.
/// Test: this function IS the test.
#[test]
fn a_spawned_tm_resolves_the_helper_home_and_leaves_the_operators_alone() {
    let decoy = tempfile::tempdir().expect("decoy $HOME");
    let confined = tempfile::tempdir().expect("confined $HOME");
    let operator = operator_framework_root();
    let before = operator.as_deref().map(snapshot);

    let mut cmd = Command::new(common::tm_bin());
    cmd.env("HOME", decoy.path());
    common::isolate_spawned_tm(&mut cmd, confined.path());
    let output = cmd
        .args(PROBE_ARGS)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn the tm binary");

    assert!(
        framework_root_of(confined.path())
            .join(MIGRATION_MARKER)
            .is_file(),
        "the child must resolve the helper's `$HOME`; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !framework_root_of(decoy.path()).exists(),
        "the child must not touch the `$HOME` it would otherwise have inherited"
    );

    if let (Some(root), Some(before)) = (operator.as_deref(), before) {
        let after = snapshot(root);
        let added: Vec<&String> = after.0.iter().filter(|n| !before.0.contains(n)).collect();
        assert!(
            added.is_empty(),
            "a spawned `tm` added {added:?} under the operator's own {} (#7568). Cargo runs \
             integration targets as parallel PROCESSES, so a concurrent target's unisolated \
             spawn can be the writer — that is the same defect, reported here rather than \
             silently tolerated.",
            root.display()
        );
        assert_eq!(
            after.1, before.1,
            "the operator's {MIGRATION_MARKER} changed size or mtime across the run"
        );
    }
}

/// Every state-pointing variable is cleared on the child.
///
/// Why: `$HOME` alone does not confine the child — `TRUSTY_MPM_ROOT` outranks
/// the home-relative default outright, and `CLAUDE_CONFIG_DIR` is absolute, so
/// either one inherited from the operator's shell re-escapes a redirected home.
/// What: hands the child a `TRUSTY_MPM_ROOT` pointing at a directory the helper
/// must strip, then asserts the marker landed under the helper's home rather
/// than that root.
/// Test: this function IS the test.
#[test]
fn the_helper_clears_every_state_pointing_var() {
    let confined = tempfile::tempdir().expect("confined $HOME");
    let escape = tempfile::tempdir().expect("escape root");

    let mut cmd = Command::new(common::tm_bin());
    cmd.env("TRUSTY_MPM_ROOT", escape.path())
        .env("CLAUDE_CONFIG_DIR", escape.path());
    common::isolate_spawned_tm(&mut cmd, confined.path());
    let output = cmd
        .args(PROBE_ARGS)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn the tm binary");

    assert!(
        !escape.path().join(MIGRATION_MARKER).is_file(),
        "an inherited TRUSTY_MPM_ROOT must be stripped, not honoured; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        framework_root_of(confined.path())
            .join(MIGRATION_MARKER)
            .is_file(),
        "with the escape hatches stripped the child falls back to the helper's `$HOME`"
    );
}

// ---------------------------------------------------------------------------
// Source rules
// ---------------------------------------------------------------------------

/// The cargo variable naming the built binary, spelled in two halves.
///
/// Why: this file's own scan reads every `tests/**` source. Writing the name as
/// one literal here would make the gate fire on itself, and skipping this file
/// from the scan to avoid that would leave a hole any future edit could widen.
/// Splitting it keeps the file inside its own scan.
const BIN_ENV_HALVES: (&str, &str) = ("CARGO_BIN_", "EXE_tm");

/// The only source permitted to name [`BIN_ENV_HALVES`].
const HELPER_SUFFIX: &str = "tests/common/mod.rs";

/// This file, skipped by rule 2 only.
///
/// Why: rule 1 still reads this file — that is what [`BIN_ENV_HALVES`]'s split
/// spelling buys. Rule 2 counts a bare `tm_bin(` token, which this file's own
/// assertion messages and detector sample carry as ordinary string data, so a
/// budget number here would track its prose rather than its spawns.
const SELF_SUFFIX: &str = "tests/spawned_tm_home_isolation.rs";

/// Per-file budget of `tm_bin()` call sites — spawns that take the raw PATH.
///
/// Why: a `Command` built by the helper is isolated by construction; a caller
/// that takes the PATH string builds its own process and must apply
/// `common::isolate_spawned_tm` itself. That is a reviewable claim per site, so
/// the population is ratcheted rather than banned. Lowering a number is always
/// welcome; raising one says a new raw-PATH spawn was reviewed.
/// What: `(path suffix, sites)`, counted after comment stripping.
const RAW_BIN_BUDGET: &[(&str, usize)] = &[
    // A `PATH` shim: the hook under test finds `tm` by name, so it needs the
    // path, and the isolation goes on the `/bin/sh` that runs the hook.
    ("tests/commit_stats_hook_budget.rs", 1),
    // A `sh -c` pipeline with `tm compress` at its tail; the isolation goes on
    // the shell.
    ("tests/tm_compress_pipe.rs", 1),
    // `#[ignore]`d live test (#1053): it drives a real `claude` session against
    // the framework the operator actually installed, so a scratch `$HOME` would
    // make it untestable rather than hermetic. Never run in CI.
    ("tests/meta_demo_e2e.rs", 1),
];

/// `crates/trusty-mpm/tests`.
fn tests_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// Every `.rs` file under `tests/`.
fn test_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&tests_root(), &mut out);
    assert!(
        out.len() > 20,
        "the tests/ walk found only {} files — a broken walk reports a clean tree \
         regardless of its contents",
        out.len()
    );
    out.sort();
    out
}

/// Offset of the first `//` in `segment` that opens a comment.
///
/// Why: these targets spawn `tm` against `http://127.0.0.1:1` on the same line
/// as the command they build. Treating that `//` as a comment would truncate
/// the line and hide whatever follows, which for a one-line spawn is the whole
/// detection. `prefix` is what the line already contributed, so a `://` split
/// across a block-comment boundary still reads as a URL.
fn line_comment_at(prefix: &str, segment: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = segment[from..].find("//") {
        let at = from + rel;
        let preceding = if at == 0 {
            prefix.chars().last()
        } else {
            segment[..at].chars().last()
        };
        if preceding == Some(':') {
            from = at + 2;
            continue;
        }
        return Some(at);
    }
    None
}

/// `text` with `/* … */` and `// …` comments removed.
///
/// Why: several targets discuss the binary-path variable in their module docs,
/// and this file's own prose names the raw-PATH helper. Counting prose would
/// make both rules fire on documentation. The two comment forms must be read in
/// the order they APPEAR, not one kind and then the other: a doc comment
/// carrying a glob such as `src/bin/tm/**` contains `/*`, so a block-comment
/// pass that ran first would hunt for a `*/` that never comes and discard the
/// rest of the file — silently reporting a clean target, which is the one
/// outcome a guard must never produce.
/// What: one pass per line carrying a block-comment flag, taking whichever of
/// `//` (see [`line_comment_at`]) and `/*` comes first. Lines are preserved, so
/// the result stays line-addressable.
/// Test: `comment_stripping_keeps_code_and_drops_prose`.
fn code_only(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let mut kept = String::new();
        let mut rest = line;
        loop {
            if in_block {
                match rest.find("*/") {
                    Some(at) => {
                        rest = &rest[at + 2..];
                        in_block = false;
                    }
                    None => break,
                }
            }
            let comment = line_comment_at(&kept, rest);
            let block = rest.find("/*");
            match (comment, block) {
                (Some(c), Some(b)) if b < c => {
                    kept.push_str(&rest[..b]);
                    rest = &rest[b + 2..];
                    in_block = true;
                }
                (Some(c), _) => {
                    kept.push_str(&rest[..c]);
                    break;
                }
                (None, Some(b)) => {
                    kept.push_str(&rest[..b]);
                    rest = &rest[b + 2..];
                    in_block = true;
                }
                (None, None) => {
                    kept.push_str(rest);
                    break;
                }
            }
        }
        lines.push(kept);
    }
    lines.join("\n")
}

/// Rule 1 — only the helper names the cargo binary-path variable.
///
/// Why: it is the one thing every spawn site needs, so gating it routes every
/// site through `tests/common/mod.rs`, where the isolation lives. The allowlist
/// is a single path and is deliberately not extendable by a table: a target
/// that genuinely needs the raw PATH calls `common::tm_bin()` and answers to
/// rule 2 instead.
/// Test: this function IS the test; `the_rules_detect_what_they_are_shown`
/// proves the detection fires.
#[test]
fn no_test_source_names_the_bin_env_outside_the_helper() {
    let needle = format!("{}{}", BIN_ENV_HALVES.0, BIN_ENV_HALVES.1);
    let mut findings = Vec::new();
    for path in test_sources() {
        let display = path.display().to_string();
        if display.ends_with(HELPER_SUFFIX) {
            continue;
        }
        if code_only(&std::fs::read_to_string(&path).expect("read source")).contains(&needle) {
            findings.push(display);
        }
    }
    assert!(
        findings.is_empty(),
        "a test source names the cargo binary-path variable directly (#7568). A spawned `tm` \
         that inherits `$HOME` resolves the OPERATOR's `~/.trusty-mpm`. Build the command with \
         `common::tm_command()` / `common::tm_command_in(home)`, or — if you need the PATH \
         rather than a `Command` — `common::tm_bin()` plus `common::isolate_spawned_tm` on the \
         process you do spawn.\n  {}",
        findings.join("\n  ")
    );
}

/// Rule 2 — the raw-PATH spawn population does not grow.
///
/// Why: `common::tm_bin()` hands back a path with no isolation attached, so each
/// use is a place the confinement is applied by hand and could be forgotten.
/// What: counts `tm_bin(` per file against [`RAW_BIN_BUDGET`], reporting both
/// over-budget (a regression) and under-budget (a stale row to lower).
/// Test: this function IS the test.
#[test]
fn raw_binary_path_use_does_not_grow() {
    let mut over = Vec::new();
    let mut stale = Vec::new();
    for path in test_sources() {
        let display = path.display().to_string();
        if display.ends_with(HELPER_SUFFIX) || display.ends_with(SELF_SUFFIX) {
            continue;
        }
        let code = code_only(&std::fs::read_to_string(&path).expect("read source"));
        let count = code.matches("tm_bin(").count();
        let budget = RAW_BIN_BUDGET
            .iter()
            .find(|(suffix, _)| display.ends_with(suffix))
            .map_or(0, |(_, n)| *n);
        if count > budget {
            over.push(format!(
                "{display}: {count} raw-PATH spawns, budget {budget}"
            ));
        } else if count < budget {
            stale.push(format!(
                "{display}: {count} raw-PATH spawns, budget {budget}"
            ));
        }
    }
    assert!(
        over.is_empty(),
        "a new raw-PATH spawn of the `tm` binary (#7568). Prefer `common::tm_command()`; if the \
         PATH really is what you need, apply `common::isolate_spawned_tm` to the process you \
         spawn and raise this file's `RAW_BIN_BUDGET` row, saying why in the PR.\n  {}",
        over.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "`RAW_BIN_BUDGET` is stale — lower these rows so the ratchet keeps its grip.\n  {}",
        stale.join("\n  ")
    );
}

/// The scanner reads code and ignores prose.
///
/// Why: a scan that matches nothing passes forever and protects nothing, which
/// is the same outcome as the defect it guards. This shows it fires on the real
/// shapes and stays quiet on the documented ones.
/// Test: this function IS the test.
#[test]
fn comment_stripping_keeps_code_and_drops_prose() {
    let needle = format!("{}{}", BIN_ENV_HALVES.0, BIN_ENV_HALVES.1);
    let raw = format!("tm_{}", "bin(");
    let sample = format!(
        "//! Runs the built binary (`{needle}`) as a child.\n\
         //! Keeps the `src/bin/tm/**` ratchet intact.\n\
         /* {needle} in a block comment */\n\
         fn a() {{ Command::new(env!(\"{needle}\")) }}\n\
         // let bin = common::{raw});\n\
         fn b() {{ let bin = common::{raw}); }}\n\
         fn c() {{ run(\"http://127.0.0.1:1\", env!(\"{needle}\")); }}\n"
    );
    let code = code_only(&sample);

    assert_eq!(
        code.matches(&needle).count(),
        2,
        "the two live call sites must survive stripping — including the one \
         behind a `http://` URL on the same line; got:\n{code}"
    );
    assert_eq!(
        code.matches(&raw).count(),
        1,
        "the commented-out raw-PATH use must not be counted; got:\n{code}"
    );
    assert!(
        code.contains("fn c()"),
        "a `/*`-shaped glob inside a doc comment must not swallow the rest of \
         the file — that shape reports a clean target regardless of its \
         contents; got:\n{code}"
    );
}
