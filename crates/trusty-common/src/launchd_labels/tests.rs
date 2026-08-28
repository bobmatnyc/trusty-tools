//! Drift guards for the canonical launchd-label registry (#4919).
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
/// #6290: retired rows are held to the same rule — a retirement moves a row
/// between tables, so its label must still be the one that is actually loaded
/// on a host, or the eviction boots out a unit that does not exist.
/// Test: this is the test.
#[test]
fn canonical_consts_match_the_convention() {
    for svc in SERVICES.iter().chain(RETIRED_SERVICES) {
        if svc.sub_unit.is_some() {
            continue;
        }
        assert_eq!(
            svc.label,
            canonical_label(svc.member),
            "{}'s label restates something the `com.trusty.<stem>` convention \
             does not produce — either the convention changed (update \
             `canonical_label`) or the literal drifted (#4919)",
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
/// #4919 is about.
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
    // #6290: across BOTH tables. A retired label colliding with a live one is
    // the worst version of this — an install would evict the unit it is about
    // to need.
    for svc in SERVICES.iter().chain(RETIRED_SERVICES) {
        for label in std::iter::once(&svc.label).chain(svc.legacy.iter()) {
            assert!(
                !seen.contains(label),
                "{label} is claimed by more than one service"
            );
            seen.push(label);
        }
    }
}

/// Why (#6290): the two tables answer opposite questions — what an install
/// WRITES versus what an upgrade CLEARS — and a row that appeared in both would
/// have an install bootstrap a unit and then boot it out, or the reverse,
/// depending on step order.
/// What: no retired member appears in `SERVICES`, and `RETIRED_SERVICES` names
/// exactly the members whose daemon has been retired.
/// Test: this is the test.
#[test]
fn retired_services_are_not_installed() {
    for retired in RETIRED_SERVICES {
        assert!(
            !SERVICES.iter().any(|s| s.member == retired.member),
            "{} is retired but still listed as a service an install writes",
            retired.member
        );
        assert!(
            retired_service_for_member(retired.member).is_some(),
            "{} must be reachable by member lookup, or nothing can evict it",
            retired.member
        );
    }
    let members: Vec<&str> = RETIRED_SERVICES.iter().map(|s| s.member).collect();
    assert_eq!(
        members,
        vec!["trusty-review"],
        "adding a retirement is a deliberate act — update this pin with it"
    );
}

/// Why (#6290): trusty-review's unit exists under two names on real hosts —
/// `com.trusty.review` from the post-#4919 installer and
/// `com.trusty.trusty-review` from before it. An eviction that clears only one
/// leaves the other loaded, respawning a `serve` subcommand the binary no
/// longer has, which is a crash loop with nothing in the log but a usage
/// message.
/// What: both labels come back from `retired_labels_for_member`, canonical
/// first, and a live member yields nothing.
/// Test: this is the test.
#[test]
fn retired_review_carries_both_its_labels() {
    let labels = retired_labels_for_member("trusty-review");
    assert_eq!(labels, vec![REVIEW, "com.trusty.trusty-review"]);
    assert!(
        retired_labels_for_member("trusty-search").is_empty(),
        "a live member has nothing to evict"
    );
    assert!(
        legacy_labels_for(REVIEW).contains(&"com.trusty.trusty-review"),
        "the retired row's legacy alias must still resolve through the label lookup"
    );
}

/// Why: the pre-#4919 labels must stay recorded as legacy or an upgrade from a
/// host installed before this fix silently leaves the old unit running beside
/// the new one — #2938's exact footgun, two daemons on :7878.
/// What: pins the specific aliases the divergent sites used.
/// Test: this is the test.
#[test]
fn pre_fix_labels_are_recorded_as_legacy() {
    assert!(
        legacy_labels_for(SEARCH).contains(&"com.trusty.trusty-search"),
        "the label `trusty-search service install` wrote before #4919 must be \
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

/// Why (#4919, and #2827 before it): every re-fix so far corrected one literal
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
            // Naming a legacy label in order to EVICT it is the one correct use
            // of one. A `launchctl bootout`/`unload` line in a build file is
            // doing exactly the migration this issue is about; the bug is
            // naming a legacy label to install, load, or point a user at.
            let evicting =
                kind != Kind::Rust && (line.contains("bootout") || line.contains("unload"));
            for label in extract_labels(&codesign_stripped(&line)) {
                if evicting && legacy_labels_for_any().contains(&label.as_str()) {
                    continue;
                }
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
         (#4919 — derive them from the registry instead of restating them):\n  {}",
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
/// #4919 review: the first version used `take_while`, so the scan STOPPED at the
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
    let mut in_block_comment = false;
    while let Some(line) = lines.next() {
        if kind == Kind::Rust && !in_block_comment && is_test_cfg_attribute(line) {
            skip_test_item(line, &mut lines);
            continue;
        }
        let code = strip_comment(line, kind, &mut in_block_comment);
        if !code.trim().is_empty() {
            out.push(code);
        }
    }
    out
}

/// Whether a line is a `#[cfg(…)]` attribute gating on the `test` cfg.
///
/// Whether a `#[cfg(…)]` attribute gates code that exists ONLY under `cfg(test)`.
///
/// Why: three separate ways to get this wrong, each of which silences the scan
/// over production code.
///
/// 1. A substring match for `test` fires on
///    `#[cfg(feature = "embedder-test-support")]`, which gates production code.
/// 2. #4919 review: POLARITY. `#[cfg(not(test))]` gates code that exists in
///    every NON-test build — the most production a thing can be — and
///    `#[cfg(any(…, test))]` gates code that exists in test builds AND others.
///    Treating either as "a test item" made `skip_test_item` swallow exactly
///    the code it was supposed to read. Literals planted inside
///    `#[cfg(not(test))] fn cache_base_dir()` and an `any(…, test)` function
///    both passed the guard; eight such sites exist in this workspace.
/// 3. `all(test, …)` IS test-only, because every conjunct must hold.
///
/// What: true only when a standalone `test` token appears outside any `not(…)`
/// and outside any `any(…)`. Everything else is production.
/// Test: `production_lines_keeps_a_feature_cfg_that_merely_contains_test`,
/// `is_test_cfg_attribute_respects_polarity`.
fn is_test_cfg_attribute(line: &str) -> bool {
    let t = line.trim_start();
    if !t.starts_with("#[cfg(") {
        return false;
    }
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut rest = t;
    while let Some(idx) = rest.find("test") {
        let before = &rest[..idx];
        let before_ok = before.chars().next_back().is_none_or(|c| !is_ident(c));
        let after_ok = rest[idx + 4..].chars().next().is_none_or(|c| !is_ident(c));
        // A standalone `test` still does not make the item test-only if it sits
        // under a `not(` or an `any(` that is still open at this point.
        if before_ok && after_ok && !under_negation_or_disjunction(before) {
            return true;
        }
        rest = &rest[idx + 4..];
    }
    false
}

/// Whether the text preceding a `test` token leaves a `not(` or `any(` open.
///
/// What: walks the prefix tracking parenthesis depth, recording the depth at
/// which each `not(` / `any(` opened, and reports whether any such combinator
/// is still unclosed at the end of the prefix.
fn under_negation_or_disjunction(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    let mut depth: i32 = 0;
    let mut open_combinators: Vec<i32> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            let head = prefix[..i].trim_end();
            if head.ends_with("not") || head.ends_with("any") {
                open_combinators.push(depth);
            }
            depth += 1;
        } else if bytes[i] == b')' {
            depth -= 1;
            open_combinators.retain(|d| *d < depth);
        }
        i += 1;
    }
    !open_combinators.is_empty()
}

/// Consume the item a `#[cfg(test)]` attribute applies to.
///
/// Why (#4919 review): `#[cfg(test)] use std::fmt;` puts the attribute and the
/// item on ONE line. Unconditionally consuming from the NEXT line therefore ate
/// a line of production code — a literal planted there passed the guard.
///
/// What: returns immediately when the attribute line already carries its whole
/// item (a `;`-terminated statement, or braces that balance on that line).
/// Otherwise a declaration ending in `;` costs one line and a braced item is
/// consumed until its braces balance.
/// Test: `skip_test_item_consumes_nothing_when_the_item_is_on_the_attribute_line`.
fn skip_test_item<'a>(
    attr_line: &str,
    lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
) {
    // Everything after the closing `]` of the attribute, if anything.
    let tail = attr_line
        .rsplit_once("])")
        .map_or_else(|| attr_line.rsplit_once(']').map(|(_, t)| t), |_| None)
        .unwrap_or("")
        .trim();
    if !tail.is_empty() {
        let opens = tail.matches('{').count();
        let closes = tail.matches('}').count();
        if tail.ends_with(';') || (opens > 0 && opens == closes) {
            return;
        }
        if opens > closes {
            consume_until_balanced(lines, i32::try_from(opens - closes).unwrap_or(1));
            return;
        }
    }

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

/// Consume lines until an already-open brace nesting closes.
fn consume_until_balanced<'a>(
    lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
    mut depth: i32,
) {
    for line in lines.by_ref() {
        depth += i32::try_from(line.matches('{').count()).unwrap_or(0);
        depth -= i32::try_from(line.matches('}').count()).unwrap_or(0);
        if depth <= 0 {
            return;
        }
    }
}

/// Remove the comment portion of a line for the given file kind.
///
/// Why (#4919 review): the Rust arm used to blank any line whose trimmed start
/// was `*`, as a proxy for "inside a block comment". That also blanked a deref
/// assignment — `*target = "com.trusty.trusty-search".to_string();` passed the
/// guard while the identical `let` form failed. Block-comment state is now
/// tracked across lines instead of guessed at from one.
///
/// What: `in_block_comment` carries `/* … */` state between calls. A line
/// inside a block comment, or a `//`-prefixed line, yields empty. Note that a
/// continuation line of a multi-line Rust STRING literal carries no quote of
/// its own, so quote-presence is not usable as a filter — that is how the
/// `com.trusty.mpm.plist` hints in `daemon_bridge` and `serve_stdio` (the #2827
/// defect class) initially escaped this scan.
/// Test: `strip_comment_keeps_a_deref_assignment`,
/// `strip_comment_tracks_block_comment_state`.
fn strip_comment(line: &str, kind: Kind, in_block_comment: &mut bool) -> String {
    let t = line.trim_start();
    match kind {
        Kind::Rust => {
            if *in_block_comment {
                // The comment ends here; anything after `*/` is code again.
                if let Some((_, after)) = line.split_once("*/") {
                    *in_block_comment = false;
                    return after.to_string();
                }
                return String::new();
            }
            if t.starts_with("//") {
                return String::new();
            }
            if let Some((before, rest)) = line.split_once("/*") {
                if let Some((_, after)) = rest.split_once("*/") {
                    return format!("{before}{after}");
                }
                *in_block_comment = true;
                return before.to_string();
            }
            line.to_string()
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
/// Why (#4919 review): scanning `crates/**/src/**.rs` alone left the two file
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

/// Why (#4919 review): the first `production_lines` used `take_while`, so a
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

/// Why (#4919 review, round 2): POLARITY. `#[cfg(not(test))]` gates code that
/// exists in every non-test build, and `#[cfg(any(…, test))]` gates code that
/// exists in test builds AND others — both are PRODUCTION. Treating them as
/// test items made the scan skip exactly what it should read; literals planted
/// inside a `not(test)` function and an `any(…, test)` function both passed.
/// Eight such sites exist in this workspace.
/// What: `test` under `not(…)` or `any(…)` is production; bare `test` and
/// `all(test, …)` are test-only.
/// Test: this is the test.
#[test]
fn is_test_cfg_attribute_respects_polarity() {
    assert!(is_test_cfg_attribute("#[cfg(test)]"));
    assert!(is_test_cfg_attribute(
        "#[cfg(all(test, target_os = \"macos\"))]"
    ));

    assert!(
        !is_test_cfg_attribute("#[cfg(not(test))]"),
        "`not(test)` gates code present in every non-test build"
    );
    assert!(
        !is_test_cfg_attribute(
            "#[cfg(any(all(target_os = \"macos\", target_arch = \"aarch64\"), test))]"
        ),
        "`any(…, test)` gates code present outside test builds too"
    );
    assert!(!is_test_cfg_attribute("#[cfg(all(not(test), unix))]"));
}

/// Why: the polarity fix only matters if the scan actually reads the body it
/// stops skipping.
/// What: a literal inside a `#[cfg(not(test))]` function is retained.
/// Test: this is the test.
#[test]
fn production_lines_reads_bodies_gated_on_not_test() {
    let body =
        "#[cfg(not(test))]\nfn cache_base_dir() {\n    let x = \"com.trusty.trusty-search\";\n}\n";
    let kept = production_lines(body, Kind::Rust);
    assert!(
        kept.iter().any(|l| l.contains("com.trusty.trusty-search")),
        "a `not(test)` body is production and must be scanned, kept: {kept:?}"
    );
}

/// Why (#4919 review, round 2): `strip_comment` blanked any line whose trimmed
/// start was `*`, as a proxy for "inside a block comment". A deref assignment
/// starts with `*` too, so `*target = "com.trusty.trusty-search".to_string();`
/// passed the guard while the identical `let` form failed.
/// What: a deref assignment is code; block-comment state is tracked across
/// lines instead of guessed from one.
/// Test: this is the test.
#[test]
fn strip_comment_keeps_a_deref_assignment() {
    let body = "*target = \"com.trusty.trusty-search\".to_string();\n";
    let kept = production_lines(body, Kind::Rust);
    assert!(
        kept.iter().any(|l| l.contains("com.trusty.trusty-search")),
        "a deref assignment is code, not a comment continuation, kept: {kept:?}"
    );
}

/// Why: block comments legitimately narrate legacy labels, so their bodies must
/// still be dropped — the fix must not simply stop stripping.
/// What: a `/* … */` body is dropped and code after it is kept.
/// Test: this is the test.
#[test]
fn strip_comment_tracks_block_comment_state() {
    let body = "/*\n * com.trusty.trusty-search was the old label\n */\nlet a = 1;\n";
    let kept = production_lines(body, Kind::Rust);
    assert!(
        !kept.iter().any(|l| l.contains("com.trusty.trusty-search")),
        "a block-comment body must stay unscanned, kept: {kept:?}"
    );
    assert!(kept.iter().any(|l| l.contains("let a = 1")));
}

/// Why (#4919 review, round 2): `#[cfg(test)] use std::fmt;` puts the attribute
/// AND the item on one line, so consuming from the next line ate a line of
/// production code — a literal planted there passed.
/// What: an attribute line carrying its whole item consumes nothing further.
/// Test: this is the test.
#[test]
fn skip_test_item_consumes_nothing_when_the_item_is_on_the_attribute_line() {
    let body = "#[cfg(test)] use std::fmt;\nlet x = \"com.trusty.trusty-search\";\n";
    let kept = production_lines(body, Kind::Rust);
    assert!(
        kept.iter().any(|l| l.contains("com.trusty.trusty-search")),
        "the line after a self-contained test item is production, kept: {kept:?}"
    );
}

/// Why (#4919 review, round 2): the codesign skip was whole-line, so a shell
/// line carrying both an identifier and a plist path had its real launchd label
/// skipped along with the identifier.
/// What: only the token adjacent to the marker is removed.
/// Test: this is the test.
#[test]
fn codesign_stripped_spares_only_the_identifier_token() {
    let line = "local X_IDENTIFIER=\"com.trusty.trusty-mpm\"; local f2=\"/x/com.bobmatnyc.trusty-search.plist\"";
    let out = codesign_stripped(line);
    assert!(
        !out.contains("com.trusty.trusty-mpm"),
        "the codesign identifier must be exempt, got: {out}"
    );
    assert!(
        out.contains("com.bobmatnyc.trusty-search"),
        "a launchd label on the same line must still be scanned, got: {out}"
    );

    let flag = codesign_stripped("codesign --identifier com.trusty.trusty-search /bin/x");
    assert!(!flag.contains("com.trusty.trusty-search"), "got: {flag}");
}

/// Every legacy alias in the registry, flattened.
///
/// Used to recognise a build-file line that names an old label in order to boot
/// it out — the migration this issue exists to perform.
fn legacy_labels_for_any() -> Vec<&'static str> {
    SERVICES
        .iter()
        .flat_map(|s| s.legacy.iter().copied())
        .collect()
}

/// Why: a Makefile that boots out a legacy label is performing the migration,
/// not perpetuating the drift. Rejecting it would have forced the deploy
/// recipes to drop the eviction they need.
/// What: a legacy label on a `bootout` line is allowed in a build file; the
/// same label on an install line is not.
/// Test: this is the test.
#[test]
fn eviction_lines_may_name_a_legacy_label() {
    let evict = "\t-launchctl bootout gui/$$(id -u)/com.trusty.trusty-search 2>/dev/null\n";
    let install = "PLIST := $(HOME)/Library/LaunchAgents/com.trusty.trusty-search.plist\n";

    let kept = production_lines(evict, Kind::Hash);
    assert!(
        !kept.is_empty(),
        "the bootout line must survive comment stripping"
    );
    assert!(
        legacy_labels_for_any().contains(&"com.trusty.trusty-search"),
        "the alias must be registered for the eviction allowance to apply"
    );
    // The install-shaped line carries no bootout/unload, so it stays a stray.
    let install_kept = production_lines(install, Kind::Hash);
    assert!(!install_kept[0].contains("bootout"));
}

/// Blank out only the identifier TOKEN adjacent to a codesign marker.
///
/// Why: codesign and bundle identifiers are a different namespace that uses the
/// full binary name deliberately — renaming one invalidates a binary's
/// designated requirement and re-triggers macOS TCC prompts (#2558).
///
/// #4919 review: skipping the whole LINE was too coarse. A shell line carrying
/// both an identifier and a plist path —
/// `local X_IDENTIFIER="a"; local f2=".../com.bobmatnyc.trusty-search.plist"` —
/// had its real launchd label skipped along with the identifier.
///
/// What: replaces the quoted or bare token immediately following
/// `--identifier` or an `*_IDENTIFIER=` assignment, leaving the rest of the
/// line to be scanned.
/// Test: `codesign_stripped_spares_only_the_identifier_token`.
fn codesign_stripped(line: &str) -> String {
    const MARKERS: &[&str] = &["--identifier", "IDENTIFIER="];
    let mut out = line.to_string();
    for marker in MARKERS {
        while let Some(idx) = out.find(marker) {
            let after = idx + marker.len();
            let rest = &out[after..];
            // Skip separators between the marker and its value.
            let val_start = rest
                .find(|c: char| !matches!(c, ' ' | '=' | '"' | '\'' | '\t'))
                .unwrap_or(rest.len());
            let tail = &rest[val_start..];
            let val_end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
                .unwrap_or(tail.len());
            let abs_start = after + val_start;
            let abs_end = abs_start + val_end;
            // Neutralise the marker so the loop terminates, and drop the token.
            out.replace_range(abs_start..abs_end, "");
            out.replace_range(idx..after, &"_".repeat(marker.len()));
        }
    }
    out
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
