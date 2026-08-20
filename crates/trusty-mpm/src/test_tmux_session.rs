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
//! Test: [`tests`] below — a session that outlives a panicking closure is gone
//! afterwards, and a guard never touches a name it did not create.

use std::process::Command;

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
    fn scratch_name(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        format!("tm-scratchguard-{tag}-{}-{nanos}", std::process::id())
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
