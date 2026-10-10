//! The generated launchd unit carries no retired #9214 input.
//!
//! Why: the daemon binds no TCP port, so a regenerated unit must not keep
//! `--port`, `--no-http` or `TRUSTY_SEARCH_NO_HTTP` — an installed unit
//! (written by an older build or by claude-mpm's setup) may carry any of
//! them, and regeneration carries forward every env key it finds (#4868).
//! What: drives the two pure seams the macOS generator calls, on every
//! platform, so the rule is tested where CI runs.
//! Test: this module — `cargo test -p trusty-search --bin trusty-search
//! retired_unit_tests`.

use super::{unit_program_args, without_retired_env};

/// Why (#9214): the regenerated unit is what launchd runs on every boot; a
/// retired input left in it warns forever and pins nothing.
/// What: an env set holding `TRUSTY_SEARCH_NO_HTTP` beside an ordinary
/// tunable loses only the retired key; the unit's arguments, with and without
/// the auto-discovery flag, hold no `--port` and no `--no-http`.
/// Test: this function.
#[test]
fn generated_unit_carries_no_retired_port_input() {
    let carried = vec![
        ("TRUSTY_SEARCH_NO_HTTP".to_string(), "1".to_string()),
        ("RUST_LOG".to_string(), "info".to_string()),
    ];
    let env = without_retired_env(carried);
    assert_eq!(
        env,
        vec![("RUST_LOG".to_string(), "info".to_string())],
        "the retired key must be dropped and nothing else"
    );
    for suppress in [false, true] {
        let args = unit_program_args(suppress);
        assert_eq!(&args[..2], &["start", "--foreground"], "{args:?}");
        assert!(
            !args
                .iter()
                .any(|a| a.starts_with("--port") || a.starts_with("--no-http")),
            "the unit must not carry a retired flag: {args:?}"
        );
        assert_eq!(args.iter().any(|a| a == "--no-auto-discover"), suppress);
    }
}
