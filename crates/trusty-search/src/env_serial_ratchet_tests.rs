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
//!
//! A second ratchet (#9684) covers the registry. A test that reaches
//! `with_registry_write_lock` resolves `indexes.toml` from `TRUSTY_DATA_DIR`
//! at write time, so an untagged one can drop `indexes.toml.lock` into the
//! data dir a `#[serial]` test has just pointed somewhere else. Every test in
//! a file that names a [`REGISTRY_REACHING`] function carries a group tag, and
//! so does every test in a module that file mounts (`#[path]` or `mod x;`),
//! because a child reaches the registry through its parent's harness.
//! Test: this file IS the test module.

use std::collections::{BTreeMap, BTreeSet};
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

/// Functions whose call reaches `with_registry_write_lock` on a path resolved
/// from `TRUSTY_DATA_DIR` at write time (#9684).
///
/// Why: the reindex finish (`persist_indexed_head_sha`), the reconcile stamp,
/// the first-query stamp (`update_last_queried_unix`), the registration and
/// delete handlers and the boot restore paths all patch `indexes.toml` through
/// `data_dir()`. The socket's `build_router` (the `RpcRouter`) reaches every
/// method.
/// What: matched as whole identifiers in test code, so a call, a `use` and a
/// path all count; `upsert_index_registry_entry_at` does not count as
/// `upsert_index_registry_entry`, because an explicit path never reads env.
const REGISTRY_REACHING: &[&str] = &[
    "spawn_reindex_awaitable",
    "spawn_reindex",
    "spawn_claimed_reindex",
    "spawn_reindex_with_cleanup",
    "run_reindex",
    "finish_reindex",
    "reindex_report",
    "reconcile_one_index",
    "stamp_handle",
    "persist_indexed_head_sha",
    "persist_deferred_embed_pending",
    "rearm_deferred_embed_if_pending",
    "resolve_indexed_head_sha",
    "run_embed_catch_up",
    "spawn_deferred_embed_pass",
    "update_last_queried_unix",
    "update_last_indexed_unix",
    "search_handler",
    "search_report",
    "create_index_handler",
    "create_index_report",
    "delete_index_handler",
    "delete_index_report",
    "unregister_index",
    "unregister_with_map",
    "relocate_index_handler",
    "relocate_index_report",
    "add_index_roots_report",
    "patch_index_config_report",
    "restore_index_on_demand",
    "resolve_or_load_index",
    "restore_one_index",
    "restore_indexes",
    "heal_boot_orphans",
    "handle_ambiguous_root",
    "clear_ambiguous_root_stamp",
    "reap_orphans_once",
    "handle_migrate_storage",
    "upsert_index_registry_entry",
    "save_index_registry",
    "remove_index_registry_entry",
    "remove_index_registry_entries",
    "build_router",
];

/// Attributes that put a test in the unnamed `serial_test` group.
const GROUP_TAGS: &[&str] = &[
    "#[serial]",
    "#[serial_test::serial]",
    "#[parallel]",
    "#[serial_test::parallel]",
];

/// Attribute prefixes that open a test function.
const TEST_ATTRS: &[&str] = &["#[test]", "#[tokio::test]", "#[tokio::test("];

/// The ratchet — no test that can reach the registry runs outside the group.
///
/// Test: this function IS the test;
/// `the_registry_ratchet_detects_an_untagged_test` proves the detection.
#[test]
fn registry_reaching_tests_join_the_unnamed_group() {
    let files: BTreeMap<String, String> = lib_sources()
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            (relative(&path), text)
        })
        .collect();
    let offenders = registry_offenders(&files);
    assert!(
        offenders.is_empty(),
        "these tests sit in a file that reaches the index registry, and carry no \
         `#[serial_test::parallel]` / `#[serial_test::serial]` tag (#9684). An untagged test \
         runs beside a `#[serial]` test that repoints `TRUSTY_DATA_DIR`, and its registry write \
         lands `indexes.toml.lock` in that test's data dir. Tag each one `parallel`, or \
         `serial` when it writes the environment.\n  {}",
        offenders.join("\n  ")
    );
}

/// The detection fires on an untagged or named-key test in a file that names
/// a registry-reaching function, and on nothing else.
///
/// Test: this function IS the test.
#[test]
fn the_registry_ratchet_detects_an_untagged_test() {
    let check = |rel: &str, s: &str| untagged_registry_tests(rel, &strip_noncode(s));

    let untagged = "use super::spawn_reindex_awaitable;\n#[tokio::test]\nasync fn t() {}\n";
    assert_eq!(check("src/a_tests.rs", untagged), ["t"]);

    let mixed = concat!(
        "fn h() { search_report(); }\n",
        "#[test]\n#[serial_test::parallel]\nfn ok_parallel() {}\n",
        "#[tokio::test(flavor = \"multi_thread\")]\n#[serial]\nasync fn ok_serial() {}\n",
        "#[test]\n#[serial_test::serial(watcher_start)]\nfn named_key() {}\n",
        "#[test]\nfn bare() {}\n",
    );
    assert_eq!(check("src/a_tests.rs", mixed), ["bare", "named_key"]);

    let explicit_path = "fn h() { upsert_index_registry_entry_at(p, e); }\n#[test]\nfn t() {}\n";
    assert!(
        check("src/a_tests.rs", explicit_path).is_empty(),
        "an explicit registry path never reads the environment"
    );

    let prose = "// calls search_report\n#[test]\nfn t() {}\n";
    assert!(
        check("src/a_tests.rs", prose).is_empty(),
        "a comment is not a reach"
    );

    let production = concat!(
        "fn serve() { search_report(); }\n",
        "#[cfg(test)]\nmod tests { #[test] fn t() {} }\n",
    );
    assert!(
        check("src/search.rs", production).is_empty(),
        "a production call above `#[cfg(test)]` is not a test's"
    );
}

/// A child module that names no token itself is still in scope (#9684).
///
/// Why: `writes_9510_tests.rs` and `writes_7434_tests.rs` are `#[path]`
/// children of `writes_tests.rs` and reach the registry through its harness
/// (`use super::*`), so a per-file token scan never saw their untagged tests.
/// Test: this function IS the test.
#[test]
fn the_registry_ratchet_follows_child_modules() {
    let files: BTreeMap<String, String> = [
        (
            "src/a_tests.rs",
            concat!(
                "fn h() { search_report(); }\n",
                "#[test]\n#[serial_test::parallel]\nfn ok_parent() {}\n",
                "#[path = \"b_tests.rs\"]\nmod b;\n",
                "mod c;\n",
            ),
        ),
        (
            "src/b_tests.rs",
            "use super::*;\n#[tokio::test]\nasync fn path_child() {}\n#[path = \"../d_tests.rs\"]\nmod d;\n",
        ),
        ("src/a_tests/c.rs", "#[test]\nfn mod_child() {}\n"),
        ("d_tests.rs", "#[test]\nfn grandchild() {}\n"),
        ("src/e_tests.rs", "#[test]\nfn unrelated() {}\n"),
        (
            "src/prod.rs",
            concat!(
                "pub mod inner;\n",
                "#[cfg(test)]\n#[path = \"prod_tests.rs\"]\nmod tests;\n",
                "#[cfg(test)]\nfn h() { search_report(); }\n",
            ),
        ),
        ("src/prod/inner.rs", "#[test]\nfn production_child() {}\n"),
        ("src/prod_tests.rs", "#[test]\nfn cfg_test_child() {}\n"),
    ]
    .into_iter()
    .map(|(rel, text)| (rel.to_owned(), text.to_owned()))
    .collect();
    assert_eq!(
        registry_offenders(&files),
        [
            "d_tests.rs: grandchild",
            "src/a_tests/c.rs: mod_child",
            "src/b_tests.rs: path_child",
            "src/prod_tests.rs: cfg_test_child",
        ],
        "a production module is not a test child"
    );
}

/// `rel: test, test` for every untagged test in a file that reaches the
/// registry or is a child module of one, sorted by path.
///
/// What: the seed set is every file whose test code names a
/// [`REGISTRY_REACHING`] function; [`child_modules`] then widens it to every
/// module those files mount, transitively. `files` maps crate-relative path
/// to source text.
/// Test: `the_registry_ratchet_follows_child_modules`.
fn registry_offenders(files: &BTreeMap<String, String>) -> Vec<String> {
    let stripped: BTreeMap<&str, String> = files
        .iter()
        .map(|(rel, text)| (rel.as_str(), strip_noncode(text)))
        .collect();
    let mut queue: Vec<&str> = stripped
        .iter()
        .filter(|(rel, code)| reaches_registry(rel, code))
        .map(|(rel, _)| *rel)
        .collect();
    let mut scope = BTreeSet::new();
    while let Some(rel) = queue.pop() {
        if !scope.insert(rel) {
            continue;
        }
        for child in child_modules(rel, &files[rel]) {
            if let Some((key, _)) = files.get_key_value(&child) {
                queue.push(key.as_str());
            }
        }
    }
    scope
        .into_iter()
        .filter_map(|rel| {
            let untagged = untagged_tests(&stripped[rel]);
            (!untagged.is_empty()).then(|| format!("{rel}: {}", untagged.join(", ")))
        })
        .collect()
}

/// Crate-relative paths of the test-only module files `rel` mounts.
///
/// What: a `mod <name>;` counts when `rel` is a test file or the declaration
/// carries a `cfg(test)` attribute; a production module is never a child. A
/// `#[path = "<p>"]` attribute resolves `<p>` against `rel`'s directory; a
/// bare declaration resolves to `<name>.rs` and `<name>/mod.rs` under `rel`'s
/// module directory, and the caller keeps the candidates that exist.
/// Line-based on raw text, because [`strip_noncode`] erases the path string.
/// Test: `the_registry_ratchet_follows_child_modules`.
fn child_modules(rel: &str, text: &str) -> Vec<String> {
    let (dir, file) = rel.rsplit_once('/').unwrap_or(("", rel));
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    let mod_dir = if matches!(file, "mod.rs" | "lib.rs" | "main.rs") {
        dir.to_owned()
    } else {
        join(dir, stem)
    };
    let test_file = file.contains("test") || rel.split('/').any(|seg| seg == "tests");
    let mut path: Option<&str> = None;
    let mut cfg_test = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        if rest.starts_with("//") {
            continue;
        }
        while let Some(attr) = rest.strip_prefix("#[") {
            let Some(end) = attr.find(']') else { break };
            let body = &attr[..end];
            if let Some(p) = body.strip_prefix("path") {
                path = p.split('"').nth(1);
            }
            cfg_test |= body.starts_with("cfg(") && names_identifier(body, "test");
            rest = attr[end + 1..].trim_start();
        }
        if rest.is_empty() {
            continue;
        }
        let decl = rest
            .strip_prefix("pub(crate) ")
            .or_else(|| rest.strip_prefix("pub(super) "))
            .or_else(|| rest.strip_prefix("pub "))
            .unwrap_or(rest);
        let name = decl.strip_prefix("mod ").and_then(|d| d.strip_suffix(';'));
        if let Some(name) = name.filter(|_| test_file || cfg_test) {
            match path {
                Some(p) => out.push(join(dir, p)),
                None => {
                    out.push(join(&mod_dir, &format!("{}.rs", name.trim())));
                    out.push(join(&mod_dir, &format!("{}/mod.rs", name.trim())));
                }
            }
        }
        path = None;
        cfg_test = false;
    }
    out
}

/// `base/path`, with `.` and `..` segments folded.
fn join(base: &str, path: &str) -> String {
    let mut segs: Vec<&str> = Vec::new();
    for seg in base.split('/').chain(path.split('/')) {
        match seg {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            _ => segs.push(seg),
        }
    }
    segs.join("/")
}

/// Does this stripped file's test code name a [`REGISTRY_REACHING`] function?
fn reaches_registry(rel: &str, code: &str) -> bool {
    let tests = test_code(rel, code);
    REGISTRY_REACHING
        .iter()
        .any(|name| names_identifier(tests, name))
}

/// Names of the tests in a stripped file that can reach the registry and
/// carry no unnamed-group tag; empty when the file names no reaching function.
///
/// Test: `the_registry_ratchet_detects_an_untagged_test`.
fn untagged_registry_tests(rel: &str, code: &str) -> Vec<String> {
    if !reaches_registry(rel, code) {
        return Vec::new();
    }
    untagged_tests(test_code(rel, code))
}

/// Names of the tests in stripped `tests` that carry no unnamed-group tag.
///
/// What: a test's attribute block runs from the item boundary (`{`, `}`, `;`)
/// before its test attribute to its `fn`; the block must hold a
/// [`GROUP_TAGS`] entry. A named key is a separate lock and does not count.
/// Test: `the_registry_ratchet_detects_an_untagged_test`.
fn untagged_tests(tests: &str) -> Vec<String> {
    let mut out = Vec::new();
    for attr in TEST_ATTRS {
        for (at, _) in tests.match_indices(attr) {
            let Some((fn_at, name)) = next_fn(tests, at) else {
                continue;
            };
            let start = tests[..at].rfind(['{', '}', ';']).map_or(0, |i| i + 1);
            let block: String = tests[start..fn_at].split_whitespace().collect();
            if !GROUP_TAGS.iter().any(|tag| block.contains(tag)) {
                out.push(name.to_owned());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Does `code` hold `name` as a whole identifier?
fn names_identifier(code: &str, name: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    code.match_indices(name).any(|(at, _)| {
        let before = code[..at].chars().next_back().is_none_or(|c| !is_ident(c));
        let after = code[at + name.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident(c));
        before && after
    })
}

/// The byte offset and name of the first `fn <name>` at or after `from`.
fn next_fn(code: &str, from: usize) -> Option<(usize, &str)> {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    code[from..].match_indices("fn").find_map(|(rel, _)| {
        let at = from + rel;
        let before_ok = code[..at].chars().next_back().is_none_or(|c| !is_ident(c));
        let rest = &code[at + 2..];
        let name_start = rest.len() - rest.trim_start().len();
        if !before_ok || name_start == 0 {
            return None;
        }
        let name = &rest[name_start..];
        let len = name.find(|c: char| !is_ident(c)).unwrap_or(name.len());
        (len > 0).then(|| (at, &name[..len]))
    })
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
