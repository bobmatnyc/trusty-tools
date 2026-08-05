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

/// Why (#4868): the whole point of making install label-correct is that the
/// plist fixes reach the live unit. #4868's own `ExitTimeOut` was written
/// into a plist under a label nothing loaded, so the daemon kept launchd's
/// 5 s default. This asserts the unit a normal install now activates
/// carries that key.
/// What: renders the config `service install` builds and requires
/// `ExitTimeOut` to be present and above the 5 s default it replaces.
/// Test: pure string generation, no fs side effects.
#[cfg(target_os = "macos")]
#[test]
fn installed_unit_carries_the_exit_timeout_fix() {
    use std::path::PathBuf;

    let cfg = build_launchd_config(
        PathBuf::from("/usr/local/bin/trusty-search"),
        PathBuf::from("/tmp/trusty-search/logs"),
        false,
        None,
    );
    let xml = cfg.render_plist().expect("render_plist must succeed");
    assert!(
        xml.contains(&format!(
            "<key>ExitTimeOut</key>\n  <integer>{}</integer>",
            trusty_common::shutdown::TERMINATION_GRACE_SECS
        )),
        "the unit `service install` activates must carry #4868's \
         ExitTimeOut, got xml: {xml}"
    );
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
