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
/// What: walks every workspace member's `src/**.rs`, takes the portion of each
/// file BEFORE its `#[cfg(test)]` marker (test fixtures deliberately name
/// drifted labels that exist on other hosts), keeps only string-literal-bearing
/// non-comment lines, extracts `com.trusty.*` / `com.bobmatnyc.*` tokens, and
/// requires [`is_canonical_label`] to recognise each one. Legacy aliases are
/// rejected on purpose — a legacy literal in production source IS the defect.
/// Test: this is the test.
#[test]
fn no_stray_launchd_label_literals_in_workspace_sources() {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates"), &mut files);
    assert!(
        files.len() > 100,
        "the scan found only {} files under {} — a broken walk would pass this \
         test vacuously",
        files.len(),
        root.join("crates").display()
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
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in production_lines(&body) {
            let trimmed = line.trim_start();
            // Comments may legitimately discuss a legacy label; code may not.
            // A continuation line of a multi-line string literal carries no
            // quote of its own, so quote-presence is NOT usable as the filter —
            // that is how the `com.trusty.mpm.plist` hints in `daemon_bridge`
            // and `serve_stdio` (the #2827 defect class: a hint naming a plist
            // launchd does not have) initially escaped this scan.
            if trimmed.starts_with("//") || trimmed.starts_with('*') {
                continue;
            }
            for label in extract_labels(line) {
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

/// Lines of a source file that are production code — everything above its
/// inline test module.
///
/// Why: test fixtures deliberately name drifted labels that exist on other
/// hosts (`launchd_probe`'s `com.trusty.mpm.dogfood`), so scanning them would
/// force those fixtures to lie. Splitting on the literal `#[cfg(test)]` was not
/// enough: `trusty-console`'s module is `#[cfg(all(test, target_os = "macos"))]`
/// and slipped straight through.
/// What: yields lines until the first `mod tests` or any `#[cfg(…test…)]`
/// attribute, whichever comes first.
fn production_lines(body: &str) -> impl Iterator<Item = &str> {
    body.lines().take_while(|line| {
        let t = line.trim_start();
        !(t.starts_with("mod tests")
            || t.starts_with("pub mod tests")
            || (t.starts_with("#[cfg(") && t.contains("test")))
    })
}

/// Resolve the workspace root from this crate's manifest directory.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/trusty-common has a workspace root two levels up")
        .to_path_buf()
}

/// Recursively collect `src/**.rs` files under a crates directory, skipping
/// dedicated test/bench trees (their fixtures name drifted labels on purpose).
fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
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
                "target" | "tests" | "benches" | "node_modules"
            ) {
                continue;
            }
            collect_rs_files(&path, out);
        } else if name.ends_with(".rs")
            && !name.ends_with("_tests.rs")
            && !name.ends_with("_test.rs")
            && name != "tests.rs"
        {
            out.push(path);
        }
    }
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
