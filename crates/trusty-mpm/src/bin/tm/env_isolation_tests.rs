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
//! Limits, stated plainly. This is a source scan, not a type-system barrier.
//! Rule 1 can only read a STRING LITERAL, so a call whose key is a variable
//! could write anything — `commands/managed_root.rs`'s two-line
//! `fn set(key, val)` is that exact shape and predates this guard. Those calls
//! are not waved through: rule 2 counts them separately, so a new one fails and
//! each recorded number is a reviewable claim about the calls already there.
//! What remains genuinely open is a write routed through a helper in ANOTHER
//! target, and this file, which is skipped so its own pattern literals do not
//! match itself. It detects; it does not prevent.
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
/// `NO_COLOR`, `TRUSTY_MPM_ROOT`, `XDG_CONFIG_HOME`, `PATH`) each have a small,
/// enumerable reader set, unlike `$HOME`. They are ratcheted rather than banned
/// so the population cannot grow while they are migrated to injected seams.
///
/// The SECOND number is the one rule 1 depends on. A `set_var` whose key is a
/// variable rather than a literal is unverifiable by a source scan — the two-line
/// `fn set(key, val)` in `commands/managed_root.rs` is the existing shape, and
/// the identical helper passed `"HOME"` would be invisible. Those calls are
/// counted separately so a NEW one fails the gate, and each recorded number is a
/// reviewable claim that the existing ones do not write a banned variable.
/// What: `(path suffix, total sites, sites with a non-literal key)`. Counted
/// after comment stripping. `set_current_dir` names no variable, so it is
/// counted in the total and never in the indirect column.
const ENV_MUTATION_BUDGET: &[(&str, usize, usize)] = &[
    // Production: PATH manipulation around a `gh` invocation.
    ("tm/gh_identity.rs", 3, 0),
    // Production: daemonisation chdir.
    ("tm/commands/daemon_run.rs", 2, 0),
    // Test-only: TRUSTY_MPM_ROOT / XDG_CONFIG_HOME behind a file-local mutex.
    ("tm/commands/managed_root.rs", 2, 2),
    // Test-only: TRUSTY_MPM_SUB_AGENT / TRUSTY_MPM_DISABLE_HOOKS.
    ("tm/tests_behavior_a.rs", 4, 4),
    // Test-only: REPOS_ROOT / TMUX / managed-session-id.
    ("tm/tests_behavior_b_tests.rs", 21, 18),
    ("tm/tests_behavior_c_tests.rs", 9, 9),
    ("tm/commands/managed_workspace_tests.rs", 3, 3),
    ("tm/commands/guided_inplace/tests.rs", 6, 4),
    // Test-only: NO_COLOR.
    ("tm/commands/session_picker_tests.rs", 6, 0),
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
    // #5544 (code-critic MEDIUM): this used to `push(bytes[i] as char)`, which
    // re-encodes every non-ASCII byte as a 2-byte char. Offsets shifted, and a
    // fixed-width window into the result could then split a char boundary and
    // panic — a false red on a legitimate file, and `src/bin/tm/` is full of
    // non-ASCII in ordinary code lines. Pushing the ORIGINAL slices keeps the
    // output byte-identical to the input outside comments.
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(at) = rest.find(['/', '\n']) else {
            out.push_str(rest);
            return out;
        };
        let after = &rest[at..];
        if let Some(body) = after.strip_prefix("/*") {
            out.push_str(&rest[..at]);
            let body_end = body.find("*/").map(|e| at + 2 + e + 2);
            let body = match body_end {
                Some(end) => &rest[at..end],
                None => rest,
            };
            // Keep the newlines so line-oriented reading of the result survives.
            for _ in body.matches('\n') {
                out.push('\n');
            }
            match body_end {
                Some(end) => rest = &rest[end..],
                None => return out,
            }
        } else if after.starts_with("//") {
            out.push_str(&rest[..at]);
            match after.find('\n') {
                Some(nl) => rest = &rest[at + nl..],
                None => return out,
            }
        } else {
            // A lone `/` or a newline: copy through and continue past it.
            let step = at + rest[at..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&rest[..step]);
            rest = &rest[step..];
        }
    }
}

/// The code following `at`, capped at 120 chars, never splitting a char.
///
/// Why (#5544, code-critic MEDIUM): a byte-width slice can land mid-char and
/// panic. Taking CHARS cannot. 120 is enough to span a multi-line
/// `set_var(\n    "HOME",\n    p,\n)`.
/// What: `code[at..]` truncated to 120 chars.
/// Test: `the_guard_detects_a_home_write_and_ignores_prose`,
/// `the_guard_survives_non_ascii_source`.
fn window_after(code: &str, at: usize) -> String {
    code[at..].chars().take(120).collect()
}

/// Does the mutation at `at` name its variable with a string literal?
///
/// Why (#5544, code-critic MEDIUM): rule 1 can only read a literal. A call whose
/// key is a variable — `commands/managed_root.rs`'s `fn set(key, val)` is the
/// existing example — could write `HOME` and the scan would never see it. Such
/// a call is therefore UNVERIFIABLE rather than clean, and is ratcheted by
/// [`ENV_MUTATION_BUDGET`]'s second number instead of being silently allowed.
/// What: skips to the opening paren and reports whether the first non-space
/// character of the argument list starts a string literal (`"` or `r"`/`r#"`).
/// Test: `the_guard_flags_an_indirect_env_key`.
fn has_literal_key(code: &str, at: usize) -> bool {
    let Some(open) = code[at..].find('(') else {
        return false;
    };
    let args = code[at + open + 1..].trim_start();
    args.starts_with('"') || args.starts_with("r\"") || args.starts_with("r#\"")
}

/// Every `set_var(` / `remove_var(` / `set_current_dir(` call, as
/// `(byte offset, names a variable)`.
///
/// Why: `set_current_dir` mutates process-global state and so belongs in the
/// ratchet, but it takes no variable NAME — it cannot smuggle a `$HOME` write
/// past rule 1, so it is excluded from the indirect-key column.
/// What: scans for each call, allowing whitespace before the paren.
fn mutation_sites(code: &str) -> Vec<(usize, bool)> {
    const CALLS: &[(&str, bool)] = &[
        ("set_var", true),
        ("remove_var", true),
        ("set_current_dir", false),
    ];
    let mut sites = Vec::new();
    for (call, names_a_var) in CALLS {
        let mut from = 0;
        while let Some(rel) = code[from..].find(call) {
            let at = from + rel;
            if code[at + call.len()..].trim_start().starts_with('(') {
                sites.push((at, *names_a_var));
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
/// What: for each mutation site, inspects the following 120 CHARS — enough to
/// span a multi-line `set_var(\n    "HOME",\n    …)` — for a banned name. A site
/// whose key is not a literal is unverifiable and is handled by rule 2's
/// indirect column instead.
/// Test: this function IS the test; `the_guard_detects_a_home_write_and_ignores_prose`
/// proves the detection fires.
#[test]
fn bin_target_writes_no_home_env() {
    let mut findings: Vec<String> = Vec::new();
    for path in bin_target_sources() {
        let code = strip_comments(&std::fs::read_to_string(&path).expect("read source"));
        for (at, names_a_var) in mutation_sites(&code) {
            if !names_a_var || !has_literal_key(&code, at) {
                continue;
            }
            let window = window_after(&code, at);
            for banned in BANNED_ENV_WRITES {
                if window.contains(banned) {
                    findings.push(format!("{}: writes ${banned}", path.display()));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "process-global `$HOME`/`$CLAUDE_CONFIG_DIR` writes are banned in the `tm` bin \
         target (#5544) — `cargo test` runs this target's tests as threads in ONE process, so \
         the write is visible to every parallel sibling and `#[serial]` does not exclude them. \
         Inject the path instead: `BannerEnv` (formatters/banner/source.rs), `trust_cmd_in` \
         (commands/project.rs), and `FrameworkPaths::under` are the established seams.\n  {}",
        findings.join("\n  ")
    );
}

/// Rule 2 — the remaining process-global env mutations do not grow, and no new
/// unverifiable one appears.
///
/// Why: `$HOME` is banned outright because its readers are unenumerable; the
/// other variables still mutated here have small, known reader sets and are
/// ratcheted instead. The indirect column is what stops rule 1 being defeated by
/// a two-line helper — an existing `fn set(key, val)` can already write anything,
/// so the only durable control is that no NEW one appears unreviewed.
/// What: counts total and non-literal-key sites per file against
/// [`ENV_MUTATION_BUDGET`]. Reports both directions — over budget is a
/// regression, under budget is a stale entry to lower.
/// Test: this function IS the test.
#[test]
fn bin_target_env_mutation_count_does_not_grow() {
    let mut over: Vec<String> = Vec::new();
    let mut stale: Vec<String> = Vec::new();
    for path in bin_target_sources() {
        let code = strip_comments(&std::fs::read_to_string(&path).expect("read source"));
        let sites = mutation_sites(&code);
        let total = sites.len();
        let indirect = sites
            .iter()
            .filter(|(at, names_a_var)| *names_a_var && !has_literal_key(&code, *at))
            .count();
        let display = path.display().to_string();
        let (budget, indirect_budget) = ENV_MUTATION_BUDGET
            .iter()
            .find(|(suffix, _, _)| display.ends_with(suffix))
            .map(|(_, t, i)| (*t, *i))
            .unwrap_or((0, 0));
        if total > budget {
            over.push(format!("{display}: {total} sites, budget {budget}"));
        } else if total < budget {
            stale.push(format!("{display}: {total} sites, budget {budget}"));
        }
        if indirect > indirect_budget {
            over.push(format!(
                "{display}: {indirect} mutations with a NON-LITERAL key, budget \
                 {indirect_budget} — a source scan cannot tell what these write, so rule 1 \
                 cannot clear them"
            ));
        } else if indirect < indirect_budget {
            stale.push(format!(
                "{display}: {indirect} non-literal-key mutations, budget {indirect_budget}"
            ));
        }
    }
    assert!(
        over.is_empty(),
        "new process-global env mutation in the `tm` bin target (#5544). Prefer an injected \
         path or value — see `BannerEnv` and `PathEnv` for the pattern. If a process write is \
         genuinely required, raise the file's numbers in `ENV_MUTATION_BUDGET` in this file and \
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
        .filter(|(at, names_a_var)| {
            *names_a_var && has_literal_key(&code, *at) && {
                let window = window_after(&code, *at);
                BANNED_ENV_WRITES.iter().any(|b| window.contains(b))
            }
        })
        .map(|(at, _)| at)
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

/// An indirect key is reported as unverifiable, not as clean.
///
/// Why (#5544, code-critic MEDIUM): rule 1 reads literals. A helper taking the
/// key as a parameter can write anything, and `commands/managed_root.rs` already
/// has one. Rule 2's indirect column is the control; this proves the classifier
/// underneath it actually distinguishes the two shapes.
/// Test: this function IS the test.
#[test]
fn the_guard_flags_an_indirect_env_key() {
    let sample = concat!(
        "fn s(k: &str, v: &str) { unsafe { std::env::set_var(k, v) } }\n",
        "fn t() { unsafe { std::env::set_var(\"NO_COLOR\", \"1\") } }\n",
        "fn u() { unsafe { std::env::set_var(SOME_CONST, \"1\") } }\n",
    );
    let code = strip_comments(sample);
    let indirect: Vec<usize> = mutation_sites(&code)
        .into_iter()
        .filter(|(at, names_a_var)| *names_a_var && !has_literal_key(&code, *at))
        .map(|(at, _)| at)
        .collect();

    assert_eq!(
        indirect.len(),
        2,
        "a parameter key and a const key are both unverifiable; a string literal is not"
    );
}

/// The scanner does not panic on non-ASCII source.
///
/// Why (#5544, code-critic MEDIUM): `strip_comments` used to re-encode every
/// non-ASCII byte as a 2-byte char, so a fixed-width window into its output
/// could split a char boundary and panic. Non-ASCII in ordinary code lines is
/// widespread under `src/bin/tm/` — `cli/mod.rs` alone has about a hundred such
/// lines — so that would have been a false red on a legitimate file, which is
/// the one failure a guard must never produce.
/// What: strips a sample whose comments, string literals, and identifiers all
/// carry multi-byte characters, asserts the non-comment text survives
/// byte-identically, and takes a window at every mutation site.
/// Test: this function IS the test.
#[test]
fn the_guard_survives_non_ascii_source() {
    let sample = concat!(
        "// ── prose with box-drawing, em-dashes — and 🤖 ──\n",
        "const BANNER: &str = \"█▀█ trusty — ✅\";\n",
        "fn f() { unsafe { std::env::set_var(\"NO_COLOR\", \"→\") } }\n",
    );
    let code = strip_comments(sample);

    assert!(
        code.contains("█▀█ trusty — ✅"),
        "non-comment text must survive byte-identically, got:\n{code}"
    );
    assert!(!code.contains("🤖"), "the comment must still be stripped");
    for (at, _) in mutation_sites(&code) {
        // The assertion is that this does not panic on a char boundary.
        let _ = window_after(&code, at);
    }

    // And the real target's sources, which is where the panic would have landed.
    for path in bin_target_sources() {
        let code = strip_comments(&std::fs::read_to_string(&path).expect("read source"));
        for (at, _) in mutation_sites(&code) {
            let _ = window_after(&code, at);
        }
    }
}
