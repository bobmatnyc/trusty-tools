//! Fail-loud inventory of every `launchctl bootstrap`-issuing site (#4470).
//!
//! Why: #4470's first round enumerated the bootstrap sites by hand, gated the
//! ones it found, and shipped. Review found a fifth
//! (`plist_bootstrap::install_mpm_supervisor`) that the enumeration had missed
//! and that nothing failed on — the guard simply did not apply there. That is
//! the same shape as #3841, where a fix applied to some branches left exactly
//! the damaged machines on an ungated one.
//!
//! A fix that enumerates call sites is incomplete until something FAILS when
//! the enumeration is wrong. This is that something, following the allowlist
//! pattern PR #4475 established for `claude` launch lines: rather than
//! asserting the known sites are present (which notices nothing when a NEW one
//! appears), it enumerates what is actually in the source and fails on anything
//! unaccounted for. Adding a sixth bootstrap site is therefore a build failure
//! carrying instructions, not a silent bypass.
//!
//! Honest about its reach, because a coverage test that overstates itself is
//! the defect it guards against: it matches bootstrap invocations LITERALLY —
//! a `.bootstrap(` method call or a `"bootstrap"` argv literal. A site that
//! reaches launchd through some future indirection carries neither and is
//! invisible here. What this catches is the shape that actually went wrong:
//! someone adding another direct bootstrap call.

/// Every file under `src/` that issues or abstracts a `launchctl bootstrap`,
/// with its literal hit count and why the count is what it is.
///
/// Update this ONLY together with the guard wiring — see the failure message.
const ALLOWLISTED_BOOTSTRAP_SITES: &[(&str, usize, &str)] = &[
    (
        "commands/plist_bootstrap.rs",
        2,
        "the trusty-mpm supervisor: `RealLaunchctl::bootstrap`'s `launchctl` argv, and \
         `install_mpm_supervisor_for`'s `target.launchctl.bootstrap(...)`. GATED by \
         `LaunchctlPort::port_guard` placed before BOTH the bootout and the plist \
         write, so a refusal changes nothing (#4470 HIGH-2 — the site the first round \
         of the fix missed)",
    ),
    (
        "commands/service_bootstrap.rs",
        1,
        "`RealServiceEnv::bootstrap_fallback`'s `cfg.bootstrap()`. Reached only via \
         `bootstrap_one`, whose fresh-install and #3841 already-present branches are \
         BOTH gated by `ServiceEnv::port_guard`, and via `verify_tail`'s second-layer \
         repair, which is gated by the same seam",
    ),
    (
        "commands/lifecycle.rs",
        2,
        "`launchd_control`'s Start and Restart `cfg.bootstrap()` calls; both GATED by \
         `port_guard::guard_bootstrap`, Restart's placed before its bootout",
    ),
];

/// Fails when a `launchctl bootstrap` site appears anywhere under `src/` that
/// is not accounted for above.
///
/// Test: this is the test.
#[test]
fn every_launchctl_bootstrap_site_is_allowlisted_and_gated() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut stack = vec![src.clone()];

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src must be readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // This file necessarily contains every pattern it searches for, in
            // the allowlist's reasons and the failure message. Test files hold
            // no production bootstrap site, so excluding them costs no
            // coverage — the production sites they exercise are still counted
            // in their own modules.
            if name.ends_with("_tests.rs") || name == "tests.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("source must be readable");
            let hits = text
                .lines()
                // Prose in a doc comment is not a bootstrap call. Filtering
                // comments is what keeps this from failing on its own
                // explanations — and on the many `#4470` notes this PR added.
                .filter(|line| {
                    let t = line.trim_start();
                    !(t.starts_with("//") || t.starts_with("*"))
                })
                .filter(|line| line.contains(".bootstrap(") || line.contains(r#""bootstrap""#))
                .count();
            if hits > 0 {
                let rel = path
                    .strip_prefix(&src)
                    .expect("under src")
                    .to_string_lossy()
                    .replace('\\', "/");
                found.insert(rel, hits);
            }
        }
    }

    let expected: std::collections::BTreeMap<String, usize> = ALLOWLISTED_BOOTSTRAP_SITES
        .iter()
        .map(|(f, n, _)| ((*f).to_owned(), *n))
        .collect();

    assert_eq!(
        found, expected,
        "\nA `launchctl bootstrap` site changed.\n\
         If you ADDED one: gate it with `commands::port_guard::guard_bootstrap` (for a \
         stable-set member) or `guard_bootstrap_label` (for anything else), placing the \
         gate BEFORE any bootout or filesystem write so a refusal changes nothing — then \
         update ALLOWLISTED_BOOTSTRAP_SITES.\n\
         An ungated bootstrap reports success while a foreign process keeps serving the \
         port (#4470 / #4230).\n\
         If it is not a bootstrap site, add it with a reason saying why.\n\
         found:    {found:#?}\n\
         expected: {expected:#?}"
    );
}
