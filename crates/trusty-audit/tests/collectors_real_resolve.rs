//! `collectors::missing`/`check` against the REAL `resolve_binary`, in its own
//! process.
//!
//! Why (#7134 fix-round item, code-critic on PR #7170 at `33f27d443`):
//! `crate::collectors::missing_resolved_by`'s doc comment already names the
//! hazard — mutating the real, process-global `PATH` to prove "missing" races
//! every OTHER test in the SAME binary that also resolves a binary
//! (`crate::run::pins`, `crate::git`, …), and `#[serial_test::serial]` only
//! orders tests against each other, never against a plain `#[test]` that
//! never opted in. An earlier version of this proof lived in `collectors.rs`'s
//! own `#[cfg(test)] mod collectors_tests` — inside the crate's `--lib` test
//! binary — and intermittently broke
//! `clone::clone_tests::two_paths_with_one_basename_are_refused_together`
//! with `"git is on PATH for this suite" … NotFound`. Cargo builds every file
//! under `tests/` as its OWN process, so a file with exactly one test here has
//! no sibling in its process to race, ever, at any `--test-threads`.
//!
//! What: prepends a directory holding stand-in `gitleaks`/`cargo-audit`/
//! `cargo-deny` executables to this process's OWN live `PATH` — never
//! replacing it, so nothing here could hide a binary another process needs —
//! and confirms the real `trusty_common::bin_resolve::resolve_binary`, reached
//! through the crate's public `collectors::missing`/`collectors::check`,
//! finds every one of them. The resolver-injected seam
//! (`collectors_tests::missing_resolved_by`-backed tests) and the pure
//! warn/refuse judgement (`collectors_tests::decide_*`) stay in `collectors.rs`
//! — this file's only job is proving the REAL resolver is actually wired in.
//!
//! Test: this is the test.

use trusty_audit::collectors::{self, ALL};

/// Creates an executable file `dir/name` (`chmod +x` on Unix).
fn touch_executable(dir: &std::path::Path, name: &str) {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\n").expect("write fake binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod +x");
    }
}

#[test]
fn missing_and_check_go_through_the_real_resolve_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    for c in ALL {
        touch_executable(dir.path(), c.binary);
    }
    let previous = std::env::var_os("PATH");
    let mut dirs = vec![dir.path().to_path_buf()];
    if let Some(p) = &previous {
        dirs.extend(std::env::split_paths(p));
    }
    let prepended = std::env::join_paths(dirs).expect("join_paths");
    // SAFETY (test-only, sole `#[test]` in this file's own process — see the
    // module docs for why that makes this safe with no `#[serial]` needed):
    // restored immediately below, and the change is additive so even a
    // between-process interleaving (there is none here) could not hide a
    // binary anything else needs.
    unsafe {
        std::env::set_var("PATH", &prepended);
    }
    let found = collectors::missing();
    let checked = collectors::check(false);
    unsafe {
        match &previous {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
    }

    assert!(
        found.is_empty(),
        "the real resolve_binary must find every stand-in on PATH: {found:?}"
    );
    assert!(
        checked
            .expect("nothing missing, so check does not refuse")
            .is_empty()
    );
}
