//! Source-scan guard: no credential read bypasses `secret_source.rs` (#9121).
//!
//! Why: the sandbox latch guarded `resolve_secret` only, while two daemon call
//! sites built their `Configurator` store with `default_store()` and a route
//! probed `resolve_env_var_bounded` directly — both reached the per-user
//! Keychain from a sandboxed daemon. A new direct read is one line anywhere in
//! the crate; this scan fails CI on it.
//! What: every production `.rs` file under `src/` is read with comments and
//! inline test modules removed. A file naming a credential-reading API must
//! be in [`ALLOWED`] for that API, with its reason. A file constructing a
//! `Configurator` must take the store from `secret_source::credential_store()`.
//! A stale allowlist entry fails too, so the list only shrinks.
//! Test: `no_credential_read_bypasses_secret_source`,
//! `the_scan_flags_a_direct_read`.

use std::path::{Path, PathBuf};

/// APIs that read `.env.local`, the credential store or the Keychain.
const CREDENTIAL_READS: &[&str] = &[
    "default_store(",
    "KeyringStore",
    "FileKeyStore",
    "resolve_env_var_bounded",
    "load_env_local_once",
    "resolve_key(",
    "resolve_key_with(",
    "resolve_provider_bounded",
    "resolved_secret_values",
    "credentials::resolve(",
    "credentials::resolve_client",
    "dotenvy::",
    "read_dotenv_key",
    "\"/usr/bin/security\"",
];

/// `(file under src/, API, reason)` — the only places a credential read may
/// bypass the latch. None of them runs inside the daemon process.
const ALLOWED: &[(&str, &str, &str)] = &[
    (
        "secret_source.rs",
        "*",
        "the one credential door; every read here is behind the sandbox latch",
    ),
    (
        "daemon/doctor_launchd_secrets_repair.rs",
        "default_store(",
        "`tm doctor --fix` CLI only; imports a plist credential INTO the store, \
         where an empty sandbox store would drop it",
    ),
    (
        "daemon/doctor_launchd_secrets_scoped.rs",
        "default_store(",
        "`tm doctor --fix-launchd-secrets` CLI only; same write path as above",
    ),
    (
        "bin/tm/commands/env_file.rs",
        "\"/usr/bin/security\"",
        "`tm env set` CLI only; reads its value from the login Keychain by design (#8939)",
    ),
    (
        "slack/mod.rs",
        "read_dotenv_key",
        "`tm slack` CLI only; the daemon never starts the Slack bot",
    ),
];

/// Files allowed to construct a `Configurator`, each over `credential_store()`.
const CONFIGURATOR_SITES: &[&str] = &["activity/classifier.rs", "daemon/manager/inference.rs"];

/// One finding: `(file, API or rule)`.
type Finding = (String, String);

/// Whether `rel` is a test file by the line-cap script's own rules.
fn is_test_file(rel: &str) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    base == "tests.rs"
        || base.ends_with("_tests.rs")
        || base.ends_with("_test.rs")
        || base == "test_support.rs"
        || base == "secret_source_test_env.rs"
        || rel.contains("/tests/")
        || rel.starts_with("tests/")
}

/// `text` without `//` comments and without a trailing inline test module.
fn code_only(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim() == "#[cfg(test)]" && opens_test_module(&lines[i + 1..]) {
            break;
        }
        let code = line.find("//").map_or(*line, |at| &line[..at]);
        out.push_str(code);
        out.push('\n');
    }
    out
}

/// Whether the lines after a `#[cfg(test)]` declare a module.
fn opens_test_module(rest: &[&str]) -> bool {
    rest.iter()
        .map(|l| l.trim())
        .find(|l| !l.starts_with("#["))
        .is_some_and(|l| l.starts_with("mod ") || l.starts_with("pub(crate) mod "))
}

/// Every finding in one file's production code.
fn scan_file(rel: &str, text: &str) -> Vec<Finding> {
    let code = code_only(text);
    let mut out: Vec<Finding> = CREDENTIAL_READS
        .iter()
        .filter(|api| code.contains(**api))
        .filter(|api| {
            !ALLOWED
                .iter()
                .any(|(file, allowed, _)| *file == rel && (*allowed == "*" || allowed == *api))
        })
        .map(|api| (rel.to_string(), (*api).to_string()))
        .collect();
    if code.contains("Configurator::new()")
        && !(CONFIGURATOR_SITES.contains(&rel)
            && code.contains("secret_source::credential_store()"))
    {
        out.push((
            rel.to_string(),
            "a Configurator store not taken from secret_source::credential_store()".into(),
        ));
    }
    out
}

/// Every production `.rs` file under `dir`, as `(path relative to src/, text)`.
fn production_sources(src: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = std::fs::read_dir(dir).expect("read a src/ directory");
    for entry in entries.filter_map(Result::ok) {
        let path: PathBuf = entry.path();
        if path.is_dir() {
            production_sources(src, &path, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let rel = path
            .strip_prefix(src)
            .expect("under src/")
            .to_string_lossy()
            .replace('\\', "/");
        if !is_test_file(&rel) {
            out.push((rel, std::fs::read_to_string(&path).expect("read source")));
        }
    }
}

/// Why/What: see the module docs.
/// Test: this test.
#[test]
fn no_credential_read_bypasses_secret_source() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    production_sources(&src, &src, &mut sources);
    assert!(
        sources.len() > 100,
        "the scan found only {} files",
        sources.len()
    );

    let findings: Vec<Finding> = sources
        .iter()
        .flat_map(|(rel, text)| scan_file(rel, text))
        .collect();
    assert!(
        findings.is_empty(),
        "#9121: a credential read bypasses secret_source.rs — route it through \
         secret_source::{{resolve_secret, resolve_bounded_gated, credential_store}}, \
         or add an ALLOWED entry with its reason if it can never run in the daemon:\n  {}",
        findings
            .iter()
            .map(|(file, api)| format!("{file}: {api}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );

    let stale: Vec<&str> = ALLOWED
        .iter()
        .filter(|(file, api, _)| {
            *api != "*"
                && !sources
                    .iter()
                    .any(|(rel, text)| rel == file && code_only(text).contains(*api))
        })
        .map(|(file, _, _)| *file)
        .collect();
    assert!(
        stale.is_empty(),
        "these ALLOWED entries no longer match; delete them: {stale:?}"
    );
}

/// Why: a scan that cannot fail proves nothing. A direct read in an
/// unlisted file, a `Configurator` over another store, and a read hidden after
/// a comment marker are each judged as the rules say.
/// Test: this test.
#[test]
fn the_scan_flags_a_direct_read() {
    let direct = "fn f() { let s = default_store(); }\n";
    assert_eq!(
        scan_file("daemon/new_route.rs", direct),
        vec![("daemon/new_route.rs".into(), "default_store(".into())]
    );

    let other_store = "let c = Configurator::new(); c.build(m, &MemoryKeyStore::new());\n";
    assert_eq!(scan_file("activity/classifier.rs", other_store).len(), 1);

    let commented = "// default_store() is not called here\n";
    assert!(scan_file("daemon/new_route.rs", commented).is_empty());

    let test_module = "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn g() { default_store(); }\n}\n";
    assert!(scan_file("daemon/new_route.rs", test_module).is_empty());
}
