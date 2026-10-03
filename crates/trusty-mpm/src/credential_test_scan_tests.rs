//! Test-code scan: a credential test is `#[serial]` and runs inside the
//! credential sandbox (#9123).
//!
//! Why: the #9123 audit found 24 tests across five crates that cleared one
//! credential variable and let the resolver fall through to the developer's
//! real `.env.local`, `$HOME` store or ambient token, some without `#[serial]`,
//! some then `assert_eq!`-ing — printing — what came back. A new one is a
//! single test away in any crate; this scan fails CI on it.
//! What: every `.rs` file under `crates/*/src` and `crates/*/tests` is lexed
//! (comments removed, literals kept as text) with the source scan's lexer. A
//! test fn that mutates the environment AND names a credential-shaped variable
//! (or that enters `CredentialSandbox`) is a credential test. It is safe when
//! it carries the unkeyed `#[serial]` — the group the sandbox holds; a keyed
//! group does not exclude it — and reaches `CredentialSandbox::enter`, itself
//! or through a helper fn in the same file. Every other credential test counts
//! against its file's [`KNOWN_UNSANDBOXED`] budget: a file over budget fails, and a
//! file under budget fails until the budget is lowered, so the list only
//! shrinks. Failure messages carry file and fn names only.
//! Test: `every_credential_test_is_serial_and_sandboxed`,
//! `the_credential_test_scan_judges_each_shape`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::scan_tests::{Lexed, eat, is_ident, lex, matching, names_word, skip_ws};

/// Calls that mutate the process environment, matched as whole identifiers.
const MUTATIONS: &[&str] = &[
    "set_var",
    "remove_var",
    "EnvVarGuard",
    "with_env",
    "in_sandbox",
    "CredentialSandbox",
];

/// The sandbox type a credential test must reach.
const SANDBOX: &str = "CredentialSandbox";

/// Fragments that make an upper-case identifier a credential variable name.
const MARKERS: &[&str] = &["TOKEN", "SECRET", "PASSWORD", "API_KEY", "_KEY"];

/// `(file under crates/, credential tests in it that are not both `#[serial]`
/// and sandboxed)`. Every entry predates #9123's guard and none is one of the
/// issue's rows; many are hermetic by other means (an injected
/// `MemoryKeyStore`, a pinned `$HOME`, a keyed serial group). Lower an entry
/// when you move a test onto the sandbox; the scan fails until you do.
const KNOWN_UNSANDBOXED: &[(&str, usize)] = &[
    ("trusty-agents/src/agents/tests/mod.rs", 5),
    ("trusty-agents/src/api/server/tests/channel_inbound.rs", 2),
    ("trusty-agents/src/api/server/tests/models.rs", 1),
    ("trusty-agents/src/channels/slack.rs", 2),
    ("trusty-agents/src/channels/telegram.rs", 2),
    ("trusty-agents/src/ctrl/ctrl_turn/dispatch.rs", 4),
    ("trusty-agents/src/llm/credentials.rs", 6),
    ("trusty-agents/src/llm/helpers/tests.rs", 3),
    ("trusty-agents/src/llm/http/tests.rs", 5),
    ("trusty-agents/src/llm/provider_pin.rs", 1),
    ("trusty-agents/src/mcp/tests/extensions_tests.rs", 1),
    ("trusty-agents/src/runtime/startup.rs", 1),
    ("trusty-agents/src/system_status/credentials.rs", 3),
    ("trusty-agents/src/tools/mcp_tools/mod.rs", 1),
    ("trusty-agents/tests/inference_shared_adapter_e2e.rs", 2),
    ("trusty-agents/tests/persona_python_plugin_wiring.rs", 1),
    ("trusty-channels/tests/client_http.rs", 3),
    ("trusty-channels/tests/telegram_client_http.rs", 3),
    ("trusty-code/src/task/mock_llm.rs", 1),
    ("trusty-common/src/credentials/authority.rs", 5),
    ("trusty-common/src/credentials/bounded_store/tests.rs", 2),
    ("trusty-common/src/credentials/dotenv.rs", 2),
    ("trusty-common/src/credentials/resolver.rs", 5),
    ("trusty-common/src/inference/providers/local.rs", 4),
    ("trusty-common/src/memory_core/dream/tests.rs", 8),
    (
        "trusty-common/src/memory_core/semantic_consolidation/mod.rs",
        6,
    ),
    ("trusty-common/tests/config_keys_cli.rs", 2),
    ("trusty-mcp/tests/mcp_config.rs", 1),
    ("trusty-memory/src/commands/prompt_context/tests.rs", 1),
    ("trusty-mpm/src/core/gh_account_enforce.rs", 2),
    ("trusty-mpm/src/core/gh_identity.rs", 6),
    ("trusty-mpm/src/core/oauth_token.rs", 3),
    (
        "trusty-mpm/src/core/session_launch/tests_skill_overrides_7751.rs",
        1,
    ),
    ("trusty-mpm/src/daemon/api_tests.rs", 1),
    ("trusty-mpm/src/daemon/bug_report/github_tests.rs", 2),
    ("trusty-mpm/src/daemon/llm_overseer.rs", 2),
    ("trusty-mpm/src/runtime/claude_code_tests.rs", 2),
    ("trusty-mpm/src/secret_source_tests.rs", 3),
    ("trusty-mpm/src/telegram/tests.rs", 1),
    ("trusty-mpm/tests/auth_cost.rs", 1),
    ("trusty-review/src/integrations/context/atlassian.rs", 4),
    ("trusty-review/src/integrations/github/auth/strategy.rs", 4),
    ("trusty-review/src/pipeline/runner_tests.rs", 2),
];

/// One fn item: its name, its outer attributes, and its body — as written
/// (literals included, so a variable named in a string counts) and as code
/// only (literals blanked, so a call named in a string does not).
struct FnItem {
    name: String,
    attrs: Vec<String>,
    body: String,
    code: String,
}

impl FnItem {
    /// The attribute paths, `#[` `]` and any argument list stripped.
    fn attr_paths(&self) -> impl Iterator<Item = (&str, bool)> {
        self.attrs.iter().map(|a| {
            let inner = a.trim_start_matches("#[").trim_end_matches(']');
            let path = inner.split('(').next().unwrap_or("").trim();
            (path, inner.contains('('))
        })
    }

    fn is_test(&self) -> bool {
        self.attr_paths()
            .any(|(p, _)| p == "test" || p.ends_with("::test"))
    }

    /// The unkeyed `#[serial]` / `#[serial_test::serial]`.
    fn is_unkeyed_serial(&self) -> bool {
        self.attr_paths()
            .any(|(p, args)| !args && (p == "serial" || p == "serial_test::serial"))
    }
}

fn text_of(src: &[Lexed]) -> String {
    src.iter().map(|&(c, _)| c).collect()
}

/// Qualifiers that may sit between a fn's attributes and its `fn` keyword.
const QUALIFIERS: &[&str] = &["pub", "async", "unsafe", "const", "extern"];

/// Every fn item with a body in `text`, nested ones included.
fn fn_items(text: &str) -> Vec<FnItem> {
    let src = lex(text);
    let mut out = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let (c, code) = src[i];
        if code && c.is_whitespace() {
            i += 1;
            continue;
        }
        if eat(&src, i, "#[").is_some()
            && let Some(end) = matching(&src, i + 1, '[', ']')
        {
            attrs.push(text_of(&src[i..end]));
            i = end;
            continue;
        }
        if code && is_ident(c) && !(i > 0 && src[i - 1].1 && is_ident(src[i - 1].0)) {
            let start = i;
            while src.get(i).is_some_and(|&(c, code)| code && is_ident(c)) {
                i += 1;
            }
            let word = text_of(&src[start..i]);
            if word == "fn" {
                if let Some((item, next)) = fn_item(&src, i, std::mem::take(&mut attrs)) {
                    out.push(item);
                    i = next;
                }
                continue;
            }
            if word == "pub" && eat(&src, skip_ws(&src, i), "(").is_some() {
                i = matching(&src, skip_ws(&src, i), '(', ')').unwrap_or(i);
            }
            if !QUALIFIERS.contains(&word.as_str()) {
                attrs.clear();
            }
            continue;
        }
        attrs.clear();
        i += 1;
    }
    out
}

/// The fn whose name starts after `fn` at `at`, and where scanning resumes
/// (just inside its body, so nested fns are found too).
fn fn_item(src: &[Lexed], at: usize, attrs: Vec<String>) -> Option<(FnItem, usize)> {
    let name_at = skip_ws(src, at);
    let mut j = name_at;
    while src.get(j).is_some_and(|&(c, code)| code && is_ident(c)) {
        j += 1;
    }
    let name = text_of(&src[name_at..j]);
    let open = (j..src.len()).find(|&k| src[k].1 && matches!(src[k].0, '{' | ';'))?;
    if src[open].0 == ';' {
        return None;
    }
    let close = matching(src, open, '{', '}')?;
    let body = text_of(&src[open..close]);
    let code = src[open..close]
        .iter()
        .map(|&(c, code)| if code { c } else { ' ' })
        .collect();
    Some((
        FnItem {
            name,
            attrs,
            body,
            code,
        },
        open + 1,
    ))
}

/// Whether `body` names a credential-shaped upper-case identifier.
fn names_credential(body: &str) -> bool {
    body.split(|c: char| !is_ident(c)).any(|w| {
        w.len() > 3
            && w.starts_with(|c: char| c.is_ascii_uppercase())
            && w.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            && (MARKERS.iter().any(|m| w.contains(m)) || w.ends_with("_PAT"))
    })
}

/// The credential tests in one file that are not both `#[serial]` and sandboxed.
fn unsandboxed_credential_tests(text: &str) -> Vec<String> {
    let items = fn_items(text);
    // Fns that reach the sandbox, directly or through another such fn.
    let mut reach: BTreeSet<&str> = items
        .iter()
        .filter(|f| names_word(&f.code, SANDBOX))
        .map(|f| f.name.as_str())
        .collect();
    loop {
        let more: Vec<&str> = items
            .iter()
            .filter(|f| !reach.contains(f.name.as_str()))
            .filter(|f| reach.iter().any(|r| names_word(&f.code, r)))
            .map(|f| f.name.as_str())
            .collect();
        if more.is_empty() {
            break;
        }
        reach.extend(more);
    }
    items
        .iter()
        .filter(|f| f.is_test())
        .filter(|f| {
            let sandboxed = reach.contains(f.name.as_str());
            let mutates = MUTATIONS.iter().any(|m| names_word(&f.code, m));
            let credential = sandboxed || (mutates && names_credential(&f.body));
            credential && !(sandboxed && f.is_unkeyed_serial())
        })
        .map(|f| f.name.clone())
        .collect()
}

/// Every `.rs` file under `dir`, as `(path relative to crates/, text)`.
fn rust_sources(crates: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("read an entry of {}: {e}", dir.display()));
    for entry in entries {
        let path: PathBuf = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name != "target" && name != "node_modules" {
                rust_sources(crates, &path, out);
            }
            continue;
        }
        if path.extension().is_some_and(|e| e == "rs") {
            let rel = path
                .strip_prefix(crates)
                .expect("under crates/")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, std::fs::read_to_string(&path).expect("read source")));
        }
    }
}

/// Why/What: see the module docs.
/// Test: this test.
#[test]
fn every_credential_test_is_serial_and_sandboxed() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ is this crate's parent")
        .to_path_buf();
    let mut sources = Vec::new();
    let roots = std::fs::read_dir(&crates).expect("read crates/");
    for krate in roots.map(|e| e.expect("crate entry").path()) {
        for sub in ["src", "tests"] {
            if krate.join(sub).is_dir() {
                rust_sources(&crates, &krate.join(sub), &mut sources);
            }
        }
    }
    assert!(
        sources
            .iter()
            .any(|(rel, _)| rel.starts_with("trusty-agents/")),
        "the scan did not reach the workspace's other crates ({} files)",
        sources.len()
    );

    let found: BTreeMap<String, Vec<String>> = sources
        .iter()
        .map(|(rel, text)| (rel.clone(), unsandboxed_credential_tests(text)))
        .filter(|(_, tests)| !tests.is_empty())
        .collect();
    let budget: BTreeMap<&str, usize> = KNOWN_UNSANDBOXED.iter().copied().collect();

    let over: Vec<String> = found
        .iter()
        .filter(|(rel, tests)| tests.len() > budget.get(rel.as_str()).copied().unwrap_or(0))
        .map(|(rel, tests)| format!("{rel}: {}", tests.join(", ")))
        .collect();
    assert!(
        over.is_empty(),
        "#9123: a credential test must be the unkeyed `#[serial]` and enter \
         `trusty_common::credentials::test_sandbox::CredentialSandbox` (itself or \
         through a helper in the same file). Over budget:\n  {}",
        over.join("\n  ")
    );

    let stale: Vec<String> = budget
        .iter()
        .filter(|(rel, n)| found.get(**rel).map_or(0, Vec::len) < **n)
        .map(|(rel, _)| format!("{rel} -> {}", found.get(*rel).map_or(0, Vec::len)))
        .collect();
    assert!(
        stale.is_empty(),
        "KNOWN_UNSANDBOXED overstates these files; lower each entry to the count shown:\n  {}",
        stale.join("\n  ")
    );
}

/// Why: a scan that cannot fail proves nothing. Each fixture is one shape the
/// rule must judge: the #9123 row shapes are flagged, the fixed shapes pass,
/// and an env test that touches no credential is not a credential test.
/// Test: this test.
#[test]
fn the_credential_test_scan_judges_each_shape() {
    let flagged = |src: &str| unsandboxed_credential_tests(src);

    // Unserialized removal of a credential (the LOW rows).
    let unserialized = "#[test]\nfn t() { unsafe { std::env::remove_var(\"GITHUB_TOKEN\"); } }";
    assert_eq!(flagged(unserialized), vec!["t".to_string()]);

    // Serialized but unsandboxed (the latent rows), via a constant name.
    let serial_only =
        "#[test]\n#[serial]\nfn t() { unsafe { std::env::set_var(TOKEN_ENV_VAR, \"x\") }; }";
    assert_eq!(flagged(serial_only), vec!["t".to_string()]);

    // Sandboxed but not serialized: the sandbox mutates the environment too.
    let sandbox_only = "#[tokio::test]\nasync fn t() { let _s = CredentialSandbox::enter(); }";
    assert_eq!(flagged(sandbox_only), vec!["t".to_string()]);

    // A keyed group does not exclude the sandbox's unkeyed one.
    let keyed = "#[test]\n#[serial(creds)]\nfn t() { let _s = CredentialSandbox::enter(); }";
    assert_eq!(flagged(keyed), vec!["t".to_string()]);

    // The fixed shapes: direct, and through a same-file helper chain.
    let fixed = "#[test]\n#[serial_test::serial]\nfn t() { let _s = CredentialSandbox::enter(); }";
    assert!(flagged(fixed).is_empty());
    let helper = "fn inner() { let _s = CredentialSandbox::enter(); }\n\
                  fn with_env(kv: &[(&str, Option<&str>)]) { inner(); }\n\
                  #[test]\n#[serial]\nfn t() { with_env(&[(\"OPENROUTER_API_KEY\", None)]); }";
    assert!(flagged(helper).is_empty());

    // Not a credential test: env mutation of a plain variable, a credential
    // named in a comment only, and a non-test fn.
    let plain = "#[test]\nfn t() { unsafe { std::env::set_var(\"NO_COLOR\", \"1\") } }";
    assert!(flagged(plain).is_empty());
    let commented = "#[test]\nfn t() { // remove_var(\"GITHUB_TOKEN\")\n}";
    assert!(flagged(commented).is_empty());
    let not_test = "fn t() { unsafe { std::env::remove_var(\"GITHUB_TOKEN\") } }";
    assert!(flagged(not_test).is_empty());

    // A test inside a `mod tests` block, behind a doc comment and `pub(crate)`.
    let nested = "#[cfg(test)]\nmod tests {\n    /// doc\n    #[test]\n    pub(crate) fn t() \
                  { unsafe { std::env::remove_var(\"X_API_KEY\") } }\n}";
    assert_eq!(flagged(nested), vec!["t".to_string()]);
}
