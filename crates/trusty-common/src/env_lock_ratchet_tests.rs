//! Mechanical guard: a test that mutates the environment under a `serial_test`
//! attribute must also take [`crate::data_dir::ENV_LOCK`].
//!
//! Why (#7253): this crate's lib test target serialises environment mutation in
//! three domains that do not exclude each other — `#[serial]`'s default group,
//! the named `#[serial(dotenv_credential_env)]` group, and `ENV_LOCK`, a plain
//! mutex that excludes only the tests that take it. Two different mutexes
//! serialise nothing against each other, so a test holding one still runs
//! inside `setenv`/`unsetenv` issued by a test holding another. That is a race
//! on process-global state, and it has produced real flakes twice: #6575 in
//! `http_client.rs`, and a 1-in-6 failure in trusty-search's
//! `search_rpc_tests.rs` found by the #7237 engineer. Both were repaired one
//! call site at a time, which is why the second one happened.
//!
//! What: over this crate's LIB target sources (`src/**`), a file that both uses
//! a `serial_test` attribute and mutates the process environment must mention
//! `ENV_LOCK`. Files that do so today are clean; the ones that do not are
//! listed in [`SERIAL_WITHOUT_ENV_LOCK`] with a reason. A new offender fails,
//! and a listed file that becomes compliant fails too, so the population can
//! only shrink.
//!
//! The audit unit is the TEST TARGET, not the crate. `ENV_LOCK` is
//! `#[cfg(test)] pub(crate)`, so only the lib target's own tests can take it,
//! and process-global env is per process — a test under `tests/` runs in a
//! different binary and cannot join this lock even in principle. This guard
//! therefore scans `src/**` only. Under `cargo nextest` each test gets its own
//! PROCESS (#4162), which isolates env state more strongly than either mutex;
//! the lock is what protects the ordinary `cargo test` run, where the whole
//! target is one process.
//!
//! The scan is confined to this crate. trusty-search's
//! `search_rpc_tests.rs`, the other file #7253 names, holds its own copy of the
//! problem and its own repair (#7257); it is not reachable from here and needs
//! no row in the table, whichever PR lands first.
//!
//! Why the rule is not narrowed to `TRUSTY_*` variables: the hazard is the
//! LOCK's scope, not the variable's name. `ENV_LOCK` is this crate's single
//! env-mutation lock (`memory_rpc.rs` and `http_client.rs` both describe it
//! that way), and the two flakes that motivated the issue were on
//! `TRUSTY_SEARCH_SOCKET` and on `HTTP_PROXY`. A `TRUSTY_`-only rule would
//! clear the second one, which is the file the issue names.
//!
//! Limits, stated plainly. This is a source scan, not a type-system barrier.
//!   - The unit is the FILE. A file that takes `ENV_LOCK` in one test and not
//!     another reads as compliant. File granularity is what makes the table
//!     reviewable, and it is the same trade `env_isolation_tests.rs` makes.
//!   - A file that mutates the environment with NO serialisation at all is a
//!     worse defect and is not covered here: #7253 is about reaching for the
//!     wrong lock, not about reaching for none.
//!   - The attribute is matched as written (`#[serial…`, `#[file_serial…`). A
//!     `#[cfg_attr(…, serial)]` spelling would be missed.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

/// Files that mutate the environment under `#[serial]` without taking
/// `ENV_LOCK`, each with the reason it is still here.
///
/// Why a table rather than a repair: every entry predates the rule, and taking
/// the lock changes the locking of a test that currently passes. Listing them
/// makes the remaining exposure countable and stops it growing; removing a row
/// is always welcome, adding one is a reviewable claim.
/// What: `(path suffix, reason)`. A file matching no suffix must be compliant.
/// The reason names the variables that file writes, which is what a reviewer
/// needs when deciding whether the row can go. A row another open PR is already
/// deleting carries `<PR> removes this row` so the two merge in either order.
const SERIAL_WITHOUT_ENV_LOCK: &[(&str, &str)] = &[
    ("src/bm25/tests.rs", "TRUSTY_BM25_CORPUS_CAP"),
    ("src/catchup/mod.rs", "TRUSTY_MEMORY_PALACE"),
    // The `dotenv_credential_env` group. These write provider API keys rather
    // than `TRUSTY_*` state, and `credentials/dotenv.rs` republishes arbitrary
    // keys in bulk — the widest env writer in the target, and the one whose
    // conversion needs the most care.
    ("src/credentials/authority.rs", "provider API keys"),
    ("src/credentials/dotenv.rs", "bulk republish of a .env file"),
    (
        "src/credentials/env_guard.rs",
        "TRUSTY_COMMON_ENV_GUARD_TEST_*, the guard's own tests",
    ),
    ("src/credentials/resolver.rs", "provider API keys"),
    ("src/inference/configurator/mod.rs", "provider API keys"),
    (
        "src/inference/configurator/resolver.rs",
        "provider API keys",
    ),
    ("src/memory_core/dream/tests.rs", "provider API keys"),
    (
        "src/memory_core/semantic_consolidation/mod.rs",
        "provider API keys",
    ),
    // Single-variable writers, one module each.
    (
        "src/daemon_guard.rs",
        "TRUSTY_TEST_ADDR_DIR_EMPTY and a caller-named data-dir variable",
    ),
    ("src/daemon_token.rs", "TRUSTY_TEST_DAEMON_TOKEN"),
    (
        "src/embedder_client/supervisor_tests.rs",
        "TRUSTY_EMBEDDERD_*",
    ),
    (
        "src/inference/providers/local.rs",
        "OLLAMA_HOST, TRUSTY_LOCAL_API_KEY",
    ),
    ("src/local_probe.rs", "OLLAMA_HOST"),
    (
        "src/memory_core/registry_tests.rs",
        "TRUSTY_MEMORY_MAX_OPEN_PALACES",
    ),
    ("src/palace_resolve_tests.rs", "TRUSTY_MEMORY_PALACE"),
    ("src/uds/on_demand_tests.rs", "TRUSTY_ANALYZE_EXTERNAL"),
    (
        "src/uds/supervisor/tests.rs",
        "TRUSTY_TEST_SUPERVISOR_EXTERNAL",
    ),
];

/// This file's own basename, skipped so its pattern literals do not match it.
const SELF_BASENAME: &str = "env_lock_ratchet_tests.rs";

/// The identifier that proves a file joined the crate-wide env lock.
const ENV_LOCK_IDENT: &str = "ENV_LOCK";

/// Attribute spellings that mean "this test asked `serial_test` to serialise
/// it". `#[serial_test::serial]` and `#[serial_test::file_serial]` are covered
/// by the first and second prefix respectively.
const SERIAL_ATTRIBUTES: &[&str] = &["#[serial", "#[file_serial"];

/// Calls that write the process environment, including this crate's own
/// cross-file wrapper.
///
/// Why `EnvVarGuard`: `credentials::env_guard` is the crate's one RAII env
/// guard, and its users (`memory_core::dream::tests`,
/// `memory_core::semantic_consolidation`) contain no raw `set_var` at all. A
/// scan for the std calls alone would read those files as environment-free.
const ENV_MUTATION_CALLS: &[&str] = &["set_var", "remove_var"];

/// Type names whose mere mention means the file mutates the environment.
const ENV_MUTATION_TYPES: &[&str] = &["EnvVarGuard"];

/// What one source file's contents say about the rule.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// No `serial_test` attribute, or no environment mutation: out of scope.
    OutOfScope,
    /// Mutates the environment under `#[serial]` and takes `ENV_LOCK`.
    Compliant,
    /// Mutates the environment under `#[serial]` and does not take `ENV_LOCK`.
    Offender,
}

/// Classify one source file.
///
/// Why `Option`: a file this guard cannot read or decode is unverifiable, and a
/// guard that treats "I could not look" as "nothing there" reports a clean
/// target regardless of its contents. Fail closed — it counts as an offender,
/// so an unreadable file fails the build rather than disappearing from it.
/// What: strips comments and string literals, then applies the three
/// predicates.
/// Test: `an_unreadable_source_counts_as_an_offender`,
/// `the_ratchet_detects_a_serial_mutation_without_env_lock`.
fn classify(source: Option<&str>) -> Verdict {
    let Some(text) = source else {
        return Verdict::Offender;
    };
    let code = strip_noncode(text);
    if !uses_serial_attribute(&code) || !mutates_env(&code) {
        return Verdict::OutOfScope;
    }
    if code.contains(ENV_LOCK_IDENT) {
        Verdict::Compliant
    } else {
        Verdict::Offender
    }
}

/// Does the stripped source carry a `serial_test` attribute?
fn uses_serial_attribute(code: &str) -> bool {
    SERIAL_ATTRIBUTES.iter().any(|a| code.contains(a))
}

/// Does the stripped source write the process environment?
fn mutates_env(code: &str) -> bool {
    ENV_MUTATION_CALLS.iter().any(|c| contains_call(code, c))
        || ENV_MUTATION_TYPES.iter().any(|t| code.contains(t))
}

/// Is `name` called anywhere in `code`?
///
/// What: `name` followed, after optional whitespace, by `(`. No left boundary
/// is required, so a locally named wrapper such as `unsafe_set_var(` still
/// counts — the fail-closed direction.
fn contains_call(code: &str, name: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = code[from..].find(name) {
        let at = from + rel;
        if code[at + name.len()..].trim_start().starts_with('(') {
            return true;
        }
        from = at + name.len();
    }
    false
}

/// Remove comments, string literals, and char literals, leaving code.
///
/// Why: the files this scans discuss `ENV_LOCK`, `#[serial]` and `set_var` at
/// length in their own prose — `daemon_addr.rs` and `http_client.rs` both
/// explain the very race this guard enforces. Counting a comment would make
/// the guard read documentation as compliance, which is the one direction a
/// fail-closed guard must never move in. String literals go too, so a URL such
/// as `"http://127.0.0.1/health"` cannot open a phantom line comment and delete
/// the rest of a real line.
/// What: a character state machine over code, line comments, nesting block
/// comments, string and raw-string literals, and char literals. Each removed
/// region becomes one space, so adjacent tokens never glue together.
/// Test: `the_stripper_removes_prose_and_string_literals`,
/// `the_ratchet_detects_a_serial_mutation_without_env_lock`.
fn strip_noncode(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < n {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            out.push(' ');
        } else if c == '/' && next == Some('*') {
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out.push(' ');
        } else if let Some(hashes) = raw_string_open(&chars, i) {
            i += 1 + hashes + 1;
            while i < n {
                if chars[i] == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#')) {
                    i += 1 + hashes;
                    break;
                }
                i += 1;
            }
            out.push(' ');
        } else if c == '"' {
            i += 1;
            while i < n {
                match chars[i] {
                    '\\' => i += 2,
                    '"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            out.push(' ');
        } else if let Some(len) = char_literal_len(&chars, i) {
            i += len;
            out.push(' ');
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// The hash count of a raw string literal opening at `i`, if one does.
///
/// What: matches `r"`, `r#"`, `r##"` … and their `b`-prefixed forms, and
/// requires that the `r` not continue an identifier — `foo_r` followed by a
/// string is two tokens, and the raw-identifier `r#type` has no quote.
fn raw_string_open(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&'r') {
        return None;
    }
    if i > 0 {
        let prev = chars[i - 1];
        // `br"…"` is a byte raw string; any other identifier character before
        // the `r` means this is the tail of a name, not a literal prefix.
        if (prev.is_alphanumeric() || prev == '_') && prev != 'b' {
            return None;
        }
    }
    let mut hashes = 0usize;
    while chars.get(i + 1 + hashes) == Some(&'#') {
        hashes += 1;
    }
    (chars.get(i + 1 + hashes) == Some(&'"')).then_some(hashes)
}

/// The length in `char`s of a char literal starting at `i`, if one does.
///
/// Why: `'` also opens a lifetime, and `'static` appears everywhere. Only the
/// literal forms are consumed, because only they can hide a `"` or a `/`.
fn char_literal_len(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&'\'') {
        return None;
    }
    if chars.get(i + 1) == Some(&'\\') {
        // An escape: scan to the closing quote, bounded so an unterminated
        // literal cannot run away.
        let end = (i + 2..chars.len().min(i + 12)).find(|k| chars[*k] == '\'')?;
        return Some(end - i + 1);
    }
    (chars.get(i + 2) == Some(&'\'')).then_some(3)
}

/// Every `.rs` file in this crate's lib target, this file excluded.
fn lib_target_sources() -> Vec<PathBuf> {
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
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    assert!(
        out.len() > 100,
        "the lib-target scan found only {} files — the walk is broken, and a broken walk \
         reports a clean target regardless of its contents",
        out.len()
    );
    out.sort();
    out
}

/// Path relative to the crate root, in `/` form, for table matching and
/// reporting.
fn relative(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The ratchet — the offender set is exactly [`SERIAL_WITHOUT_ENV_LOCK`].
///
/// Why: see the module docs. Both directions are enforced: a new offender is
/// the regression, and a listed file that became compliant is a stale row that
/// would otherwise let a future regression back in under cover.
/// What: classifies every lib-target source and compares the offender set
/// against the table by path suffix.
/// Test: this function IS the test;
/// `the_ratchet_detects_a_serial_mutation_without_env_lock` proves the
/// detection underneath it fires.
#[test]
fn serial_env_mutating_files_take_env_lock() {
    let mut unlisted: Vec<String> = Vec::new();
    let mut matched: Vec<&str> = Vec::new();
    for path in lib_target_sources() {
        let rel = relative(&path);
        if classify(std::fs::read_to_string(&path).ok().as_deref()) != Verdict::Offender {
            continue;
        }
        match SERIAL_WITHOUT_ENV_LOCK
            .iter()
            .find(|(suffix, _)| rel.ends_with(suffix))
        {
            Some((suffix, _)) => matched.push(suffix),
            None => unlisted.push(rel),
        }
    }
    let stale: Vec<&str> = SERIAL_WITHOUT_ENV_LOCK
        .iter()
        .map(|(suffix, _)| *suffix)
        .filter(|suffix| !matched.contains(suffix))
        .collect();

    assert!(
        unlisted.is_empty(),
        "these files mutate the process environment under a `serial_test` attribute without \
         taking `data_dir::ENV_LOCK` (#7253). `#[serial]` and `ENV_LOCK` are different mutexes \
         and exclude nothing of each other, so such a test still runs inside another test's \
         `setenv`. Take `ENV_LOCK` for the whole window the variable is changed — \
         `http_client.rs`'s `with_http_proxy` is the pattern. If a file genuinely cannot, add \
         it to `SERIAL_WITHOUT_ENV_LOCK` in this file with the reason.\n  {}",
        unlisted.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "`SERIAL_WITHOUT_ENV_LOCK` is stale — these rows name files that are now compliant (or \
         gone). Delete the rows so the ratchet keeps its grip.\n  {}",
        stale.join("\n  ")
    );
}

/// The detection fires on a violation it is shown, and not on prose.
///
/// Why: a scanner that silently matches nothing passes forever and protects
/// nothing. The failure mode this guard exists to prevent is exactly the
/// failure mode a broken guard hides, so the detection is proved on synthetic
/// source rather than inferred from a green run.
/// What: classifies four samples — the violation, the same file with the lock,
/// a file whose only mention of the lock is a comment, and a serial file that
/// touches no environment.
/// Test: this function IS the test.
#[test]
fn the_ratchet_detects_a_serial_mutation_without_env_lock() {
    let offender = concat!(
        "#[test]\n#[serial(dotenv_credential_env)]\n",
        "fn t() { unsafe { std::env::set_var(\"TRUSTY_X\", \"1\") } }\n",
    );
    assert_eq!(classify(Some(offender)), Verdict::Offender);

    let compliant = concat!(
        "#[test]\n#[serial(dotenv_credential_env)]\n",
        "fn t() {\n    let _g = crate::data_dir::ENV_LOCK.lock().unwrap();\n",
        "    unsafe { std::env::set_var(\"TRUSTY_X\", \"1\") }\n}\n",
    );
    assert_eq!(classify(Some(compliant)), Verdict::Compliant);

    let prose_only = concat!(
        "// This test would need ENV_LOCK if it mutated anything shared.\n",
        "#[test]\n#[serial]\nfn t() { unsafe { std::env::remove_var(\"TRUSTY_X\") } }\n",
    );
    assert_eq!(
        classify(Some(prose_only)),
        Verdict::Offender,
        "a comment naming ENV_LOCK is documentation, not the lock"
    );

    let guard_wrapper = "#[test]\n#[serial]\nfn t() { let _g = EnvVarGuard::remove(\"K\"); }\n";
    assert_eq!(
        classify(Some(guard_wrapper)),
        Verdict::Offender,
        "the crate's RAII env guard is an environment mutation"
    );

    let no_env = "#[test]\n#[serial]\nfn t() { assert!(true); }\n";
    assert_eq!(classify(Some(no_env)), Verdict::OutOfScope);

    let no_serial = "#[test]\nfn t() { unsafe { std::env::set_var(\"TRUSTY_X\", \"1\") } }\n";
    assert_eq!(
        classify(Some(no_serial)),
        Verdict::OutOfScope,
        "#7253 is about reaching for the wrong lock; reaching for none is a different defect"
    );
}

/// A file the guard cannot read counts as an offender.
///
/// Why: "I could not look" must never render as "nothing there". Fail closed.
/// Test: this function IS the test.
#[test]
fn an_unreadable_source_counts_as_an_offender() {
    assert_eq!(classify(None), Verdict::Offender);
}

/// Comments and string literals are removed; code survives.
///
/// Why: a URL literal contains `//`, so a stripper that does not know about
/// strings deletes the rest of that line — and `http_client.rs`, one of the two
/// files this rule was written for, is full of them. Deleting code is how a
/// fail-closed guard turns into a false green.
/// What: strips a sample carrying a URL, a nested block comment, a char literal
/// holding a quote, and a raw string, then asserts what survived.
/// Test: this function IS the test.
#[test]
fn the_stripper_removes_prose_and_string_literals() {
    let sample = concat!(
        "let url = \"http://127.0.0.1/health\"; set_var(\"A\", \"b\");\n",
        "/* outer /* inner mentions ENV_LOCK */ still comment */ let q = '\"';\n",
        "let raw = r#\"#[serial] set_var(\"#; let keep: &'static str = \"x\";\n",
        "// trailing prose about #[serial] and ENV_LOCK\n",
    );
    let code = strip_noncode(sample);

    assert!(
        contains_call(&code, "set_var"),
        "the real call after the URL literal must survive:\n{code}"
    );
    assert!(
        !code.contains(ENV_LOCK_IDENT),
        "every ENV_LOCK here is prose or a literal:\n{code}"
    );
    assert!(
        !uses_serial_attribute(&code),
        "the only #[serial] spellings here are inside a raw string and a comment:\n{code}"
    );
    assert!(
        code.contains("'static"),
        "a lifetime is code, not a char literal:\n{code}"
    );
}

/// The scanner does not panic on the crate's real sources.
///
/// Why: `strip_noncode` walks a `Vec<char>` with hand-rolled indices, and this
/// crate's sources carry box-drawing, emoji, nested block comments and raw
/// strings. A panic here would be a false red on a legitimate file.
/// Test: this function IS the test.
#[test]
fn the_scanner_survives_every_real_source() {
    for path in lib_target_sources() {
        let text = std::fs::read_to_string(&path).ok();
        let _ = classify(text.as_deref());
    }
}
