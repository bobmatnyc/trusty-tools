//! Source-scan guard: no credential read bypasses `secret_source.rs` (#9121).
//!
//! Why: the sandbox latch guarded `resolve_secret` only, while two daemon call
//! sites built their `Configurator` store with `default_store()` and a route
//! probed `resolve_env_var_bounded` directly — both reached the per-user
//! Keychain from a sandboxed daemon. A new direct read is one line anywhere in
//! the crate; this scan fails CI on it.
//! What: every production `.rs` file under `src/` is lexed, with comments
//! removed string-aware and each `#[cfg(test)]` module cut out — a `mod x;`
//! declaration alone, an inline `mod x { … }` exactly to its closing brace —
//! so all other code in the file is scanned. A file naming a credential-reading
//! API, as an identifier and so at its import site too, must be in [`ALLOWED`]
//! for that API, with its reason. A file naming `Configurator` must be a
//! [`CONFIGURATOR_SITES`] entry taking its store from
//! `secret_source::credential_store()`. A stale entry in either list fails
//! too, so the lists only shrink.
//! Test: `no_credential_read_bypasses_secret_source`,
//! `the_scan_flags_a_direct_read`,
//! `the_scan_reads_past_test_modules_aliases_and_urls`.

use std::path::{Path, PathBuf};

#[path = "source_scan_lex.rs"]
mod lex;
use lex::{code_only, is_ident};

/// APIs that read `.env.local`, the credential store or the Keychain. Each
/// matches where an identifier starts, so `default_store` also catches its
/// import, a `use … as` alias and `default_store_with`, never `my_default_store`.
const CREDENTIAL_READS: &[&str] = &[
    "default_store",
    "KeyringStore",
    "FileKeyStore",
    "resolve_env_var_bounded",
    "resolve_provider_bounded",
    "store_get_bounded",
    "load_env_local_once",
    "load_env_from_path",
    "env_local_value",
    "read_var_from_env_local",
    "resolve_key",
    "resolved_secret_values",
    "credentials::resolve",
    "dotenvy",
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
        "default_store",
        "`tm doctor --fix` CLI only; imports a plist credential INTO the store, \
         where an empty sandbox store would drop it",
    ),
    (
        "daemon/doctor_launchd_secrets_scoped.rs",
        "default_store",
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

/// Whether `code` names `api` where an identifier starts (#9121).
fn names(code: &str, api: &str) -> bool {
    let bounded = api.chars().next().is_some_and(is_ident);
    code.match_indices(api)
        .any(|(at, _)| !bounded || !code[..at].chars().next_back().is_some_and(is_ident))
}

/// Whether `code` names `word` as a whole identifier.
fn names_word(code: &str, word: &str) -> bool {
    code.match_indices(word).any(|(at, _)| {
        !code[..at].chars().next_back().is_some_and(is_ident)
            && !code[at + word.len()..].chars().next().is_some_and(is_ident)
    })
}

/// Every finding in one file's production code.
fn scan_file(rel: &str, text: &str) -> Vec<Finding> {
    let code = code_only(text);
    let mut out: Vec<Finding> = CREDENTIAL_READS
        .iter()
        .filter(|api| names(&code, api))
        .filter(|api| {
            !ALLOWED
                .iter()
                .any(|(file, allowed, _)| *file == rel && (*allowed == "*" || allowed == *api))
        })
        .map(|api| (rel.to_string(), (*api).to_string()))
        .collect();
    // #9121: the bare type name, so an aliased import is caught too.
    if names_word(&code, "Configurator")
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
    let entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read src/ directory {}: {e}", dir.display()))
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("read an entry of {}: {e}", dir.display()));
    for entry in entries {
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

    let code_of = |file: &str| {
        sources
            .iter()
            .find(|(rel, _)| rel == file)
            .map(|(_, text)| code_only(text))
    };
    let mut stale: Vec<&str> = ALLOWED
        .iter()
        .filter(|(file, api, _)| *api != "*" && !code_of(file).is_some_and(|c| names(&c, api)))
        .map(|(file, _, _)| *file)
        .collect();
    stale.extend(
        CONFIGURATOR_SITES
            .iter()
            .filter(|file| !code_of(file).is_some_and(|c| names_word(&c, "Configurator"))),
    );
    assert!(
        stale.is_empty(),
        "these allowlist entries no longer match; delete them: {stale:?}"
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
        vec![("daemon/new_route.rs".into(), "default_store".into())]
    );

    let other_store = "let c = Configurator::new(); c.build(m, &MemoryKeyStore::new());\n";
    assert_eq!(scan_file("activity/classifier.rs", other_store).len(), 1);

    let commented = "// default_store() is not called here\n/* nor default_store() */\n";
    assert!(scan_file("daemon/new_route.rs", commented).is_empty());

    let test_module = "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn g() { default_store(); }\n}\n";
    assert!(scan_file("daemon/new_route.rs", test_module).is_empty());

    let declared = "#[cfg(test)]\n#[path = \"x_tests.rs\"]\npub(crate) mod tests;\nfn f() {}\n";
    assert_eq!(code_only(declared).trim(), "fn f() {}");

    let near_miss = "fn my_default_store() {}\n";
    assert!(scan_file("daemon/new_route.rs", near_miss).is_empty());
}

/// Why (#9121): blind spots of the first scan — code after a `mod tests;`
/// declaration, an aliased import, a URL literal whose `//` hid the rest of
/// its line, and a brace inside an inline test module's string — each let a
/// direct read through. The lexer edge cases below each guard one rule of
/// `lex` and its helpers: a fixture goes red if that rule is removed.
/// Test: this test.
#[test]
fn the_scan_reads_past_test_modules_aliases_and_urls() {
    let missed: Vec<&str> = [
        "#[cfg(test)]\nmod tests;\nfn f() { default_store(); }",
        "use trusty_common::credentials::default_store as ds;\nfn f() { ds(); }",
        "fn f() { let u = \"https://x\"; default_store(); }",
        "#[cfg(test)]\nmod tests {\n    fn g() { let b = \"}\"; let c = '}'; }\n}\nfn f() { default_store(); }",
        // string_end: a `"{"` / `'{'` in a test module must not stretch the cut
        // over the production call after it (a stray `}` closes a wrong cut).
        "#[cfg(test)]\nmod t { fn g() { let s = \"{\"; let c = '{'; } }\nfn f() { default_store(); }\n}",
        "#[cfg(test)]\nmod t { fn g() { let s = \"{\"; let c = '{'; } }\nfn f() { default_store(); }",
        "fn f() { let u = r#\"a \" // b\"#; default_store(); }",
        // raw_string_start, `b` prefix: `br#"…"#` is a raw string, `//` inside it
        // must not hide the rest of the line.
        "fn f() { let u = br#\"a \" // b\"#; default_store(); }",
        // block_comment_end: nested comment closes at its own `*/`.
        "/* /* */ */ fn f() { default_store(); }",
        "use trusty_common::inference::Configurator as C;\nfn f() { C::new(); }",
    ]
    .into_iter()
    .filter(|fixture| scan_file("daemon/new_route.rs", fixture).is_empty())
    .collect();
    assert!(missed.is_empty(), "not flagged: {missed:#?}");

    // Each of these hides the call inside a comment or a test module; a lexer
    // rule that fails leaves it visible and the scan flags it.
    let over_flagged: Vec<&str> = [
        // raw_string_start, `#` then no quote: `r#type` is an identifier, not a
        // raw string swallowing the test module's closing brace.
        "#[cfg(test)]\nmod t { fn g() { let r#type = 1; default_store(); } }\nfn f() {}",
        // block_comment_end: depth tracking — a nested `/* */` stays one comment.
        "/* a /* b */ default_store(); */ fn f() {}",
        // string_end: `{` in a string does not open a brace.
        "#[cfg(test)]\nmod t { fn g() { let s = \"{\"; default_store(); } }\nfn f() {}",
        // char_literal_end: `'{'` is a char, not a brace.
        "#[cfg(test)]\nmod t { fn g() { let c = '{'; default_store(); } }\nfn f() {}",
        // string_end: an escaped quote does not end the string.
        "#[cfg(test)]\nmod t { fn g() { let s = \"\\\"}\"; } fn k() { default_store(); } }",
        // char_literal_end: `'a` is a lifetime, not a char literal that
        // swallows the `{` up to the next `'`.
        "#[cfg(test)]\nmod t { fn g<'a>() { let x: &'a u8 = &0; } fn k() { default_store(); } }",
        "#[cfg(test)]\nmod t { fn g() { 'outer: loop { break 'outer; } } fn k() { default_store(); } }",
    ]
    .into_iter()
    .filter(|fixture| !scan_file("daemon/new_route.rs", fixture).is_empty())
    .collect();
    assert!(
        over_flagged.is_empty(),
        "wrongly flagged: {over_flagged:#?}"
    );
}
