//! Mechanical guard: no test in the `tm` bin target may repoint `$HOME`.
//!
//! Why (#5544): `cargo test` runs a target's tests as threads in ONE process,
//! so `std::env::set_var` is visible to every other test in the same binary for
//! as long as it is set. A restore-on-drop guard bounds the leak's lifetime but
//! not its visibility, and `#[serial_test::serial]` excludes only other
//! `#[serial]` tests — the default group is not a lock against the parallel
//! majority. `$HOME` is the worst variable to leak this way because it is read
//! TRANSITIVELY: `dirs::home_dir`, `FrameworkPaths::default`, `tempfile`, and
//! the three-tier agent-roster scan all consult it, so the set of tests that can
//! observe a repoint is unbounded and cannot be enumerated by inspection. That
//! produced a real false red — two compose tests saw 38 agents instead of 43,
//! missing exactly the five carried only by `~/.claude/agents`, because
//! `$CLAUDE_CONFIG_DIR` is absolute and survives a `$HOME` repoint while
//! `~/.claude/agents` does not.
//!
//! Fixing the six writers by hand does not close the class: the next non-serial
//! reader anyone adds reopens it, silently. This file is what makes the repair
//! durable — it fails the build when a writer comes back.
//!
//! What: two rules over the `tm` bin target's own sources.
//!   1. HARD BAN, no allowlist — nothing may write `HOME` or `CLAUDE_CONFIG_DIR`
//!      into the process environment.
//!   2. RATCHET — every other `set_var` / `remove_var` / `set_current_dir` site
//!      is counted per file against [`ENV_MUTATION_BUDGET`]. A new site, or a
//!      site in a file absent from the table, fails. Lowering a number as sites
//!      are removed is always welcome; raising one is a deliberate, reviewable
//!      claim that a new process-global write is warranted.
//!
//! The audit unit is the TEST TARGET, not the crate. `serial_test` groups and
//! process-global env are both per process, so neither can span test binaries —
//! the trusty-mpm LIB target has its own, larger population of `$HOME` writers
//! that these tests never raced, because it is a different process. This guard
//! deliberately scans only `src/bin/tm/`.
//!
//! Limits, stated plainly. This is a source scan, not a type-system barrier:
//! it cannot stop a write routed through a helper in another target, and it
//! skips its own file (whose patterns would otherwise match themselves). It
//! detects rather than prevents. Rule 1 is the one that closes #5544's class;
//! rule 2 keeps the general population from growing while the remaining
//! variables are migrated to injected seams.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

/// Variables no `tm` bin-target source may write into the process environment.
///
/// Why: both are read transitively by the agent-tier resolution every session
/// composition performs, so a repoint of either is observable by an
/// unenumerable set of concurrent readers. See the module docs.
const BANNED_ENV_WRITES: &[&str] = &["HOME", "CLAUDE_CONFIG_DIR"];

/// Per-file budget of remaining process-global env mutations, path-suffix keyed.
///
/// Why: these variables (`TRUSTY_MPM_MANAGED_SESSION_ID`, `REPOS_ROOT`, `TMUX`,
/// `NO_COLOR`, `PATH`) each have a small, enumerable reader set, unlike `$HOME`.
/// They are ratcheted rather than banned so the population cannot grow while
/// they are migrated to injected seams one at a time.
/// What: `(path suffix, exact permitted count)`. Sites are counted after
/// comment stripping.
const ENV_MUTATION_BUDGET: &[(&str, usize)] = &[
    // Production: PATH manipulation around a `gh` invocation.
    ("tm/gh_identity.rs", 3),
    // Production: daemonisation chdir.
    ("tm/commands/daemon_run.rs", 2),
    // Test-only: TRUSTY_MPM_ROOT / XDG_CONFIG_HOME behind a file-local mutex.
    ("tm/commands/managed_root.rs", 2),
    // Test-only: TRUSTY_MPM_SUB_AGENT / TRUSTY_MPM_DISABLE_HOOKS.
    ("tm/tests_behavior_a.rs", 4),
    // Test-only: REPOS_ROOT / TMUX / managed-session-id.
    ("tm/tests_behavior_b_tests.rs", 21),
    ("tm/tests_behavior_c_tests.rs", 9),
    ("tm/commands/managed_workspace_tests.rs", 3),
    ("tm/commands/guided_inplace/tests.rs", 6),
    // Test-only: NO_COLOR.
    ("tm/commands/session_picker_tests.rs", 6),
];

/// This file's own basename, skipped so its pattern literals do not match.
const SELF_BASENAME: &str = "env_isolation_tests.rs";

/// The `tm` bin target's source root.
fn bin_target_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("bin")
        .join("tm")
}

/// Every `.rs` file under the `tm` bin target, this file excluded.
fn bin_target_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .filter_map(Result::ok);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != SELF_BASENAME)
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&bin_target_root(), &mut out);
    assert!(
        out.len() > 20,
        "the bin-target scan found only {} files — the walk is broken, and a \
         broken walk would report a clean target regardless of its contents",
        out.len()
    );
    out.sort();
    out
}

/// Remove `//` line comments and `/* */` block comments.
///
/// Why: this module's prose, and the `// SAFETY:` notes at the surviving
/// mutation sites, discuss `set_var` and `$HOME` at length. Counting those
/// would make the guard fire on documentation.
/// What: strips block comments first (non-greedy, across lines), then trailing
/// line comments. Byte offsets are not preserved, so callers report file-level
/// findings rather than line numbers.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut in_block = false;
    let mut in_line = false;
    while i < bytes.len() {
        if in_block {
            if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                in_block = false;
                i += 2;
            } else {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
        } else if in_line {
            if bytes[i] == b'\n' {
                in_line = false;
                out.push('\n');
            }
            i += 1;
        } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            in_block = true;
            i += 2;
        } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            in_line = true;
            i += 2;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Byte offsets of every `set_var(` / `remove_var(` / `set_current_dir(` call.
fn mutation_sites(code: &str) -> Vec<usize> {
    const CALLS: &[&str] = &["set_var", "remove_var", "set_current_dir"];
    let mut sites = Vec::new();
    for call in CALLS {
        let mut from = 0;
        while let Some(rel) = code[from..].find(call) {
            let at = from + rel;
            // Require a call, allowing whitespace/newline before the paren.
            let tail = code[at + call.len()..].trim_start();
            if tail.starts_with('(') {
                sites.push(at);
            }
            from = at + call.len();
        }
    }
    sites
}

/// Rule 1 — nothing in the `tm` bin target writes `$HOME` or `$CLAUDE_CONFIG_DIR`.
///
/// Why: see the module docs. This is the rule that closes #5544's race class:
/// with zero writers, a newly added non-serial reader of a `$HOME`-relative
/// tier cannot race one, because there is nothing left to race.
/// What: for each mutation site, inspects the following 120 code bytes — enough
/// to span a multi-line `set_var(\n    "HOME",\n    …)` — for a banned name.
/// Test: this function IS the test.
#[test]
fn bin_target_writes_no_home_env() {
    let mut findings: Vec<String> = Vec::new();
    for path in bin_target_sources() {
        let code = strip_comments(&std::fs::read_to_string(&path).expect("read source"));
        for at in mutation_sites(&code) {
            let window = &code[at..code.len().min(at + 120)];
            for banned in BANNED_ENV_WRITES {
                if window.contains(banned) {
                    findings.push(format!("{}: writes ${banned}", path.display()));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "process-global `${{HOME}}`/`$CLAUDE_CONFIG_DIR` writes are banned in the `tm` bin \
         target (#5544) — `cargo test` runs this target's tests as threads in ONE process, so \
         the write is visible to every parallel sibling and `#[serial]` does not exclude them. \
         Inject the path instead: `BannerEnv` (formatters/banner/source.rs), `trust_cmd_in` \
         (commands/project.rs), and `FrameworkPaths::under` are the established seams.\n  {}",
        findings.join("\n  ")
    );
}

/// Rule 2 — the remaining process-global env mutations do not grow.
///
/// Why: `$HOME` is banned outright because its readers are unenumerable; the
/// other variables still mutated here have small, known reader sets and are
/// ratcheted instead. Without a ratchet the population drifts back up, and the
/// next variable to acquire a wide reader set repeats #5544 under a new name.
/// What: counts sites per file and compares against [`ENV_MUTATION_BUDGET`].
/// Reports both directions — an over-budget file is a regression, an
/// under-budget one is a stale table entry to lower.
/// Test: this function IS the test.
#[test]
fn bin_target_env_mutation_count_does_not_grow() {
    let mut over: Vec<String> = Vec::new();
    let mut stale: Vec<String> = Vec::new();
    for path in bin_target_sources() {
        let code = strip_comments(&std::fs::read_to_string(&path).expect("read source"));
        let found = mutation_sites(&code).len();
        let display = path.display().to_string();
        let budget = ENV_MUTATION_BUDGET
            .iter()
            .find(|(suffix, _)| display.ends_with(suffix))
            .map(|(_, n)| *n)
            .unwrap_or(0);
        if found > budget {
            over.push(format!("{display}: {found} sites, budget {budget}"));
        } else if found < budget {
            stale.push(format!("{display}: {found} sites, budget {budget}"));
        }
    }
    assert!(
        over.is_empty(),
        "new process-global env mutation in the `tm` bin target (#5544). Prefer an injected \
         path or value — see `BannerEnv` and `PathEnv` for the pattern. If a process write is \
         genuinely required, raise the file's number in `ENV_MUTATION_BUDGET` in this file and \
         say why in the PR.\n  {}",
        over.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "`ENV_MUTATION_BUDGET` is stale — these files now mutate less than their recorded \
         budget. Lower the numbers so the ratchet keeps its grip.\n  {}",
        stale.join("\n  ")
    );
}

/// The guard detects a violation it is shown.
///
/// Why: a scanner that silently matches nothing passes forever and protects
/// nothing — the failure mode this guard exists to prevent is exactly the
/// failure mode a broken guard hides. Proving it fires on a synthetic sample is
/// what separates "no violations" from "no detection".
/// What: runs the rule-1 detection over literal source text containing a
/// `$HOME` write in three shapes (inline, multi-line, `remove_var`), asserts all
/// three are found, and asserts a commented-out write and an unrelated variable
/// are not.
/// Test: this function IS the test.
#[test]
fn the_guard_detects_a_home_write_and_ignores_prose() {
    let sample = concat!(
        "fn a() { unsafe { std::env::set_var(\"HOME\", p) } }\n",
        "fn b() { unsafe { std::env::set_var(\n    \"HOME\",\n    p,\n) } }\n",
        "fn c() { unsafe { std::env::remove_var(\"CLAUDE_CONFIG_DIR\") } }\n",
        "// fn d() { std::env::set_var(\"HOME\", p) }\n",
        "fn e() { unsafe { std::env::set_var(\"NO_COLOR\", \"1\") } }\n",
    );
    let code = strip_comments(sample);
    let banned: Vec<usize> = mutation_sites(&code)
        .into_iter()
        .filter(|at| {
            let window = &code[*at..code.len().min(at + 120)];
            BANNED_ENV_WRITES.iter().any(|b| window.contains(b))
        })
        .collect();

    assert_eq!(
        banned.len(),
        3,
        "the guard must catch the inline, multi-line, and remove_var shapes — and only those. \
         Stripped source:\n{code}"
    );
    assert_eq!(
        mutation_sites(&code).len(),
        4,
        "the commented-out write must not be counted, the NO_COLOR one must be"
    );
}
