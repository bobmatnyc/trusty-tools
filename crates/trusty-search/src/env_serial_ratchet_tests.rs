//! Mechanical guard (#5937): a `src/**` test that writes the process
//! environment must run in the unnamed `#[serial]` group.
//!
//! Why: #5937's hang was a race between a test that set an env var and a test
//! that read it under a different lock. This crate serialises env access with
//! `serial_test`'s unnamed group. A NAMED key (`#[serial(env_workers)]`) or a
//! private mutex is a separate lock that excludes nothing of that group.
//! What: over `src/**` test code, a file that calls `set_var`, `remove_var` or
//! `set_current_dir` must carry an unnamed `#[serial]` /
//! `#[serial_test::serial]` attribute and no named `serial` key. Files that
//! cannot are listed in [`WRITERS_OUTSIDE_SERIAL`] with a reason. A new
//! offender fails, and so does a stale row.
//!
//! Limits: the unit is the FILE, as in trusty-common's ratchet. `tests/**`
//! binaries are separate processes and outside this scan. It checks writers
//! only: a test that READS the data dir (`persistence::data_dir`, the daemon
//! dir) without writing it takes `#[serial_test::parallel]`, which excludes
//! the writers but not other readers (#9233).
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

/// Test files that write the environment outside the unnamed group, each
/// with its reason. `(path suffix, reason)`.
const WRITERS_OUTSIDE_SERIAL: &[(&str, &str)] = &[];

/// This file's own basename, skipped so its literals do not match it.
const SELF_BASENAME: &str = "env_serial_ratchet_tests.rs";

/// Calls that write the process environment.
const ENV_WRITE_CALLS: &[&str] = &["set_var", "remove_var", "set_current_dir"];

/// Unnamed spellings of the group.
const UNNAMED_SERIAL: &[&str] = &["#[serial]", "#[serial_test::serial]"];

/// Named spellings, each of which starts a separate lock.
const NAMED_SERIAL: &[&str] = &["#[serial(", "#[serial_test::serial("];

/// Is this stripped file an offender?
///
/// What: its test code writes env and either carries no unnamed `#[serial]`
/// or carries a named key.
/// Test: `the_ratchet_detects_a_writer_outside_the_unnamed_group`.
fn is_offender(rel: &str, code: &str) -> bool {
    let tests = test_code(rel, code);
    if !writes_env(tests) {
        return false;
    }
    let squashed: String = tests.split_whitespace().collect();
    let unnamed = UNNAMED_SERIAL.iter().any(|a| squashed.contains(a));
    let named = NAMED_SERIAL.iter().any(|a| squashed.contains(a));
    !unnamed || named
}

/// The ratchet — the offender set is exactly [`WRITERS_OUTSIDE_SERIAL`].
///
/// Test: this function IS the test;
/// `the_ratchet_detects_a_writer_outside_the_unnamed_group` proves the
/// detection.
#[test]
fn env_writing_tests_use_the_unnamed_serial_group() {
    let mut unlisted = Vec::new();
    let mut matched = Vec::new();
    for path in lib_sources() {
        let rel = relative(&path);
        let offender = match std::fs::read_to_string(&path) {
            Ok(text) => is_offender(&rel, &strip_noncode(&text)),
            Err(_) => true,
        };
        if !offender {
            continue;
        }
        match WRITERS_OUTSIDE_SERIAL
            .iter()
            .find(|(suffix, _)| rel.ends_with(suffix))
        {
            Some((suffix, _)) => matched.push(*suffix),
            None => unlisted.push(rel),
        }
    }
    let stale: Vec<&str> = WRITERS_OUTSIDE_SERIAL
        .iter()
        .map(|(suffix, _)| *suffix)
        .filter(|suffix| !matched.contains(suffix))
        .collect();
    assert!(
        unlisted.is_empty(),
        "these files write the process environment in test code outside the unnamed \
         `#[serial]` group (#5937). A named key or a private mutex is a separate lock, so the \
         write races every test that reads the variable under `#[serial]`. Put the writer in \
         `#[serial]`, or add the file to `WRITERS_OUTSIDE_SERIAL` with the reason.\n  {}",
        unlisted.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "`WRITERS_OUTSIDE_SERIAL` is stale — delete these rows:\n  {}",
        stale.join("\n  ")
    );
}

/// The detection fires on unserialised and split-group writers, not on prose.
///
/// Test: this function IS the test.
#[test]
fn the_ratchet_detects_a_writer_outside_the_unnamed_group() {
    let check = |rel: &str, s: &str| is_offender(rel, &strip_noncode(s));

    let unserialised = "#[test]\nfn t() { unsafe { std::env::set_var(\"K\", \"1\") } }\n";
    assert!(check("src/a_tests.rs", unserialised));

    let private_mutex = concat!(
        "static ENV_LOCK: Mutex<()> = Mutex::new(());\n",
        "#[test]\nfn t() { let _g = ENV_LOCK.lock(); unsafe { std::env::remove_var(\"K\") } }\n",
    );
    assert!(
        check("src/a_tests.rs", private_mutex),
        "a private mutex is not the group"
    );

    let named = "#[test]\n#[serial_test::serial(env_workers)]\nfn t() { unsafe { std::env::set_var(\"K\", \"1\") } }\n";
    assert!(
        check("src/a_tests.rs", named),
        "a named key splits the group"
    );

    let unnamed = "#[test]\n#[serial]\nfn t() { unsafe { std::env::set_var(\"K\", \"1\") } }\n";
    assert!(!check("src/a_tests.rs", unnamed));

    let prose = "// #[serial] then set_var(\"K\")\nfn f() {}\n";
    assert!(!check("src/a_tests.rs", prose));

    let production = concat!(
        "fn load() { unsafe { std::env::set_var(\"K\", \"v\") } }\n",
        "#[cfg(test)]\nmod tests { #[test] fn t() {} }\n",
    );
    assert!(
        !check("src/daemon.rs", production),
        "a production write above `#[cfg(test)]` is not a test's"
    );
}

/// Every `.rs` file under `src/`, this file excluded.
fn lib_sources() -> Vec<PathBuf> {
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
        "the scan found only {} files — a broken walk reports a clean crate",
        out.len()
    );
    out.sort();
    out
}

/// Path relative to the crate root, in `/` form.
fn relative(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The part of a stripped source that is test code.
///
/// What: the whole file when its path names it a test file (a `test` in the
/// basename or a `tests` directory); otherwise everything from the first
/// `#[cfg(test)]` on, so a production `set_var` above it is not read as a
/// test's. Code after a `#[cfg(test)]` counts even if it is production — the
/// fail-closed direction.
fn test_code<'a>(rel: &str, code: &'a str) -> &'a str {
    let basename = rel.rsplit('/').next().unwrap_or(rel);
    if basename.contains("test") || rel.split('/').any(|seg| seg == "tests") {
        return code;
    }
    code.find("#[cfg(test)]").map_or("", |at| &code[at..])
}

/// Does the stripped test code write the process environment?
fn writes_env(code: &str) -> bool {
    ENV_WRITE_CALLS.iter().any(|c| contains_call(code, c))
}

/// Is `name` called anywhere in `code` (`name`, optional whitespace, `(`)?
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

/// Remove comments, string literals and char literals, leaving code.
///
/// Why: files discuss `set_var` and the lock at length in prose; a comment
/// must never read as a write or as compliance. Copied from trusty-common's
/// `env_lock_ratchet_tests::strip_noncode`, which carries its proofs — that
/// module is `#[cfg(test)]` there and unreachable from here.
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
fn raw_string_open(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&'r') {
        return None;
    }
    if i > 0 {
        let prev = chars[i - 1];
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

/// The length of a char literal starting at `i`; a lifetime is not one.
fn char_literal_len(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&'\'') {
        return None;
    }
    if chars.get(i + 1) == Some(&'\\') {
        let end = (i + 2..chars.len().min(i + 12)).find(|k| chars[*k] == '\'')?;
        return Some(end - i + 1);
    }
    (chars.get(i + 2) == Some(&'\'')).then_some(3)
}
