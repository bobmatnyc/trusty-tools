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
];

/// Files allowed to construct a `Configurator`, each over `credential_store()`.
const CONFIGURATOR_SITES: &[&str] = &["activity/classifier.rs", "daemon/manager/inference.rs"];

/// One finding: `(file, API or rule)`.
type Finding = (String, String);

/// One source char and whether it is code (`true`) or inside a literal.
type Lexed = (char, bool);

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

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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

/// `text` as scanned: comments removed and every `#[cfg(test)]` module cut
/// out — the declaration alone, or an inline body to its closing brace. Code
/// after a test module stays (#9121).
fn code_only(text: &str) -> String {
    let lexed = lex(text);
    let mut out = String::with_capacity(lexed.len());
    let mut i = 0;
    while i < lexed.len() {
        if let Some(end) = test_module_end(&lexed, i) {
            i = end;
            continue;
        }
        out.push(lexed[i].0);
        i += 1;
    }
    out
}

/// `text` without comments, each char tagged code or literal, so a `//` or a
/// brace inside a string or char literal is never read as syntax (#9121).
fn lex(text: &str) -> Vec<Lexed> {
    let s: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let next = s.get(i + 1).copied();
        let end = match (s[i], next) {
            ('/', Some('/')) => {
                let end = s[i..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(s.len(), |n| i + n);
                i = end;
                continue;
            }
            ('/', Some('*')) => {
                i = block_comment_end(&s, i);
                out.push((' ', true));
                continue;
            }
            ('"', _) => Some(string_end(&s, i + 1, 0)),
            ('r', _) => raw_string_start(&s, i).map(|(hashes, body)| string_end(&s, body, hashes)),
            ('\'', _) => char_literal_end(&s, i),
            _ => None,
        };
        match end {
            Some(end) => {
                out.extend(s[i..end].iter().map(|&c| (c, false)));
                i = end;
            }
            None => {
                out.push((s[i], true));
                i += 1;
            }
        }
    }
    out
}

/// The index past the `*/` closing the (nestable) block comment at `at`.
fn block_comment_end(s: &[char], at: usize) -> usize {
    let mut depth = 0usize;
    let mut i = at;
    while i + 1 < s.len() {
        match (s[i], s[i + 1]) {
            ('/', '*') => depth += 1,
            ('*', '/') => {
                depth -= 1;
                if depth == 0 {
                    return i + 2;
                }
            }
            _ => {
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    s.len()
}

/// The index past the quote closing a string whose body starts at `body`.
/// `hashes` is the raw-string `#` count; a raw string has no escapes.
fn string_end(s: &[char], body: usize, hashes: usize) -> usize {
    let mut i = body;
    while i < s.len() {
        if hashes == 0 && s[i] == '\\' {
            i += 2;
            continue;
        }
        if s[i] == '"' && (1..=hashes).all(|n| s.get(i + n) == Some(&'#')) {
            return i + 1 + hashes;
        }
        i += 1;
    }
    s.len()
}

/// For a raw string `r#"…"#` (or `br"…"`) at `at`: its `#` count and the index
/// where its body starts. A raw identifier like `r#type` is not one.
fn raw_string_start(s: &[char], at: usize) -> Option<(usize, usize)> {
    let before = at.checked_sub(1).map(|p| s[p]);
    let prefixed = match before {
        Some('b') => !at.checked_sub(2).is_some_and(|p| is_ident(s[p])),
        Some(c) => !is_ident(c),
        None => true,
    };
    if !prefixed {
        return None;
    }
    let hashes = s[at + 1..].iter().take_while(|&&c| c == '#').count();
    (s.get(at + 1 + hashes) == Some(&'"')).then_some((hashes, at + 2 + hashes))
}

/// The index past a char literal at `at`, or `None` for a lifetime.
fn char_literal_end(s: &[char], at: usize) -> Option<usize> {
    if s.get(at + 1) == Some(&'\\') {
        let close = s.iter().skip(at + 3).take(10).position(|&c| c == '\'')?;
        return Some(at + 3 + close + 1);
    }
    (s.get(at + 1).is_some_and(|&c| c != '\'') && s.get(at + 2) == Some(&'\'')).then_some(at + 3)
}

/// When a `#[cfg(test)]` module starts at `at`, the index just past it: past
/// the `;` of a declaration, or past the brace closing an inline body.
fn test_module_end(src: &[Lexed], at: usize) -> Option<usize> {
    let mut i = eat(src, at, "#[cfg(test)]")?;
    loop {
        i = skip_ws(src, i);
        if eat(src, i, "#[").is_none() {
            break;
        }
        i = matching(src, i + 1, '[', ']')?;
    }
    if let Some(after) = eat(src, i, "pub") {
        i = skip_ws(src, after);
        if eat(src, i, "(").is_some() {
            i = skip_ws(src, matching(src, i, '(', ')')?);
        }
    }
    let after_mod = eat(src, i, "mod")?;
    let name = skip_ws(src, after_mod);
    if name == after_mod {
        return None;
    }
    i = name;
    while src.get(i).is_some_and(|&(c, code)| code && is_ident(c)) {
        i += 1;
    }
    if i == name {
        return None;
    }
    i = skip_ws(src, i);
    match src.get(i) {
        Some((';', true)) => Some(i + 1),
        Some(('{', true)) => matching(src, i, '{', '}'),
        _ => None,
    }
}

/// `at + needle.len()` when the code at `at` spells `needle`.
fn eat(src: &[Lexed], at: usize, needle: &str) -> Option<usize> {
    let mut i = at;
    for want in needle.chars() {
        match src.get(i) {
            Some(&(c, true)) if c == want => i += 1,
            _ => return None,
        }
    }
    Some(i)
}

fn skip_ws(src: &[Lexed], mut i: usize) -> usize {
    while src
        .get(i)
        .is_some_and(|&(c, code)| code && c.is_whitespace())
    {
        i += 1;
    }
    i
}

/// The index past the `close` balancing the `open` at `at`. Literal chars
/// never count, so a `"}"` inside a test module cannot end it early.
fn matching(src: &[Lexed], at: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &(c, code)) in src.iter().enumerate().skip(at) {
        if code && c == open {
            depth += 1;
        } else if code && c == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i + 1);
            }
        }
    }
    None
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
