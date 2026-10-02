//! Tests for the shutdown legacy-registry pass (#8942, #9101). Hermetic: tmux
//! resolves to a scripted fake that logs its argv.

use super::skip_legacy_names;

/// #9101: a legacy registry entry is a bare tmux name, so the shutdown pass
/// kills none of them — not the Architect's names (#8942), and not an
/// ordinary one, which a restarted tmux server may have handed to an
/// unrelated session. No tmux call runs at all.
#[serial_test::serial]
#[tokio::test]
async fn the_shutdown_legacy_pass_kills_no_legacy_session() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::test_support::hermetic_temp_dir();
    let log = dir.path().join("calls.log");
    let bin = dir.path().join("fake-tmux");
    let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display());
    std::fs::write(&bin, script).expect("write fake tmux");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let names = ["tm-arch", "tm-arch-poll", "tm-work"].map(String::from);
    let skipped =
        crate::core::tmux::with_tmux_binary(bin, async { skip_legacy_names(names) }).await;

    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(skipped, 3, "every legacy name is reported as left running");
    assert!(calls.is_empty(), "the legacy pass ran tmux: {calls}");
}
