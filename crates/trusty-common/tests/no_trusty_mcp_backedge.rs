//! Ratchet: `trusty-common` must never depend on `trusty-mcp`.
//!
//! Why: `trusty-mcp` depends on `trusty-common` (and #6316 slice 2 makes that
//! edge unconditional, so the shared `daemon_bridge_json_rpc` can reach
//! `trusty_common::uds`). An edge back the other way — which the `tickets`
//! feature carried until #6316 — closes a cycle the moment `tickets` is on,
//! and cargo reports it as an unrelated resolution failure far from the
//! manifest line that caused it. This test names the cause instead.
//! What: Parses this crate's own `Cargo.toml` and fails if `trusty-mcp`
//! appears as a key in any dependency table, as a `[dependencies.trusty-mcp]`
//! style header, or as `dep:trusty-mcp` / `trusty-mcp/<feature>` inside
//! `[features]`. Comment lines are ignored, which is what keeps the
//! explanatory comments in that manifest from tripping it.
//! Test: this file — `manifest_declares_no_trusty_mcp_dependency` is the
//! assertion, `scanner_flags_a_reintroduced_dependency` proves the scanner
//! would actually catch a regression rather than passing vacuously.
//!
//! Scope: the DIRECT edge, which is the one a person reintroduces by editing a
//! manifest. `cargo tree -p trusty-common -e features -i trusty-mcp` is the
//! whole-graph check; run it when auditing, not on every build.
//!
//! No `required-features`: this target compiles under every feature lane,
//! including `--features unconditional-only`, so no lane can skip the ratchet.

// #6316: trusty-common must not depend on trusty-mcp (cycle)

const FORBIDDEN: &str = "trusty-mcp";

/// One reason a manifest fails the ratchet: the line and why it matched.
#[derive(Debug, PartialEq, Eq)]
struct Violation {
    section: String,
    line: String,
}

/// Scan a `Cargo.toml`'s text for any declaration of `trusty-mcp`.
///
/// Why: the check has to be exact about *where* a match counts — the same
/// crate name appears legitimately in this manifest's prose comments.
/// What: tracks the current table header, then flags a dependency-table key,
/// a `[…dependencies.trusty-mcp]` header, or a feature-list mention.
/// Test: `scanner_flags_a_reintroduced_dependency`,
/// `scanner_ignores_comments_and_unrelated_keys`.
fn scan(manifest: &str) -> Vec<Violation> {
    let mut section = String::new();
    let mut found = Vec::new();

    for raw in manifest.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = header
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string();
            // `[dependencies.trusty-mcp]`, `[target.'cfg(unix)'.dev-dependencies.trusty-mcp]`.
            if section.ends_with(&format!("dependencies.{FORBIDDEN}")) {
                found.push(Violation {
                    section: section.clone(),
                    line: line.to_string(),
                });
            }
            continue;
        }

        let is_dep_table = section.ends_with("dependencies");
        let key = line.split('=').next().unwrap_or("").trim();
        if is_dep_table && key == FORBIDDEN {
            found.push(Violation {
                section: section.clone(),
                line: line.to_string(),
            });
        }

        if section == "features"
            && (line.contains(&format!("dep:{FORBIDDEN}"))
                || line.contains(&format!("\"{FORBIDDEN}/")))
        {
            found.push(Violation {
                section: section.clone(),
                line: line.to_string(),
            });
        }
    }

    found
}

#[test]
fn manifest_declares_no_trusty_mcp_dependency() {
    let manifest = include_str!("../Cargo.toml");
    let violations = scan(manifest);
    assert!(
        violations.is_empty(),
        "trusty-common regained a `{FORBIDDEN}` dependency, which closes a \
         cycle (`trusty-mcp` depends on `trusty-common`). See #6316. \
         Offending declarations: {violations:#?}"
    );
}

#[test]
fn scanner_flags_a_reintroduced_dependency() {
    let regressed = "\
[dependencies]
anyhow = { workspace = true }
trusty-mcp = { workspace = true, optional = true }

[features]
tickets = [\"dep:trusty-mcp\", \"gh-cli\"]
";
    let violations = scan(regressed);
    assert_eq!(
        violations.len(),
        2,
        "expected the dependency key and the feature entry: {violations:#?}"
    );
    assert_eq!(violations[0].section, "dependencies");
    assert_eq!(violations[1].section, "features");
}

#[test]
fn scanner_flags_a_dotted_dependency_header() {
    let regressed = "[target.'cfg(unix)'.dev-dependencies.trusty-mcp]\nversion = \"0.1\"\n";
    assert_eq!(scan(regressed).len(), 1, "dotted header must be caught");
}

#[test]
fn scanner_ignores_comments_and_unrelated_keys() {
    let clean = "\
[dependencies]
# trusty-mcp = { workspace = true } -- removed by #6316, do not restore
trusty-mcp-adjacent = { workspace = true }

[package.metadata.docs]
note = \"see trusty-mcp for the shared loop\"

[features]
tickets = [\"dep:uuid\", \"gh-cli\"]
";
    assert_eq!(scan(clean), Vec::new());
}
