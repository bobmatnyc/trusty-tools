//! Drift guards for the canonical launchd-label registry (#4868).
//!
//! Why: the registry only helps if nothing outside it writes a launchd label.
//! #2827 was fixed by correcting one literal, and the same class of defect came
//! straight back somewhere else because nothing stopped the next literal from
//! being typed. These tests are that stop.

use super::*;

/// Why: a `SERVICES` entry could restate a label the convention would never
/// produce — which is exactly how `com.trusty.trusty-search` survived. The
/// convention has to be checkable, not merely documented.
/// What: every main-daemon entry's `label` must equal `canonical_label(member)`.
/// Test: this is the test.
#[test]
fn canonical_consts_match_the_convention() {
    for svc in SERVICES {
        if svc.sub_unit.is_some() {
            continue;
        }
        assert_eq!(
            svc.label,
            canonical_label(svc.member),
            "{}'s label restates something the `com.trusty.<stem>` convention \
             does not produce — either the convention changed (update \
             `canonical_label`) or the literal drifted (#4868)",
            svc.member
        );
    }
}

/// Why: a sub-unit whose label does not extend its member's base label is a
/// unit an upgrade will orphan — `com.trusty.trusty-search.logrotate` outlived
/// the main unit it was named after and is still loaded on the owner's host.
/// What: every sub-unit entry's label must be `<base>.<sub_unit>`, where base
/// is that member's own main-daemon label.
/// Test: this is the test.
#[test]
fn sub_unit_labels_extend_their_base() {
    for svc in SERVICES {
        let Some(sub) = svc.sub_unit else { continue };
        let base = canonical_label(svc.member);
        assert_eq!(
            svc.label,
            sub_label(&base, sub),
            "{}'s `{sub}` sub-unit must be named off its member's base label",
            svc.member
        );
    }
}

/// Why: listing a still-canonical label as legacy makes an install evict the
/// unit it just bootstrapped — a self-inflicted outage, which is half of what
/// #4868 is about.
/// What: no legacy alias may equal any service's canonical label.
/// Test: this is the test.
#[test]
fn legacy_labels_are_never_canonical() {
    for svc in SERVICES {
        for legacy in svc.legacy {
            assert!(
                service_for_label(legacy).is_none(),
                "{legacy} is listed as a legacy alias of {} but is also some \
                 service's canonical label — evicting it would take down a \
                 live unit",
                svc.label
            );
        }
    }
}

/// Why: two services claiming the same legacy alias means an install of either
/// one boots out the other's unit.
/// What: every canonical label is unique, and no legacy alias appears twice.
/// Test: this is the test.
#[test]
fn every_legacy_label_resolves_to_one_service() {
    let mut seen: Vec<&str> = Vec::new();
    for svc in SERVICES {
        for label in std::iter::once(&svc.label).chain(svc.legacy.iter()) {
            assert!(
                !seen.contains(label),
                "{label} is claimed by more than one service"
            );
            seen.push(label);
        }
    }
}

/// Why: the pre-#4868 labels must stay recorded as legacy or an upgrade from a
/// host installed before this fix silently leaves the old unit running beside
/// the new one — #2938's exact footgun, two daemons on :7878.
/// What: pins the specific aliases the divergent sites used.
/// Test: this is the test.
#[test]
fn pre_fix_labels_are_recorded_as_legacy() {
    assert!(
        legacy_labels_for(SEARCH).contains(&"com.trusty.trusty-search"),
        "the label `trusty-search service install` wrote before #4868 must be \
         evicted on upgrade"
    );
    assert!(
        legacy_labels_for(SEARCH).contains(&"com.bobmatnyc.trusty-search"),
        "the trusty-search Makefile's `com.bobmatnyc.*` family must be evicted \
         on upgrade"
    );
    assert!(legacy_labels_for(CONSOLE).contains(&"com.trusty.trusty-console"));
    assert!(legacy_labels_for(REVIEW).contains(&"com.trusty.trusty-review"));
    assert!(legacy_labels_for(SEARCH_LOGROTATE).contains(&"com.trusty.trusty-search.logrotate"));
}

/// Directories whose launchd-label literals are legitimately outside the
/// registry.
///
/// `macos_signing` mints CODESIGN identifiers, a different namespace that uses
/// the full binary name on purpose — renaming one invalidates the binary's
/// designated requirement and re-triggers macOS TCC prompts (#2558). It must
/// not be normalised onto the launchd convention.
const SCAN_EXEMPT_PATHS: &[&str] = &["trusty-installer/src/commands/macos_signing"];

/// Why (#4868, and #2827 before it): every re-fix so far corrected one literal
/// and left the mechanism that mints them intact, so a new divergent literal
/// appeared somewhere else — the installer's mirror table, a Makefile, a shell
/// hint. This scans the production sources for launchd-label literals and
/// fails on any the registry does not own, so the third recurrence cannot
/// merge.
///
/// What: walks every workspace member's `src/**.rs` plus the build and deploy
/// files a label can hide in (`Makefile`, `*.sh`, `*.plist`, `*.yml`), strips
/// each Rust file's inline test module (test fixtures deliberately name drifted
/// labels that exist on other hosts) and every comment, extracts
/// `com.trusty.*` / `com.bobmatnyc.*` tokens, and reports each one. Legacy
/// aliases are rejected everywhere. In Rust a CANONICAL label typed as a
/// literal is rejected too — a correct-but-duplicated literal is the state
/// trusty-search's label was in before it drifted, and Rust has the registry.
/// A Makefile, shell script or plist cannot import a Rust constant, so it may
/// name the canonical label; a legacy or unknown one still fails.
/// Test: this is the test.
#[test]
fn no_stray_launchd_label_literals_in_workspace_sources() {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_scannable_files(&root, &mut files);

    let rs = files.iter().filter(|p| kind_of(p) == Kind::Rust).count();
    let other = files.len() - rs;
    assert!(
        rs > 2000 && other > 20,
        "the scan found {rs} Rust and {other} build/deploy file(s) under {} — a \
         broken walk would pass this test vacuously",
        root.display()
    );

    let mut strays: Vec<String> = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        if SCAN_EXEMPT_PATHS.iter().any(|ex| rel.contains(ex)) {
            continue;
        }
        // The registry is where the literals are SUPPOSED to live.
        if rel.contains("trusty-common/src/launchd_labels") {
            continue;
        }
        let kind = kind_of(path);
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in production_lines(&body, kind) {
            // Codesign / bundle identifiers are a different namespace that uses
            // the full binary name deliberately; renaming one invalidates a
            // binary's designated requirement and re-triggers macOS TCC prompts
            // (#2558). They are always an explicit `--identifier` or a
            // `*_IDENTIFIER` assignment, never a launchd label.
            if line.contains("--identifier") || line.contains("IDENTIFIER=") {
                continue;
            }
            for label in extract_labels(&line) {
                // A Makefile, shell script or plist cannot import a Rust
                // constant, so naming the CANONICAL label is the best it can
                // do. What it must never carry is a legacy or unknown label —
                // that is what `com.bobmatnyc.trusty-search` was. Rust has no
                // such excuse: it gets the registry, so any literal is a stray.
                if kind != Kind::Rust && is_canonical_label(&label) {
                    continue;
                }
                strays.push(format!("{rel}: {label}"));
            }
        }
    }

    assert!(
        strays.is_empty(),
        "launchd label literals not owned by `trusty_common::launchd_labels` \
         (#4868 — derive them from the registry instead of restating them):\n  {}",
        strays.join("\n  ")
    );
}

/// What sort of file is being scanned, which decides its comment syntax and
/// whether it has an inline test module to strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Rust source: `//` comments, and an inline `#[cfg(test)] mod tests { … }`
    /// to strip.
    Rust,
    /// Makefile / shell / YAML: `#` comments, no test module.
    Hash,
    /// XML property list: `<!-- -->` comments, no test module.
    Xml,
}

/// Lines of a file that carry executable meaning — comments stripped, and for
/// Rust, the inline test module removed.
///
/// Why: comments legitimately discuss a legacy label (this module's own header
/// does), and test fixtures deliberately name drifted labels that exist on other
/// hosts, such as `launchd_probe`'s `com.trusty.mpm.dogfood`. Scanning either
/// would force them to lie.
///
/// #4868 review: the first version used `take_while`, so the scan STOPPED at the
/// first `#[cfg(test)]`-ish line. In a `mod.rs` that declaration sits near the
/// top among the module list, so the entire production body below it went
/// unscanned — a literal planted at line 301 of
/// `trusty-mpm/src/services/discoverer/mod.rs` passed. It now SKIPS the test
/// item and keeps going: a bare `mod tests;` declaration costs one line, and a
/// `mod tests { … }` block is skipped by brace balance.
///
/// What: yields owned lines with comment text removed. A line whose code part
/// is empty is dropped.
/// Test: `production_lines_skips_past_a_test_module_declaration`,
/// `production_lines_strips_an_inline_test_block`,
/// `production_lines_keeps_a_feature_cfg_that_merely_contains_test`.
fn production_lines(body: &str, kind: Kind) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = body.lines().peekable();
    while let Some(line) = lines.next() {
        if kind == Kind::Rust && is_test_cfg_attribute(line) {
            skip_test_item(&mut lines);
            continue;
        }
        let code = strip_comment(line, kind);
        if !code.trim().is_empty() {
            out.push(code);
        }
    }
    out
}

/// Whether a line is a `#[cfg(…)]` attribute gating on the `test` cfg.
///
/// Why: a substring check for `test` also fires on
/// `#[cfg(feature = "embedder-test-support")]`, which gates PRODUCTION code —
/// skipping the item it guards would reopen the hole this function exists to
/// close.
/// What: true only when the attribute contains `test` as a standalone token
/// (not part of a longer identifier such as `embedder-test-support`).
fn is_test_cfg_attribute(line: &str) -> bool {
    let t = line.trim_start();
    if !t.starts_with("#[cfg(") {
        return false;
    }
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut rest = t;
    while let Some(idx) = rest.find("test") {
        let before_ok = rest[..idx].chars().next_back().is_none_or(|c| !is_ident(c));
        let after_ok = rest[idx + 4..].chars().next().is_none_or(|c| !is_ident(c));
        if before_ok && after_ok {
            return true;
        }
        rest = &rest[idx + 4..];
    }
    false
}

/// Consume the item a `#[cfg(test)]` attribute applies to.
///
/// What: a declaration or `use` ending in `;` costs exactly one line; a braced
/// item is consumed until its braces balance.
fn skip_test_item<'a>(lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>) {
    let mut depth: i32 = 0;
    let mut opened = false;
    for line in lines.by_ref() {
        depth += i32::try_from(line.matches('{').count()).unwrap_or(0);
        depth -= i32::try_from(line.matches('}').count()).unwrap_or(0);
        if line.contains('{') {
            opened = true;
        }
        if opened {
            if depth <= 0 {
                return;
            }
        } else if line.trim_end().ends_with(';') {
            return;
        }
    }
}

/// Remove the comment portion of a line for the given file kind.
fn strip_comment(line: &str, kind: Kind) -> String {
    let t = line.trim_start();
    match kind {
        // A continuation line of a multi-line Rust string literal carries no
        // quote of its own, so quote-presence is NOT usable as the filter —
        // that is how the `com.trusty.mpm.plist` hints in `daemon_bridge` and
        // `serve_stdio` (the #2827 defect class: a hint naming a plist launchd
        // does not have) initially escaped this scan. Drop whole-line comments
        // and block-comment bodies instead.
        Kind::Rust => {
            if t.starts_with("//") || t.starts_with('*') || t.starts_with("/*") {
                String::new()
            } else {
                line.to_string()
            }
        }
        Kind::Hash => line.split('#').next().unwrap_or("").to_string(),
        Kind::Xml => {
            if t.starts_with("<!--") {
                String::new()
            } else {
                line.to_string()
            }
        }
    }
}

/// Classify a path by the comment syntax it uses.
fn kind_of(path: &std::path::Path) -> Kind {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if name.ends_with(".rs") {
        Kind::Rust
    } else if name.ends_with(".plist") {
        Kind::Xml
    } else {
        Kind::Hash
    }
}

/// Resolve the workspace root from this crate's manifest directory.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/trusty-common has a workspace root two levels up")
        .to_path_buf()
}

/// Recursively collect the files the label scan covers.
///
/// Why (#4868 review): scanning `crates/**/src/**.rs` alone left the two file
/// types the last recurrence actually lived in unguarded — the per-crate
/// `Makefile`s carried the `com.bobmatnyc.*` family and
/// `scripts/install-trusty-search-signed.sh` carried the inverted hint. A guard
/// that cannot see where the bug was is not a guard.
///
/// What: every `.rs` under a crate `src/` tree, plus every `Makefile`, `*.sh`,
/// `*.plist` and `*.yml` in the repository. Dedicated test/bench trees and
/// `docs/` are skipped — their fixtures and research notes name drifted labels
/// deliberately.
fn collect_scannable_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                "target"
                    | "tests"
                    | "benches"
                    | "node_modules"
                    | "docs"
                    | ".git"
                    | ".claude"
                    | "test-data"
                    | "testdata"
                    | "vmtest-harness"
            ) {
                continue;
            }
            collect_scannable_files(&path, out);
            continue;
        }
        let is_rust = name.ends_with(".rs")
            && !name.ends_with("_tests.rs")
            && !name.ends_with("_test.rs")
            && name != "tests.rs";
        let is_build_or_deploy = name == "Makefile"
            || name.ends_with(".sh")
            || name.ends_with(".plist")
            || name.ends_with(".yml");
        if is_rust || is_build_or_deploy {
            out.push(path);
        }
    }
}

/// Why (#4868 review): the first `production_lines` used `take_while`, so a
/// `mod tests;` declaration near the top of a `mod.rs` terminated the scan and
/// everything below it went unread — 342 of 2847 files lost more than half
/// their body, many after ~15 lines. A planted literal at line 301 of
/// `trusty-mpm/src/services/discoverer/mod.rs` passed.
/// What: a declaration costs one line and scanning continues past it.
/// Test: this is the test.
#[test]
fn production_lines_skips_past_a_test_module_declaration() {
    let body = "pub mod a;\n#[cfg(test)]\nmod tests;\npub mod b;\nlet x = \"deep\";\n";
    let kept = production_lines(body, Kind::Rust);
    assert!(
        kept.iter().any(|l| l.contains("deep")),
        "code below a `mod tests;` declaration must still be scanned, kept: {kept:?}"
    );
    assert!(!kept.iter().any(|l| l.contains("mod tests;")));
}

/// Why: an inline test module names drifted labels on purpose, so it must be
/// skipped — but only as far as its closing brace.
/// What: the block body is dropped and code after it is kept.
/// Test: this is the test.
#[test]
fn production_lines_strips_an_inline_test_block() {
    let body = "fn real() {}\n#[cfg(all(test, target_os = \"macos\"))]\nmod tests {\n    let f = \"com.trusty.trusty-fixture\";\n}\nfn after() {}\n";
    let kept = production_lines(body, Kind::Rust);
    assert!(!kept.iter().any(|l| l.contains("trusty-fixture")));
    assert!(
        kept.iter().any(|l| l.contains("fn after")),
        "code after the test block must survive, kept: {kept:?}"
    );
}

/// Why: a substring check for `test` also fires on
/// `#[cfg(feature = "embedder-test-support")]`, which gates PRODUCTION code.
/// Skipping the item it guards would reopen the hole the fix closes.
/// What: only a standalone `test` cfg token counts.
/// Test: this is the test.
#[test]
fn production_lines_keeps_a_feature_cfg_that_merely_contains_test() {
    assert!(!is_test_cfg_attribute(
        "#[cfg(feature = \"embedder-test-support\")]"
    ));
    assert!(is_test_cfg_attribute("#[cfg(test)]"));
    assert!(is_test_cfg_attribute(
        "#[cfg(all(test, target_os = \"macos\"))]"
    ));

    let body = "#[cfg(feature = \"embedder-test-support\")]\npub fn kept() {}\n";
    let kept = production_lines(body, Kind::Rust);
    assert!(
        kept.iter().any(|l| l.contains("pub fn kept")),
        "a feature cfg must not swallow the item it guards, kept: {kept:?}"
    );
}

/// Why: Makefiles and shell scripts are where two of the divergent sites lived,
/// and their comments legitimately narrate the old labels.
/// What: `#` comments are stripped, code is kept.
/// Test: this is the test.
#[test]
fn production_lines_strips_hash_comments() {
    let body = "# was com.trusty.trusty-search\nPLIST := com.trusty.search.plist\n";
    let kept = production_lines(body, Kind::Hash);
    assert_eq!(kept.len(), 1);
    assert!(kept[0].contains("com.trusty.search"));
}

/// Pull `com.trusty.*` / `com.bobmatnyc.*` label tokens out of one line.
///
/// Trailing `.plist` and punctuation are stripped so a path or a hint string
/// resolves to the label it names rather than reading as a stray.
fn extract_labels(line: &str) -> Vec<String> {
    const PREFIXES: &[&str] = &["com.trusty.", "com.bobmatnyc."];
    let mut found = Vec::new();
    for prefix in PREFIXES {
        let mut rest = line;
        while let Some(idx) = rest.find(prefix) {
            let tail = &rest[idx..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
                .unwrap_or(tail.len());
            let token = tail[..end]
                .trim_end_matches('.')
                .trim_end_matches(".plist")
                .trim_end_matches('.');
            // A bare prefix is a `starts_with` guard, not a label.
            if token.len() > prefix.len() {
                found.push(token.to_string());
            }
            rest = &rest[idx + prefix.len()..];
        }
    }
    found
}
