//! Shared hermetic test fixtures: temp directories (#3382) and a
//! guaranteed-dead loopback address (#4306/#4415).
//!
//! Why (dead loopback): see [`dead_loopback_url`] and [`DEAD_LOOPBACK_PORT`].
//! It lives here, beside `hermetic_temp_dir`, because it is the same kind of
//! thing — a fixture whose whole job is to remove an ambient dependency from
//! the tests that use it — and because three separate copies of it had already
//! drifted across the `tui` test modules.
//!
//! Why (temp dirs): bare `tempfile::TempDir::new()` resolves via `std::env::temp_dir()`,
//! which honors an inherited `$TMPDIR`. A test-invoking harness/sandbox set
//! `TMPDIR` to a real project directory (observed: `~/trusty-mpm-projects`),
//! so every bare `TempDir::new()` call in the suite deposited its mktemp
//! scaffold directly into that project tree instead of a scratch location —
//! 50 directories / ~167MB accumulated over ~24h with nothing reaping them.
//! What: [`hermetic_temp_dir`] is the one replacement for every bare
//! `TempDir::new()` in trusty-mpm's test code. It roots the directory under
//! [`real_system_tmp`] — the hardcoded OS temp path, chosen deliberately
//! over `std::env::temp_dir()` so an inherited `TMPDIR` can never redirect
//! test litter into a project tree again — and tags it with the
//! [`TEST_DIR_PREFIX`] so any directory that survives a hard-killed test
//! process (Drop-based cleanup cannot run after SIGKILL) is trivially
//! attributable and safe to sweep. [`sweep_stale_test_dirs`] runs once per
//! test process (via [`std::sync::Once`], triggered by the first
//! `hermetic_temp_dir()` call) and best-effort removes `tm-test-*`
//! directories older than a day, bounding leak growth without a background
//! daemon.
//! Test: `test_support::tests` below, including regression coverage for a
//! degenerate-`$HOME` false positive (`HOME=/tmp` or `HOME=/`, as seen in
//! root-container / OpenShift-style environments) that an earlier revision
//! of [`guard_against_project_tree`] panicked on unconditionally; see also
//! the TMPDIR-pollution proof in the #3382 PR description
//! (`TMPDIR=$HOME/... cargo test -p trusty-mpm provisioner` deposits nothing
//! under that tree).

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

/// RAII ownership of a real tmux session created by a test (#6116).
///
/// The same file backs the `tm` binary's copy of this module — see its own
/// docs for why one source file serves both targets.
#[path = "test_tmux_session.rs"]
pub(crate) mod tmux_session;

/// The loopback port every dead-daemon test points at (#4306, #4415).
///
/// Why this specific port, rather than one the fixture binds for itself: the
/// fixture needs an address that (a) refuses connections deterministically and
/// (b) cannot be taken over by any other binder. A privileged loopback port
/// satisfies both by construction, and — measured on this suite's two targets —
/// is the only shape that satisfies (a):
///
/// * **Refuses, immediately.** Nothing listens, so the kernel answers `RST` →
///   `ECONNREFUSED`. Measured on macOS: 30/30 connects refused, worst case
///   132µs. Contrast the obvious-looking alternative of binding a port and
///   deliberately NOT calling `listen(2)`: that reserves the port, but macOS
///   silently DROPS the `SYN` instead of resetting it, so the connect times out
///   (measured: `TimedOut` after a full 10s) rather than being refused — which
///   would convert every daemon-down test here into a slow one and hand the
///   error-classification tests a timeout where they assert on a connect error.
/// * **Unavailable to any other binder.** It is below 1024, so binding it needs
///   root / `CAP_NET_BIND_SERVICE` (measured: `PermissionDenied` for an
///   unprivileged bind of both `:1` and `:1023`), and it sits outside every
///   ephemeral range the OS allocates from (macOS 49152–65535, Linux
///   `ip_local_port_range` default 32768–60999), so it can never be handed out
///   as an ephemeral. Port 1 is IANA `tcpmux`, which has no modern
///   implementation.
///
/// What it replaces: three copies of a `dead_loopback_url()` helper
/// (`tui::project_ctl::poll::tests`, `tui::coordinator::tests`,
/// `tui::project_ctl::tests`) feeding 13 call sites, each of which obtained a
/// "known dead" address by binding an ephemeral port and then DROPPING the
/// listener. Dropping it returns the port to the OS ephemeral pool, so the
/// premise — "nothing can be listening here" — held only until some other
/// binder was handed that same port; then the connect SUCCEEDS and the test
/// asserts against a live socket (`classify_connect_error_is_transport`'s
/// `expect_err` panics, or the request fails at the HTTP layer and
/// misclassifies as `NonTransport`). #4415 observed exactly that failure.
/// Measured on macOS: a bind-and-drop port was reassigned to a competing
/// binder — and accepted a connection — after 16,103 ephemeral binds, i.e. one
/// wrap of the 49152–65535 range, which a full suite's port churn reaches.
const DEAD_LOOPBACK_PORT: u16 = 1;

/// Pin "privileged" at COMPILE time — it is a property of the constant, not of
/// any particular run, so a future edit that moves the port above 1024 (into
/// bindable, and eventually ephemeral, territory) must fail the build rather
/// than wait for a test to notice. Also what keeps
/// [`tests::dead_loopback_port_is_not_bindable_or_ephemeral`] free of a
/// `clippy::assertions_on_constants` lint.
const _: () = assert!(
    DEAD_LOOPBACK_PORT < 1024,
    "DEAD_LOOPBACK_PORT must stay below 1024 so binding it requires root and the \
     OS never hands it out as an ephemeral port (#4306/#4415)"
);

/// Verifies [`DEAD_LOOPBACK_PORT`]'s premise once per test process.
static DEAD_PORT_VERIFIED: Once = Once::new();

/// A `127.0.0.1` URL guaranteed to refuse every connection — the ONE
/// dead-address fixture for this crate (#4306, #4415).
///
/// Why: see [`DEAD_LOOPBACK_PORT`] for the full argument and the measurements.
/// What: returns `http://127.0.0.1:1`, having first confirmed (once per test
/// process) that the address really does refuse. That check turns the fixture's
/// one environmental assumption — "no process on this machine listens on
/// loopback port 1" — into an immediate, named failure instead of a confusing
/// downstream one: a `expect_err` panic or a `Transport`/`NonTransport`
/// misclassification several frames away, which is precisely the debugging
/// experience #4415 describes as training people to re-run rather than
/// investigate.
/// Test: [`tests::dead_loopback_url_refuses_every_connect`] and
/// [`tests::dead_loopback_port_is_not_bindable_or_ephemeral`] pin the two
/// properties, so the guarantee is verified rather than merely documented.
pub(crate) fn dead_loopback_url() -> String {
    DEAD_PORT_VERIFIED.call_once(|| {
        let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, DEAD_LOOPBACK_PORT));
        // A generous timeout: it bounds a pathological environment, it does not
        // define the expectation. The assertion is on the error KIND, so a
        // machine that refuses slowly still passes and one that accepts (or
        // silently drops) fails loudly, naming the remedy.
        match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
            Ok(_) => panic!(
                "test_support::dead_loopback_url(): something is LISTENING on {addr}, so the \
                 dead-address fixture's premise is void (#4306/#4415). Stop that listener, or \
                 pick another privileged, non-ephemeral loopback port for DEAD_LOOPBACK_PORT."
            ),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(e) => panic!(
                "test_support::dead_loopback_url(): {addr} neither accepted nor REFUSED the \
                 connect — it failed with {:?} ({e}) (#4306/#4415). The fixture requires a \
                 prompt ECONNREFUSED; a dropped SYN (TimedOut) would make every daemon-down \
                 test slow and would hand the error-classification tests the wrong error kind.",
                e.kind()
            ),
        }
    });
    format!("http://127.0.0.1:{DEAD_LOOPBACK_PORT}")
}

/// Prefix every hermetic test temp directory carries.
///
/// Why: `TempDir::drop` cannot run after a hard-killed test process (e.g. a
/// CI timeout `SIGKILL`), so some leakage is unavoidable. A stable, greppable
/// prefix makes any leaked directory trivially attributable to trusty-mpm's
/// test suite (vs. some other tool's temp files) and safe for
/// [`sweep_stale_test_dirs`] — or a human running `rm -rf` — to reap without
/// guessing.
pub(crate) const TEST_DIR_PREFIX: &str = "tm-test-";

/// How long a `tm-test-*` directory may sit in the hermetic root before
/// [`sweep_stale_test_dirs`] treats it as leaked and removes it.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

static SWEEP_ONCE: Once = Once::new();

/// Return the real, hardcoded OS temp root.
///
/// Why: deliberately NOT `std::env::temp_dir()`, which honors `$TMPDIR` —
/// the exact mechanism #3382's incident exploited. The chosen rule: on Unix
/// (macOS + Linux, the only targets trusty-mpm's test suite runs on — CI is
/// `ubuntu-latest`, local dev is macOS), always resolve to `/tmp`. It is the
/// one system scratch path that exists on every Unix trusty-mpm supports,
/// is never inside a user's home or project tree, and is NOT configurable
/// via any environment variable — so no inherited `TMPDIR`, however
/// polluted, can ever redirect it. On non-Unix targets (the Tauri GUI's
/// Windows builds only; no test suite runs there) this falls back to
/// `std::env::temp_dir()`, since Windows has no established TMPDIR-pollution
/// pattern and no equivalent always-present hardcoded system path.
/// What: returns `/tmp` on Unix, `std::env::temp_dir()` otherwise.
/// Test: [`tests::real_system_tmp_ignores_tmpdir_env`].
fn real_system_tmp() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/tmp")
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir()
    }
}

/// Whether `home` is a "meaningful" (non-degenerate) containment boundary
/// relative to `candidate` — more than one path component, and not
/// identical to `candidate` itself.
///
/// Why: shared by [`guard_against_project_tree`] AND its own test suite
/// (`tests::hermetic_temp_dir_is_prefixed_and_outside_home`) so both apply
/// the EXACT same degenerate-`$HOME` exclusion — a code reviewer reproduced
/// a real crash by running the compiled test binary with `HOME=/tmp` (or
/// `HOME=/`): every real system temp path trivially "starts with" `/`, and
/// `/tmp` trivially equals `HOME=/tmp`, so a naive `Path::starts_with` check
/// panicked on EVERY [`hermetic_temp_dir`] call in that environment —
/// strictly worse than the pre-fix behavior, not defense in depth. Having
/// the assertion logic live in one place prevents the test suite's own
/// containment check from drifting out of sync with the production check and
/// reintroducing the same false positive from the other direction.
/// What: `home.components().count() > 1 && home != candidate`.
fn is_meaningful_home_boundary(home: &Path, candidate: &Path) -> bool {
    home.components().count() > 1 && home != candidate
}

/// Panic loudly if `root` resolves inside the user's home directory.
///
/// Why: defense in depth. [`real_system_tmp`]'s hardcoded `/tmp` never trips
/// this in practice, but if that function is ever changed to consult an env
/// var again (or a future platform's fallback resolves somewhere
/// unexpected), a hermetic root that lands inside `$HOME` — where every
/// observed project tree lives — must fail the test run loudly rather than
/// silently littering it, per #3382.
/// What: compares `root` against `$HOME` with `Path::starts_with`, but ONLY
/// when [`is_meaningful_home_boundary`] says `$HOME` is non-degenerate
/// relative to `root`. No-ops if `$HOME` is unset.
/// Test: [`tests::guard_panics_on_genuine_home_containment`],
/// [`tests::guard_does_not_panic_when_home_is_tmp`],
/// [`tests::guard_does_not_panic_when_home_is_root`].
fn guard_against_project_tree(root: &Path) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let home = PathBuf::from(home);
    if is_meaningful_home_boundary(&home, root) && root.starts_with(&home) {
        panic!(
            "hermetic test temp root {root:?} resolves inside $HOME ({home:?}) — \
             refusing to risk littering a project tree (see #3382)"
        );
    }
}

/// Create a hermetic test `TempDir`, immune to inherited `$TMPDIR` pollution.
///
/// Why: the one replacement for every bare `TempDir::new()` call site
/// flagged by #3382 — rooting under [`real_system_tmp`] instead of
/// `env::temp_dir()` means a polluted `TMPDIR` can never again cause test
/// scaffolding to land in a user's project tree.
/// What: sweeps stale leaked directories once per test process (see
/// [`sweep_stale_test_dirs`]), then creates a `TempDir` under the hermetic
/// root with the [`TEST_DIR_PREFIX`] prefix.
/// Test: [`tests::hermetic_temp_dir_is_prefixed_and_outside_home`].
pub(crate) fn hermetic_temp_dir() -> TempDir {
    SWEEP_ONCE.call_once(sweep_stale_test_dirs);
    let root = real_system_tmp();
    guard_against_project_tree(&root);
    tempfile::Builder::new()
        .prefix(TEST_DIR_PREFIX)
        .tempdir_in(&root)
        .expect("create hermetic test temp dir")
}

/// Best-effort sweep of stale `tm-test-*` directories left behind by
/// hard-killed test processes.
///
/// Why: `TempDir::drop` cannot fire after a `SIGKILL`, so leaked directories
/// are an accepted possibility, not a bug to eliminate outright — #3382's
/// incident was 50 of them accumulating over ~24h with nothing reaping them.
/// A lightweight sweep at the start of a test process bounds that growth
/// without a background daemon, proportionate to how rare and small the
/// leaks actually are.
/// What: reads [`real_system_tmp`], and for every entry whose name starts
/// with [`TEST_DIR_PREFIX`] and whose modified time is more than
/// [`STALE_AFTER`] old, best-effort `remove_dir_all`s it. All errors
/// (permission, concurrent removal by another test process racing the same
/// sweep, a non-existent root) are silently ignored — this is hygiene, not a
/// correctness requirement, so it must never fail a test run.
/// Test: [`tests::sweep_removes_only_stale_prefixed_dirs`].
fn sweep_stale_test_dirs() {
    let root = real_system_tmp();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(TEST_DIR_PREFIX) {
            continue;
        }
        let is_stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if is_stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    /// `real_system_tmp` must ignore `$TMPDIR` even when it points somewhere
    /// that would otherwise cause litter (e.g. a project tree).
    #[test]
    fn real_system_tmp_ignores_tmpdir_env() {
        // Safety/portability note: this test only asserts the *return value*
        // is independent of TMPDIR; it does not mutate global env state.
        let root = real_system_tmp();
        #[cfg(unix)]
        assert_eq!(root, PathBuf::from("/tmp"));
        assert!(root.exists(), "hermetic root must exist: {root:?}");
    }

    /// Why serial: reads `$HOME` (via `guard_against_project_tree`, indirectly
    /// through `hermetic_temp_dir`) and asserts on it below. Must be
    /// serialized against every other test in this binary that mutates or
    /// reads `$HOME` for the same reason — same shared default group as
    /// `session_manager::workspace_guard::tests::is_safe_to_remove_rejects_home`
    /// (#2461 sweep) and the three `HomeOverride`-mutating tests below; a
    /// code reviewer noted the mutating tests' own `Mutex` only serializes
    /// them against each other, not against `$HOME`-reading tests elsewhere,
    /// which `#[serial]`'s shared default group closes.
    #[serial_test::serial]
    #[test]
    fn hermetic_temp_dir_is_prefixed_and_outside_home() {
        let dir = hermetic_temp_dir();
        let name = dir
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        assert!(
            name.starts_with(TEST_DIR_PREFIX),
            "expected {name:?} to start with {TEST_DIR_PREFIX:?}"
        );
        // Same degenerate-HOME exclusion as `guard_against_project_tree`
        // (compared against the ROOT, not the leaf `dir.path()` — otherwise
        // this assertion reintroduces the exact false positive it's meant to
        // catch: `dir.path()` is always a child of, never equal to, the
        // root, so a naive `home != dir.path()` check is never degenerate).
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            if is_meaningful_home_boundary(&home, &real_system_tmp()) {
                assert!(
                    !dir.path().starts_with(&home),
                    "hermetic dir must never resolve inside $HOME: {:?}",
                    dir.path()
                );
            }
        }
    }

    /// RAII guard that overrides `$HOME` for the duration of a `#[serial]`
    /// test and restores the prior value on drop (including on panic-driven
    /// unwind, via the `#[should_panic]` test below) — mirrors
    /// `core::session_launch::tests::EnvVarGuard`'s established pattern in
    /// this crate.
    ///
    /// Why NOT an internal `Mutex` (a code reviewer flagged an earlier
    /// revision that had one): a `Mutex` scoped to this struct only
    /// serializes `HomeOverride`-using tests against EACH OTHER, not against
    /// every other test in this binary that reads `$HOME` without going
    /// through this guard — e.g.
    /// [`hermetic_temp_dir_is_prefixed_and_outside_home`] above, or
    /// `session_manager::workspace_guard::tests::is_safe_to_remove_rejects_home`.
    /// `#[serial_test::serial]`'s shared default group, tagged on every one
    /// of those tests, closes that gap crate-wide instead of only locally.
    struct HomeOverride {
        prev: Option<std::ffi::OsString>,
    }

    impl Drop for HomeOverride {
        fn drop(&mut self) {
            // SAFETY: every caller of `override_home` is `#[serial]`, so no
            // other test thread races this set/restore.
            match self.prev.take() {
                Some(v) => unsafe { std::env::set_var("HOME", v) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    /// Set `$HOME` to `value`, returning a guard that restores it on drop.
    /// Callers MUST be tagged `#[serial_test::serial]` — see [`HomeOverride`].
    fn override_home(value: &str) -> HomeOverride {
        let prev = std::env::var_os("HOME");
        // SAFETY: caller is `#[serial]`.
        unsafe { std::env::set_var("HOME", value) };
        HomeOverride { prev }
    }

    /// The genuine positive case: a real per-user home (e.g. `/Users/x`,
    /// `/home/x`) containing the candidate root must still panic.
    ///
    /// Why serial: mutates `$HOME` via [`override_home`] — see
    /// [`HomeOverride`] for why serialization must be crate-wide, not a
    /// locally scoped lock.
    #[serial_test::serial]
    #[test]
    #[should_panic(expected = "resolves inside $HOME")]
    fn guard_panics_on_genuine_home_containment() {
        let _home = override_home("/Users/test-user");
        guard_against_project_tree(&PathBuf::from("/Users/test-user/trusty-mpm-projects"));
    }

    /// Regression for the false-positive a code reviewer reproduced: with
    /// `HOME=/tmp` (root-container / OpenShift-style environments),
    /// `real_system_tmp()`'s `/tmp` trivially equals `$HOME`, so a naive
    /// `starts_with` check panicked on EVERY `hermetic_temp_dir()` call —
    /// strictly worse than the pre-fix behavior. Must be a no-op.
    ///
    /// Why serial: see [`guard_panics_on_genuine_home_containment`].
    #[serial_test::serial]
    #[test]
    fn guard_does_not_panic_when_home_is_tmp() {
        let _home = override_home("/tmp");
        guard_against_project_tree(&PathBuf::from("/tmp"));
    }

    /// Regression for the same false-positive with `HOME=/`: every absolute
    /// path trivially "starts with" `/`, so a naive check panicked on every
    /// `hermetic_temp_dir()` call. Must be a no-op.
    ///
    /// Why serial: see [`guard_panics_on_genuine_home_containment`].
    #[serial_test::serial]
    #[test]
    fn guard_does_not_panic_when_home_is_root() {
        let _home = override_home("/");
        guard_against_project_tree(&PathBuf::from("/tmp"));
    }

    #[test]
    fn sweep_removes_only_stale_prefixed_dirs() {
        let root = real_system_tmp();
        let stale = root.join(format!(
            "{TEST_DIR_PREFIX}sweep-stale-{}",
            std::process::id()
        ));
        let fresh = root.join(format!(
            "{TEST_DIR_PREFIX}sweep-fresh-{}",
            std::process::id()
        ));
        let unrelated = root.join(format!("not-tm-prefixed-{}", std::process::id()));
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();

        // Back-date the "stale" directory's mtime by 2 days (std::fs::FileTimes,
        // stable since 1.75 — no need for an extra crate dependency here).
        let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        let times = std::fs::FileTimes::new().set_modified(two_days_ago);
        std::fs::File::open(&stale)
            .unwrap()
            .set_times(times)
            .unwrap();

        sweep_stale_test_dirs();

        assert!(!stale.exists(), "stale tm-test- dir should be swept");
        assert!(fresh.exists(), "fresh tm-test- dir should survive");
        assert!(unrelated.exists(), "non-prefixed dir must never be touched");

        // Clean up what the sweep left behind (this test writes directly
        // under the real /tmp, not a TempDir, so nothing auto-removes them).
        let _ = std::fs::remove_dir_all(&fresh);
        let _ = std::fs::remove_dir_all(&unrelated);
        let _ = std::fs::remove_dir_all(&stale);
    }

    /// #4306/#4415 property 1 of 2: the address must answer `ECONNREFUSED`,
    /// and must do so on EVERY attempt.
    ///
    /// Deliberately asserts the error KIND, not merely that the connect failed.
    /// That distinction is load-bearing and was not academic: the first attempt
    /// at this fixture reserved a port by binding it without `listen(2)`, which
    /// looked correct and which a "did it fail?" assertion would have passed —
    /// macOS silently drops the `SYN` there, so connects TIME OUT rather than
    /// being refused. `classify_connect_error_is_transport` would still have
    /// gone green (reqwest maps both to `Transport`) while every daemon-down
    /// test quietly became a multi-second one. Asserting the kind is what
    /// caught it.
    #[test]
    fn dead_loopback_url_refuses_every_connect() {
        let addr: std::net::SocketAddr = dead_loopback_url()
            .trim_start_matches("http://")
            .parse()
            .expect("the fixture's url must be host:port after the scheme");

        for attempt in 0..50 {
            let err = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(10))
                .expect_err("nothing may be listening on the dead-address port");
            assert_eq!(
                err.kind(),
                std::io::ErrorKind::ConnectionRefused,
                "attempt {attempt} to {addr} must be REFUSED (RST), not dropped: {err:?}"
            );
        }
    }

    /// #4306/#4415 property 2 of 2: the port must be unavailable to any other
    /// binder. This is the half the old bind-and-drop fixture lacked, and the
    /// direct cause of the observed flake — so it is asserted, not assumed.
    ///
    /// Two independent guarantees, checked separately because either one alone
    /// would be enough to break if a future edit moved `DEAD_LOOPBACK_PORT`
    /// into the ephemeral range or above 1024.
    #[test]
    fn dead_loopback_port_is_not_bindable_or_ephemeral() {
        // (a) Privileged: an unprivileged bind must be refused outright, so no
        //     test — nor any other unprivileged process — can start listening.
        // "privileged" itself is pinned at compile time next to the constant.
        let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, DEAD_LOOPBACK_PORT));
        assert!(
            std::net::TcpListener::bind(addr).is_err(),
            "an unprivileged bind of {addr} must fail — a bindable dead-address port is \
             exactly the invalidated premise #4306 describes"
        );

        // (b) Never ephemeral: the OS must not hand this port to a competing
        //     binder. 2,000 binds is a cheap sanity sweep rather than a proof
        //     (the proof is that the port is below every ephemeral range); it
        //     exists to catch a future edit that moves the constant.
        for _ in 0..2_000 {
            if let Ok(other) = std::net::TcpListener::bind("127.0.0.1:0") {
                assert_ne!(
                    other.local_addr().expect("local addr").port(),
                    DEAD_LOOPBACK_PORT,
                    "the OS handed a competing binder the dead-address port"
                );
            }
        }
    }

    /// Guards against `UNIX_EPOCH` underflow bugs in age math (belt-and-braces).
    #[test]
    fn stale_after_is_positive_duration() {
        assert!(STALE_AFTER > Duration::from_secs(0));
        assert!(SystemTime::now().duration_since(UNIX_EPOCH).is_ok());
    }
}
