//! CLI-surface tests for the `trusty-analyze` binary.
//!
//! Why: `main.rs` sits near the 500-SLOC production cap, and a `_tests.rs`
//! sibling is classified as a test file (3000-SLOC cap) so coverage can grow
//! without pushing the router over it.
//!
//! What: parser-level assertions about `Cmd::Serve`'s argument surface.
//!
//! Test: this module.

use super::*;

/// Why (#5067): removing the neural embedder removed the only reason
/// `--fastembed-cache` existed, but an installed launchd plist or shell
/// wrapper may still pass it — and clap rejects an unknown argument before
/// `main` runs, which would convert a fixed startup stall into a startup
/// failure. Keeping the flag as a no-op is this change's only backward
/// compatibility promise, so it is asserted rather than merely documented.
/// What: `serve --fastembed-cache <path>` still parses and still carries the
/// value; the flag is hidden from `--help` so nothing advertises it to new
/// callers; and `TRUSTY_FASTEMBED_CACHE` remains bound so a plist setting the
/// environment variable instead of the flag also keeps starting.
/// Test: this test.
#[test]
fn serve_accepts_deprecated_fastembed_cache_flag() {
    let cli = Cli::try_parse_from([
        "trusty-analyze",
        "serve",
        "--fastembed-cache",
        "/var/cache/fastembed",
    ])
    .expect("`--fastembed-cache` must still parse — existing plists pass it");

    match cli.cmd {
        Cmd::Serve {
            fastembed_cache, ..
        } => assert_eq!(
            fastembed_cache.as_deref(),
            Some(std::path::Path::new("/var/cache/fastembed")),
            "the flag must still bind a value, not be silently dropped by clap"
        ),
        other => panic!("expected Cmd::Serve, got {other:?}"),
    }

    let serve = Cli::command()
        .get_subcommands()
        .find(|c| c.get_name() == "serve")
        .expect("serve subcommand")
        .clone();
    let arg = serve
        .get_arguments()
        .find(|a| a.get_id() == "fastembed_cache")
        .expect("fastembed_cache argument");
    assert!(
        arg.is_hide_set(),
        "the flag is accepted for compatibility only; it must not be advertised in --help"
    );
    assert_eq!(
        arg.get_env(),
        Some(std::ffi::OsStr::new("TRUSTY_FASTEMBED_CACHE")),
        "a plist that sets the env var instead of the flag must also keep starting"
    );
}

/// Why (#5067): the compatibility flag must be optional — the common case is
/// an invocation that never mentions it, and a `default_value` would have made
/// the parsed value indistinguishable from one the operator supplied.
/// What: plain `serve` parses with `fastembed_cache: None`.
/// Test: this test.
#[test]
fn serve_without_fastembed_cache_parses_to_none() {
    let cli = Cli::try_parse_from(["trusty-analyze", "serve"]).expect("bare serve must parse");
    match cli.cmd {
        Cmd::Serve {
            fastembed_cache, ..
        } => assert!(fastembed_cache.is_none()),
        other => panic!("expected Cmd::Serve, got {other:?}"),
    }
}
