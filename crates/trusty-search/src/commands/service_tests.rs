//! Unit tests for the `service` module (`trusty-search service …`).
//!
//! Why: keeping tests in a sibling file rather than inline lets the definition
//! file stay under the 500-SLOC production cap (`scripts/check_line_cap.sh`)
//! while retaining full coverage — mirrors the `plist_bootstrap.rs` /
//! `plist_bootstrap_tests.rs` split in trusty-installer.
//!
//! What: covers the launchd label the installed unit targets (#4868), the
//! fd-limit (#2947) and `KeepAlive` (#4113) plist fixes, the `ExitTimeOut`
//! window (#4393), and the auto-discovery arg/env rules (#4823).
//!
//! Test: `cargo test -p trusty-search commands::service`.

// `super::*` (in particular `build_launchd_config`) is only referenced by
// the macOS-only tests below; on other platforms the whole module body
// compiles out to nothing, so gate the import to avoid an
// `unused_imports` -D warnings failure on non-macOS CI runners (#2947
// follow-up).
#[cfg(target_os = "macos")]
use super::*;

/// Why (#4868 — the regression this test exists for): `LAUNCHD_LABEL` was
/// `com.trusty.trusty-search`, a unit launchd has never had loaded. Every
/// `service install` therefore wrote and bootstrapped a SECOND agent beside
/// the live `com.trusty.search`, evicted nothing, and left the plist fixes
/// made under this same issue (`ExitTimeOut`) in a file launchd never reads.
/// A test asserting the constant against a re-typed literal is what let the
/// wrong value look correct for four issues, so this asserts against the
/// registry and names the wrong answer explicitly.
/// What: the label is the canonical `com.trusty.search`, the pre-fix label
/// is recorded as legacy so an upgrade evicts it, and the plist the
/// renderer produces carries that label in its `Label` key — the file
/// launchd actually reads, not just the struct.
/// Test: pure construction plus string generation, no fs side effects.
#[cfg(target_os = "macos")]
#[test]
fn build_launchd_config_targets_the_label_launchd_has_loaded() {
    use std::path::PathBuf;
    use trusty_common::launchd_labels;

    assert_eq!(
        LAUNCHD_LABEL,
        launchd_labels::SEARCH,
        "the daemon must name the unit the installer and doctor name"
    );
    assert_ne!(
        LAUNCHD_LABEL, "com.trusty.trusty-search",
        "this is the pre-#4868 label — bootstrapping it starts a second \
         daemon contending for :7878 and the index locks (#2938)"
    );
    assert!(
        launchd_labels::legacy_labels_for(LAUNCHD_LABEL).contains(&"com.trusty.trusty-search"),
        "the pre-#4868 label must be evicted on upgrade or a host that ran \
         the old installer keeps both units"
    );

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        false,
        None,
    );
    assert_eq!(cfg.label, LAUNCHD_LABEL);
    assert_eq!(
        cfg.plist_path().expect("home dir resolvable").file_name(),
        Some(std::ffi::OsStr::new("com.trusty.search.plist")),
        "the plist filename and the Label key must agree or launchctl \
         cannot find the job"
    );
    let xml = cfg.render_plist().expect("render_plist must succeed");
    assert!(
        xml.contains(&format!("<string>{LAUNCHD_LABEL}</string>")),
        "the rendered Label key is what launchd reads, got xml: {xml}"
    );
}

/// Why (#4868): the plist fix only reaches the live unit if the file
/// `install_and_activate` writes — under the canonical label — is the one
/// carrying it. #4868's `ExitTimeOut` was rendered correctly all along; it was
/// written under a label nothing loaded, so the daemon kept launchd's 5 s
/// default. That makes the load-bearing assertion `ExitTimeOut` AND
/// `com.trusty.search` in the SAME document, not `ExitTimeOut` on its own —
/// which trusty-common's `render_plist_declares_exit_timeout` already covers.
///
/// #4868 review: an earlier version of this test asserted only on
/// `render_plist()` and was named for an install path it never touched.
/// What: renders the exact config `service install` hands to
/// `install_and_activate`, and requires the canonical `Label`, the plist
/// filename, and `ExitTimeOut` to agree in one rendered unit.
/// Test: pure string generation, no fs side effects and no `launchctl`.
#[cfg(target_os = "macos")]
#[test]
fn the_unit_install_activates_carries_the_exit_timeout_fix() {
    use std::path::PathBuf;

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        false,
        None,
    );
    // The file `install_and_activate` writes is `plist_path()`, keyed off the
    // same label it bootstraps. Both must be canonical or the fix lands in an
    // inert file again.
    assert_eq!(
        cfg.plist_path().expect("home dir resolvable").file_name(),
        Some(std::ffi::OsStr::new("com.trusty.search.plist"))
    );

    let xml = cfg.render_plist().expect("render_plist must succeed");
    assert!(
        xml.contains(&format!(
            "<string>{}</string>",
            trusty_common::launchd_labels::SEARCH
        )),
        "the activated unit must declare the canonical label, got xml: {xml}"
    );
    assert!(
        xml.contains(&format!(
            "<key>ExitTimeOut</key>\n  <integer>{}</integer>",
            trusty_common::shutdown::TERMINATION_GRACE_SECS
        )),
        "the unit `service install` activates must carry #4868's \
         ExitTimeOut, got xml: {xml}"
    );
}

/// Why (#4868 review): `installed_unit` read only `<canonical>.plist`, so on the
/// host this whole issue describes — one whose live unit still carries the
/// LEGACY name — it returned `None` and every #4823 tunable was silently
/// dropped, moments before eviction deleted the plist holding the only record.
/// What: the canonical plist is consulted first, then each legacy alias in
/// registry order, so a migrating host still has somewhere to read from.
/// Test: pure path construction — no filesystem access.
#[cfg(target_os = "macos")]
#[test]
fn installed_unit_paths_prefers_canonical_then_legacy() {
    let home = std::path::Path::new("/Users/x");
    let paths = installed_unit_paths(home);
    let names: Vec<String> = paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        names.first().map(String::as_str),
        Some("com.trusty.search.plist"),
        "the canonical unit must win when it exists, got {names:?}"
    );
    assert!(
        names.contains(&"com.trusty.trusty-search.plist".to_string()),
        "the pre-#4868 plist must be readable as a fallback or the migration \
         destroys operator tunables, got {names:?}"
    );
    assert!(
        names.contains(&"com.bobmatnyc.trusty-search.plist".to_string()),
        "the Makefile's label family must be readable too, got {names:?}"
    );
    assert!(paths
        .iter()
        .all(|p| p.starts_with("/Users/x/Library/LaunchAgents")));
}

/// Why: the LaunchdConfig handed to `trusty_common::launchd` must always
/// carry the fd-limit fix — dropping it silently reintroduces the
/// large-fleet warm-boot EMFILE crash (issue #2947).
/// What: builds the config with dummy paths and asserts `fd_limit` is
/// `Some(LAUNCHD_FD_LIMIT)`.
/// Test: pure construction, no fs side effects.
#[cfg(target_os = "macos")]
#[test]
fn build_launchd_config_sets_fd_limit() {
    use std::path::PathBuf;
    use trusty_common::launchd::LAUNCHD_FD_LIMIT;

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        false,
        None,
    );
    assert_eq!(
        cfg.fd_limit,
        Some(LAUNCHD_FD_LIMIT),
        "fd_limit must be Some(LAUNCHD_FD_LIMIT) so the generated plist \
         raises both soft and hard NumberOfFiles limits to \
         {LAUNCHD_FD_LIMIT}, preventing EMFILE during large-fleet \
         warm-boot (issue #2947)"
    );
}

/// Why: the generated plist XML (what launchd actually reads from disk)
/// must contain both resource-limit dicts with the canonical fd value —
/// asserting on `render_plist()` output catches regressions where the
/// config struct is correct but the renderer drops the dicts.
/// What: renders the plist with a dummy exe/log dir and checks that the
/// `SoftResourceLimits` / `HardResourceLimits` / `NumberOfFiles` keys
/// appear with the right integer value.
/// Test: pure string generation, no fs side effects.
#[cfg(target_os = "macos")]
#[test]
fn build_launchd_config_plist_includes_fd_limit() {
    use std::path::PathBuf;
    use trusty_common::launchd::LAUNCHD_FD_LIMIT;

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        false,
        None,
    );
    let xml = cfg.render_plist().expect("render_plist must succeed");

    assert!(
        xml.contains("<key>SoftResourceLimits</key>"),
        "plist must contain SoftResourceLimits to raise the fd ceiling"
    );
    assert!(
        xml.contains("<key>HardResourceLimits</key>"),
        "plist must contain HardResourceLimits so the soft limit is not \
         clamped below it"
    );
    let fd_str = format!("<integer>{LAUNCHD_FD_LIMIT}</integer>");
    assert!(
        xml.contains(&fd_str),
        "plist NumberOfFiles must equal {LAUNCHD_FD_LIMIT}, got xml: {xml}"
    );
}

/// Why: issue #4113 — `KeepAlive::OnSuccess` (`SuccessfulExit: false`)
/// restarts the daemon only after a NON-zero exit, so a clean SIGTERM /
/// orderly drain left search down indefinitely with no recovery and no
/// alarm. Reverting this field to `OnSuccess` silently reintroduces a
/// permanent-outage class of bug that no other test would catch.
/// What: builds the config with dummy paths and asserts `keep_alive` is
/// `KeepAlive::Always`.
/// Test: pure construction, no fs side effects.
#[cfg(target_os = "macos")]
#[test]
fn build_launchd_config_keeps_alive_after_clean_exit() {
    use std::path::PathBuf;
    use trusty_common::launchd::KeepAlive;

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        false,
        None,
    );
    assert_eq!(
        cfg.keep_alive,
        KeepAlive::Always,
        "keep_alive must be KeepAlive::Always so launchd restarts the \
         daemon after a CLEAN (exit 0) shutdown too — KeepAlive::OnSuccess \
         leaves it down permanently (issue #4113). Deliberate stops go \
         through `launchctl bootout` / `trusty-search service uninstall`."
    );
}

/// Why: the config struct can be right while the renderer emits the wrong
/// plist fragment — launchd reads the XML, not the struct. This asserts on
/// what actually lands in `~/Library/LaunchAgents`.
/// What: renders the plist and requires an unconditional
/// `<key>KeepAlive</key><true/>` with no `SuccessfulExit` dictionary
/// anywhere in the document.
/// Test: pure string generation, no fs side effects.
#[cfg(target_os = "macos")]
#[test]
fn build_launchd_config_plist_has_unconditional_keepalive() {
    use std::path::PathBuf;

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        false,
        None,
    );
    let xml = cfg.render_plist().expect("render_plist must succeed");

    assert!(
        xml.contains("<key>KeepAlive</key>\n  <true/>"),
        "plist must set KeepAlive unconditionally true (issue #4113), got \
         xml: {xml}"
    );
    assert!(
        !xml.contains("SuccessfulExit"),
        "plist must NOT carry a SuccessfulExit dict — that is the \
         restart-only-on-failure policy #4113 removed, got xml: {xml}"
    );
}

/// Why (issue #4823): the whole point of the fix is that the operator's
/// auto-discovery suppression reaches the file launchd actually reads.
/// Asserting on `render_plist()` rather than the config struct is what
/// catches the #4709 failure mode where the struct is right and the
/// renderer drops the fragment.
/// What: builds a suppressing config and requires `--no-auto-discover` to
/// appear inside `ProgramArguments`, after `start --foreground`.
/// Test: pure string generation, no fs side effects.
#[cfg(target_os = "macos")]
#[test]
fn build_launchd_config_plist_carries_no_auto_discover_arg() {
    use std::path::PathBuf;

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        true,
        None,
    );
    assert_eq!(
        cfg.args,
        vec!["start", "--foreground", "--no-auto-discover"],
        "the suppression must travel as a CLI arg (issue #4823)"
    );

    let xml = cfg.render_plist().expect("render_plist must succeed");
    let args_section = xml
        .split("<key>ProgramArguments</key>")
        .nth(1)
        .and_then(|rest| rest.split("</array>").next())
        .unwrap_or_default();
    assert!(
        args_section.contains("<string>--no-auto-discover</string>"),
        "ProgramArguments must carry --no-auto-discover so the setting \
         survives regeneration (issue #4823), got xml: {xml}"
    );
    // The #4709 restart policy must survive alongside the new arg.
    assert!(
        xml.contains("<key>KeepAlive</key>\n  <true/>"),
        "KeepAlive must stay unconditionally true (issue #4113/#4709) when \
         auto-discovery is suppressed, got xml: {xml}"
    );
}

/// Why: the default install must be byte-identical to pre-#4823 behaviour
/// — adding the flag unconditionally would disable discovery for everyone.
/// What: a non-suppressing config must not mention the flag anywhere.
/// Test: pure string generation, no fs side effects.
#[cfg(target_os = "macos")]
#[test]
fn build_launchd_config_omits_no_auto_discover_by_default() {
    use std::path::PathBuf;

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        false,
        None,
    );
    assert_eq!(cfg.args, vec!["start", "--foreground"]);
    let xml = cfg.render_plist().expect("render_plist must succeed");
    assert!(
        !xml.contains("no-auto-discover"),
        "default install must not suppress auto-discovery, got xml: {xml}"
    );
}

/// Why (issue #4823 comment — the trap): the reporter's `daemon.env` holds
/// `TRUSTY_NO_AUTO_DISCOVER=1`. Copying that into the plist's
/// `EnvironmentVariables` is what made the daemon refuse to boot, because
/// clap parses the env value through `FromStr<bool>`. The generated plist
/// must never carry that key at all, whatever the process env or the
/// installed unit says.
/// What: drives the assembly with a lookup that returns `1` for the var AND
/// an installed unit carrying `TRUSTY_NO_AUTO_DISCOVER=1`, then requires
/// both the pairs and the rendered XML to be free of the key.
/// Test: pure — the env lookup is injected, so no process env is mutated.
#[cfg(target_os = "macos")]
#[test]
fn launchd_env_pairs_never_carries_no_auto_discover() {
    use crate::commands::service_unit::{parse_installed_unit, NO_AUTO_DISCOVER_ENV};
    use std::path::PathBuf;

    let existing = parse_installed_unit(
        "<key>EnvironmentVariables</key><dict>\
         <key>TRUSTY_NO_AUTO_DISCOVER</key><string>1</string></dict>",
    );
    assert!(
        existing.suppresses_auto_discover(),
        "fixture must represent the hand-edited unit"
    );

    let pairs = launchd_env_pairs(
        Some(PathBuf::from("/Users/x")),
        |k| (k == NO_AUTO_DISCOVER_ENV).then(|| "1".to_string()),
        Some(&existing),
    );
    assert!(
        !pairs.iter().any(|(k, _)| k == NO_AUTO_DISCOVER_ENV),
        "launchd_env_pairs must never emit {NO_AUTO_DISCOVER_ENV}, got {pairs:?}"
    );

    // The renderer is the thing launchd reads — assert there too. The
    // suppression must survive as a CLI arg while the env key stays absent.
    let mut cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        true,
        Some(&existing),
    );
    cfg.env_vars = pairs;
    let xml = cfg.render_plist().expect("render_plist must succeed");
    assert!(
        !xml.contains(NO_AUTO_DISCOVER_ENV),
        "the rendered plist must never carry {NO_AUTO_DISCOVER_ENV} — a \
         value clap rejects aborts daemon startup (issue #4823), got xml: {xml}"
    );
    assert!(
        xml.contains("<string>--no-auto-discover</string>"),
        "the suppression must still be preserved, as a CLI arg, got xml: {xml}"
    );
}

/// Why (issue #4823, generalised): `service install` runs from a shell that
/// exports nothing, so reading only the process env blanked tunables the
/// installed unit carried — silently unpinning e.g. `TRUSTY_DEVICE=cpu`.
/// What: with an empty env lookup, the installed unit's value survives; the
/// unconditional `HF_HOME` pin (#86) is still emitted alongside it.
/// Test: pure — the env lookup is injected, so no process env is mutated.
#[cfg(target_os = "macos")]
#[test]
fn launchd_env_pairs_carries_forward_installed_tunables() {
    use crate::commands::service_unit::parse_installed_unit;
    use std::path::PathBuf;

    let existing = parse_installed_unit(
        "<key>EnvironmentVariables</key><dict>\
         <key>TRUSTY_BM25_CORPUS_CAP</key><string>4242</string></dict>",
    );
    let pairs = launchd_env_pairs(Some(PathBuf::from("/Users/x")), |_| None, Some(&existing));
    assert!(
        pairs
            .iter()
            .any(|(k, v)| k == "TRUSTY_BM25_CORPUS_CAP" && v == "4242"),
        "an installed unit's tunable must survive regeneration (issue \
         #4823), got {pairs:?}"
    );
    assert!(
        pairs
            .iter()
            .any(|(k, v)| k == "HF_HOME" && v == "/Users/x/.cache/huggingface"),
        "the #86 HF_HOME pin must still be emitted, got {pairs:?}"
    );
}

/// Why (#4868 review — I introduced this): before this issue, install wrote
/// `com.trusty.trusty-search.plist` and never touched the live unit, so failing
/// to reproduce a key was harmless. Now it overwrites `com.trusty.search.plist`,
/// which IS the live unit, and anything not reproduced is DESTROYED. The live
/// plist carried five keys no allowlist named —
/// `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS` among them, the hand-patch from an
/// incident where a restart cost a 200k-chunk index to a 30 s redb open timeout.
/// What: an env key the code has never heard of survives regeneration, and so
/// does the incident hand-patch specifically.
/// Test: pure — the env lookup is injected, so no process env is mutated.
#[cfg(target_os = "macos")]
#[test]
fn launchd_env_pairs_carries_forward_unanticipated_keys() {
    use crate::commands::service_unit::parse_installed_unit;
    use std::path::PathBuf;

    let existing = parse_installed_unit(
        "<key>EnvironmentVariables</key><dict>\
         <key>TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS</key><string>60</string>\
         <key>FASTEMBED_CACHE_DIR</key><string>/Users/x/.cache/fastembed</string>\
         <key>RUST_LOG</key><string>info</string>\
         <key>SOME_KEY_NOBODY_ANTICIPATED</key><string>keepme</string>\
         </dict>",
    );

    let pairs = launchd_env_pairs(Some(PathBuf::from("/Users/x")), |_| None, Some(&existing));
    let has = |k: &str, v: &str| pairs.iter().any(|(a, b)| a == k && b == v);

    assert!(
        has("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS", "60"),
        "dropping this re-arms the warm-boot index-loss incident, invisibly \
         until the next restart; got {pairs:?}"
    );
    assert!(has("FASTEMBED_CACHE_DIR", "/Users/x/.cache/fastembed"));
    assert!(has("RUST_LOG", "info"));
    assert!(
        has("SOME_KEY_NOBODY_ANTICIPATED", "keepme"),
        "an allowlist keeps losing the NEXT hand-set var — every key the unit \
         carried must survive; got {pairs:?}"
    );
}

/// Why: `HF_HOME` is recomputed at install time (#86) and `PATH` is seeded by
/// `with_daemon_path` (#1298). Carrying a stale value forward would let an old
/// unit outrank the freshly resolved one — the opposite failure from dropping.
/// What: the installed unit's stale `HF_HOME` loses to the computed one, and
/// appears exactly once.
/// Test: pure — the env lookup is injected.
#[cfg(target_os = "macos")]
#[test]
fn launchd_env_pairs_does_not_let_a_stale_template_key_win() {
    use crate::commands::service_unit::parse_installed_unit;
    use std::path::PathBuf;

    let existing = parse_installed_unit(
        "<key>EnvironmentVariables</key><dict>\
         <key>HF_HOME</key><string>/stale/hf</string></dict>",
    );
    let pairs = launchd_env_pairs(Some(PathBuf::from("/Users/x")), |_| None, Some(&existing));

    let hf: Vec<&(String, String)> = pairs.iter().filter(|(k, _)| k == "HF_HOME").collect();
    assert_eq!(hf.len(), 1, "HF_HOME must appear once, got {pairs:?}");
    assert_eq!(hf[0].1, "/Users/x/.cache/huggingface");
}

/// Why (#4868 review): the live unit carries a `WorkingDirectory` and the
/// generated template never emitted the key, so regeneration silently changed
/// the daemon's working directory.
/// What: a `WorkingDirectory` on the installed unit is parsed and rendered back.
/// Test: pure string generation, no fs side effects.
#[cfg(target_os = "macos")]
#[test]
fn build_launchd_config_carries_forward_working_directory() {
    use crate::commands::service_unit::parse_installed_unit;
    use std::path::PathBuf;

    let existing = parse_installed_unit(
        "<key>WorkingDirectory</key><string>/Users/x/work</string>\
         <key>EnvironmentVariables</key><dict></dict>",
    );
    assert_eq!(
        existing.working_directory.as_deref(),
        Some("/Users/x/work"),
        "the key must be parsed before it can be preserved"
    );

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        false,
        Some(&existing),
    );
    let xml = cfg.render_plist().expect("render_plist must succeed");
    assert!(
        xml.contains("<key>WorkingDirectory</key>\n  <string>/Users/x/work</string>"),
        "the regenerated unit must keep the working directory, got xml: {xml}"
    );
}

/// Why: a unit that never had a `WorkingDirectory` must not gain one.
/// What: no installed unit ⇒ the key is absent.
/// Test: pure string generation.
#[cfg(target_os = "macos")]
#[test]
fn build_launchd_config_omits_working_directory_by_default() {
    use std::path::PathBuf;

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        false,
        None,
    );
    let xml = cfg.render_plist().expect("render_plist must succeed");
    assert!(!xml.contains("WorkingDirectory"), "got xml: {xml}");
}

/// #4829: a freshly generated unit must carry `RUST_LOG=info`.
///
/// Why: launchd exec's the daemon with no shell environment. With `RUST_LOG`
/// unset, `trusty_common::tracing_init` filters at `"warn"` and every
/// `tracing::info!` the daemon writes about its own boot is dropped — the two
/// lines that confirm auto-discovery suppression among them, which is what made
/// the 2026-08-04 investigation unverifiable from production logs.
/// What: no installed unit, empty process env — the pairs still include
/// `RUST_LOG=info`.
/// Test: pure — the env lookup is injected, so no process env is mutated.
#[cfg(target_os = "macos")]
#[test]
fn launchd_env_pairs_defaults_rust_log_to_info() {
    use std::path::PathBuf;

    let pairs = launchd_env_pairs(Some(PathBuf::from("/Users/x")), |_| None, None);
    assert!(
        pairs.iter().any(|(k, v)| k == "RUST_LOG" && v == "info"),
        "#4829: a generated unit with no prior config must still log at INFO; \
         got {pairs:?}"
    );
}

/// #4829: the INFO default must never outrank an operator's own `RUST_LOG`.
///
/// Why: a default that overwrites explicit configuration is the #4868 failure
/// in reverse — an operator who set `debug` (or narrowed the filter to one
/// target) would silently get `info` back on every reinstall.
/// What: drives both override paths — a value the installed unit carried, and a
/// value exported in the installing shell — and asserts each survives, exactly
/// once.
/// Test: pure — the env lookup is injected.
#[cfg(target_os = "macos")]
#[test]
fn launchd_env_pairs_keeps_an_operator_rust_log() {
    use crate::commands::service_unit::parse_installed_unit;
    use std::path::PathBuf;

    let existing = parse_installed_unit(
        "<key>EnvironmentVariables</key><dict>\
         <key>RUST_LOG</key><string>trusty_search=debug</string>\
         </dict>",
    );
    let from_unit = launchd_env_pairs(Some(PathBuf::from("/Users/x")), |_| None, Some(&existing));
    let rust_log: Vec<&(String, String)> =
        from_unit.iter().filter(|(k, _)| k == "RUST_LOG").collect();
    assert_eq!(
        rust_log.len(),
        1,
        "RUST_LOG must appear exactly once, got {from_unit:?}"
    );
    assert_eq!(rust_log[0].1, "trusty_search=debug");

    let from_shell = launchd_env_pairs(
        Some(PathBuf::from("/Users/x")),
        |k| (k == "RUST_LOG").then(|| "warn".to_string()),
        None,
    );
    assert!(
        from_shell
            .iter()
            .any(|(k, v)| k == "RUST_LOG" && v == "warn"),
        "an exported RUST_LOG must win over the INFO default; got {from_shell:?}"
    );
}
