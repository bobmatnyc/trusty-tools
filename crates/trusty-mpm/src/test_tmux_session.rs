//! RAII ownership of a REAL tmux session created by a test (#6116).
//!
//! Why: three fixtures in this crate shelled out to `tmux new-session` and
//! cleaned up with a bare `kill-session` placed after the code under test. That
//! kill is skipped whenever anything between the two panics, and one of those
//! fixtures panics by design — `wait_for_stable_dead_runtime` gives up at a 10s
//! deadline. tmux sessions are machine-global and outlive the test process, so
//! every panicked or cancelled run left one behind permanently: 29 `tm-deadrt*`
//! sessions accumulated on the owner's machine over 30 hours. #1790 isolated the
//! session STORE and said nothing about raw tmux spawns; this module is the
//! missing half.
//!
//! What: [`ScratchTmuxSession`] owns the session it created and kills it in
//! `Drop`, so normal return, `?`, assertion failure, and unwinding panic all
//! tear it down. It targets tmux with the `=<name>` EXACT-match form rather
//! than the bare `<name>` the old call sites used — a bare target falls through
//! to prefix and `fnmatch` matching, so `kill-session -t tm-deadrt99-01` will
//! happily destroy `tm-deadrt99-01-sibling` when the exact name is gone.
//! Verified on tmux 3.6b: with only the sibling alive, the bare form killed it
//! and the `=` form left it untouched.
//!
//! This file is compiled into TWO targets — the `trusty-mpm` lib and the `tm`
//! binary — through a `#[path]` include from each one's `test_support` module,
//! because a fixture reachable from only one of them would have to be written
//! twice, and a kill-on-drop guardrail that exists in two copies is one edit
//! away from drifting. The binary it drives is passed in rather than resolved
//! here for the same reason: `core::tmux::resolve_tmux_binary_or_bare` is
//! spelled `crate::…` in the lib and `trusty_mpm::…` in the bin, and no single
//! spelling compiles in both.
//!
//! **What `Drop` cannot cover, and what does.** A `Drop` guard runs on an
//! unwinding panic and on nothing else: a SIGKILL, a `cargo test` timeout, an
//! aborted run and a killed terminal all end the process without unwinding, so
//! the session survives. Three did on 2026-08-24. Two mechanisms cover that gap,
//! and neither is this guard:
//!
//! * The daemon refuses to adopt any session named under
//!   [`trusty_common::session_naming::RESERVED_TEST_PREFIX`] (#6116), so a
//!   leaked one never becomes a record or a picker row, and stays visible to
//!   `daemon::orphan_gc`, which kills an idle shell with no live child.
//!   [`reserved_session_name`] and [`reserved_project_slug`] are the two ways a
//!   fixture lands in that namespace — both build from the same constant the
//!   daemon reads, so the mint and the refusal cannot drift.
//! * [`ScratchTmuxSession::spawn`] sweeps stale reserved-namespace sessions
//!   once per test process, so the NEXT run on this machine reaps what the last
//!   one leaked. It is bounded by age
//!   ([`STALE_SESSION_AGE_SECS`]) so a concurrently running suite is never
//!   touched. What it does NOT do: reap anything before that next run happens,
//!   reap on a machine where the suite never runs again, or help at all if tmux
//!   is unreachable. It reduces how long a leak lives; the daemon-side refusal
//!   is what makes the leak harmless while it does.
//!
//! Test: [`tests`] below — a session that outlives a panicking closure is gone
//! afterwards, a guard never touches a name it did not create, and the stale
//! sweep selects by age and namespace.

use std::process::Command;
use std::sync::Once;

/// Age past which a reserved-namespace session is certainly leaked (#6116).
///
/// Why: no test in this suite holds a tmux session for anything close to half
/// an hour, so a survivor this old cannot belong to a run still in progress —
/// which is what makes the sweep safe to run while sibling test binaries and
/// other engineers' `cargo test` processes are using the same tmux server.
const STALE_SESSION_AGE_SECS: i64 = 1_800;

/// Runs [`sweep_stale_reserved_sessions`] once per test process.
static SWEEP_ONCE: Once = Once::new();

/// A tmux session name in the namespace the daemon refuses to adopt (#6116).
///
/// What: `tm-xtest-<tag>-<pid>`, unique per process so the two targets this
/// file compiles into cannot collide in the machine-global tmux namespace when
/// cargo runs them concurrently.
/// Test: [`tests::a_minted_name_is_one_the_daemon_refuses`].
pub(crate) fn reserved_session_name(tag: &str) -> String {
    format!(
        "{}{tag}-{}",
        trusty_common::session_naming::RESERVED_TEST_PREFIX,
        std::process::id()
    )
}

/// The repo/project slug a fixture passes to `create_with_id` so the tmux name
/// the daemon DERIVES lands in the reserved namespace (#6116).
///
/// Why: a fixture that seeds a record does not choose its `tmux_name` —
/// `build_managed_session_name` derives `tm-<project>-NN` from the repo url. So
/// the namespace has to be reached through the project slug, not by naming the
/// session directly.
/// What: the reserved prefix with `tm-` and the trailing dash removed
/// (`xtest`), joined to `tag`. Feed it as the repo segment of a url and the
/// derived name is `tm-xtest-<tag>-NN`.
/// Test: [`tests::a_derived_project_slug_yields_a_reserved_name`]; end to end
/// by `session_resume_headless_dead_runtime_reconciles_and_restarts`, which
/// asserts the name it actually got is reserved.
pub(crate) fn reserved_project_slug(tag: &str) -> String {
    let leaf = trusty_common::session_naming::RESERVED_TEST_PREFIX
        .strip_prefix(trusty_common::session_naming::PREFIX)
        .unwrap_or(trusty_common::session_naming::RESERVED_TEST_PREFIX)
        .trim_end_matches('-');
    format!("{leaf}-{tag}")
}

/// Pick the reserved-namespace sessions in a `tmux list-sessions` listing that
/// are older than `max_age_secs`.
///
/// Why: the selection is the whole risk in the sweep — killing a session a
/// concurrent run still needs would be worse than the leak it cleans up — so it
/// is a pure function over the listing text, testable without a tmux server.
/// What: reads `<created-epoch> <session-name>` lines (the format
/// [`sweep_stale_reserved_sessions`] requests, created first because a session
/// name may contain spaces), and keeps names in the reserved namespace whose
/// age exceeds `max_age_secs`. An unparseable line is skipped, never killed.
/// Test: [`tests::stale_sweep_selects_only_old_reserved_sessions`].
fn stale_reserved_names(listing: &str, now_epoch: i64, max_age_secs: i64) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| line.trim_end().split_once(' '))
        .filter_map(|(created, name)| Some((created.trim().parse::<i64>().ok()?, name)))
        .filter(|(created, name)| {
            now_epoch - created > max_age_secs
                && trusty_common::session_naming::is_reserved_test_session_name(name)
        })
        .map(|(_, name)| name.to_string())
        .collect()
}

/// Kill reserved-namespace sessions left behind by an earlier, hard-killed test
/// process (#6116).
///
/// Why/What: see this module's docs — `Drop` cannot run after a SIGKILL, so the
/// next run cleans up instead. Best-effort throughout: a tmux that will not
/// answer (no server yet, no binary) simply sweeps nothing.
/// Test: the selection half via [`stale_reserved_names`]; the kill half is the
/// same `kill-session -t =<name>` call [`ScratchTmuxSession::drop`] makes.
fn sweep_stale_reserved_sessions(tmux_bin: &str) {
    let Ok(out) = Command::new(tmux_bin)
        .args(["list-sessions", "-F", "#{session_created} #{session_name}"])
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    for name in stale_reserved_names(
        &String::from_utf8_lossy(&out.stdout),
        now,
        STALE_SESSION_AGE_SECS,
    ) {
        eprintln!("test-support: killing leaked test tmux session '{name}' (#6116)");
        let _ = Command::new(tmux_bin)
            .args(["kill-session", "-t", &format!("={name}")])
            .output();
    }
}

/// A real tmux session owned by a test, killed when this value drops.
///
/// Why: see the module docs — the panic path, not the happy path, is the whole
/// point.
/// What: [`ScratchTmuxSession::spawn`] creates the session and takes ownership
/// of exactly the name it passed to `new-session`; `Drop` kills that one name.
/// A failed `new-session` panics instead of yielding a guard, so a guard never
/// claims a session it did not create.
/// Test: [`tests::drop_kills_the_session_even_when_the_body_panics`],
/// [`tests::drop_kills_only_the_exact_name_it_created`].
pub(crate) struct ScratchTmuxSession {
    tmux_bin: String,
    name: String,
}

impl ScratchTmuxSession {
    /// Create `name` as a detached tmux session running `pane_command`.
    ///
    /// Panics if `new-session` cannot be spawned or exits non-zero. A duplicate
    /// name is one such non-zero exit, which is the behaviour that keeps the
    /// ownership claim honest: the caller gets no guard, so nothing later kills
    /// a session someone else created.
    pub(crate) fn spawn(tmux_bin: &str, name: &str, pane_command: &str) -> Self {
        // #6116: reap what an earlier hard-killed run leaked, before adding to
        // the namespace. Once per process, mirroring `test_support`'s
        // `sweep_stale_test_dirs`.
        SWEEP_ONCE.call_once(|| sweep_stale_reserved_sessions(tmux_bin));
        let status = Command::new(tmux_bin)
            .args(["new-session", "-d", "-s", name, pane_command])
            .status()
            .unwrap_or_else(|e| panic!("spawn `{tmux_bin} new-session -s {name}`: {e}"));
        assert!(
            status.success(),
            "`{tmux_bin} new-session -d -s {name}` failed with {status}; the session was NOT \
             created, so no guard may claim it"
        );
        Self {
            tmux_bin: tmux_bin.to_string(),
            name: name.to_string(),
        }
    }

    /// The exact session name this guard owns.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Whether a session named exactly `name` is live right now.
    ///
    /// `=` pins tmux to an exact match; see the module docs for why the bare
    /// form is unsafe here.
    pub(crate) fn exists(tmux_bin: &str, name: &str) -> bool {
        Command::new(tmux_bin)
            .args(["has-session", "-t", &format!("={name}")])
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// Whether `tmux_bin` runs at all, for fixtures that skip rather than fail
    /// where tmux is absent (the convention `core::process`'s ignored
    /// disclaim-wrapper test already follows).
    pub(crate) fn tmux_available(tmux_bin: &str) -> bool {
        Command::new(tmux_bin)
            .arg("-V")
            .output()
            .is_ok_and(|out| out.status.success())
    }
}

impl Drop for ScratchTmuxSession {
    fn drop(&mut self) {
        // Best-effort by construction: `Drop` runs during unwinding, where a
        // panic would abort the process and bury the original failure.
        let _ = Command::new(&self.tmux_bin)
            .args(["kill-session", "-t", &format!("={}", self.name)])
            .output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bare `tmux`, matching what `core::tmux::resolve_tmux_binary_or_bare`
    /// falls back to. These tests only need the `PATH` lookup a `cargo test`
    /// shell already has.
    const TMUX: &str = "tmux";

    /// Process-unique, so the two targets this file compiles into cannot
    /// collide in the machine-global tmux namespace when cargo runs them
    /// concurrently.
    ///
    /// #6116: minted through [`reserved_session_name`] — these tests spawn real
    /// sessions, so a hard kill leaks THEM too, and the leak has to land in the
    /// namespace the daemon refuses to adopt.
    fn scratch_name(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        reserved_session_name(&format!("guard-{tag}-{nanos}"))
    }

    /// The mint and the daemon's refusal read the same constant, so a fixture
    /// cannot drift out of the namespace that protects it.
    #[test]
    fn a_minted_name_is_one_the_daemon_refuses() {
        let name = reserved_session_name("mint");
        assert!(trusty_common::session_naming::is_reserved_test_session_name(&name));
        // Still managed, so the orphan-GC keeps its first gate on the pane.
        assert!(trusty_common::session_naming::is_managed_session_name(
            &name
        ));
    }

    /// A fixture that reaches the namespace through a derived name gets there
    /// too: `tm-<project>-NN` built from the slug is reserved.
    #[test]
    fn a_derived_project_slug_yields_a_reserved_name() {
        let derived = trusty_common::session_naming::build_managed_session_name(
            Some(&reserved_project_slug("deadrt1234")),
            std::path::Path::new("/tmp"),
            &[],
        )
        .expect("derive a name");
        assert!(
            trusty_common::session_naming::is_reserved_test_session_name(&derived),
            "a fixture's derived name must land in the reserved namespace, got {derived}"
        );
    }

    /// The sweep's whole risk is what it selects: only the reserved namespace,
    /// only past the age bound, and never a line it could not parse.
    #[test]
    fn stale_sweep_selects_only_old_reserved_sessions() {
        let now = 1_000_000;
        let listing = "\
900000 tm-xtest-deadrt1-01
999999 tm-xtest-running-now-01
900000 tm-trusty-tools-01
900000 work
not-an-epoch tm-xtest-garbage-01
";
        assert_eq!(
            stale_reserved_names(listing, now, STALE_SESSION_AGE_SECS),
            vec!["tm-xtest-deadrt1-01".to_string()],
            "only the aged, reserved, parseable session may be killed"
        );
    }

    /// The defect #6116 reports, inverted into an assertion: the fixture panics
    /// mid-test — exactly what `wait_for_stable_dead_runtime`'s 10s deadline
    /// does — and the session must still be gone afterwards.
    ///
    /// The panic message reaching stderr during this run is expected output,
    /// not a failure.
    #[test]
    fn drop_kills_the_session_even_when_the_body_panics() {
        if !ScratchTmuxSession::tmux_available(TMUX) {
            eprintln!("tmux not available; skipping");
            return;
        }
        let name = scratch_name("panic");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let session = ScratchTmuxSession::spawn(TMUX, &name, "sh");
            assert!(
                ScratchTmuxSession::exists(TMUX, session.name()),
                "fixture precondition: the session must be live before the panic"
            );
            panic!("simulated mid-test failure while the guard is alive");
        }));
        assert!(
            outcome.is_err(),
            "the closure must actually have panicked, or this proves nothing"
        );
        assert!(
            !ScratchTmuxSession::exists(TMUX, &name),
            "session '{name}' survived a panicking test — this is #6116, where a \
             post-hoc kill-session placed after the code under test never ran"
        );
    }

    /// The guard must kill the one name it created and nothing else. A bare
    /// `-t <name>` target does not satisfy this: with the exact name already
    /// gone, tmux prefix-matches and kills `<name>-sibling` instead.
    #[test]
    fn drop_kills_only_the_exact_name_it_created() {
        if !ScratchTmuxSession::tmux_available(TMUX) {
            eprintln!("tmux not available; skipping");
            return;
        }
        let owned_name = scratch_name("exact");
        let sibling_name = format!("{owned_name}-sibling");
        let sibling = ScratchTmuxSession::spawn(TMUX, &sibling_name, "sh");
        {
            let _owned = ScratchTmuxSession::spawn(TMUX, &owned_name, "sh");
        }
        assert!(
            !ScratchTmuxSession::exists(TMUX, &owned_name),
            "the guard must kill its own session on drop"
        );
        assert!(
            ScratchTmuxSession::exists(TMUX, &sibling_name),
            "'{sibling_name}' shares a prefix with the dropped guard's name but was created \
             by someone else; a bare `-t` target would have killed it"
        );
        drop(sibling);
    }

    /// A guard is only handed out for a session that was actually created, so
    /// a name already taken yields a panic rather than an ownership claim over
    /// someone else's session.
    #[test]
    fn spawning_a_duplicate_name_panics_instead_of_claiming_it() {
        if !ScratchTmuxSession::tmux_available(TMUX) {
            eprintln!("tmux not available; skipping");
            return;
        }
        let name = scratch_name("dup");
        let owner = ScratchTmuxSession::spawn(TMUX, &name, "sh");
        let second = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ScratchTmuxSession::spawn(TMUX, &name, "sh")
        }));
        assert!(
            second.is_err(),
            "a duplicate `new-session` must panic, not hand out a second owner"
        );
        assert!(
            ScratchTmuxSession::exists(TMUX, &name),
            "the failed spawn must leave the original owner's session untouched"
        );
        drop(owner);
        assert!(!ScratchTmuxSession::exists(TMUX, &name));
    }
}
