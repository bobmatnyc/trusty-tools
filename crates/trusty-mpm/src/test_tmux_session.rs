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
///
/// What it cannot tell apart: a tmux listing carries a name and a creation
/// time, no provenance. On a machine running this suite, a session the DAEMON
/// created for a project legitimately named `xtest-…` is killed here at 30
/// minutes like any other. The daemon-side rules avoid that class of mistake by
/// also requiring an adopted provenance
/// ([`crate::session_manager::SessionRecord::is_leaked_test_adoption`]); this
/// sweep has no record to ask.
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
    ///
    /// The panic quotes tmux's own stderr, because the exit status alone does
    /// not distinguish the cases (#6523). `create window failed: fork failed:
    /// Device not configured` means the HOST is out of pseudo-terminals — on
    /// macOS the pool is capped by `kern.tty.ptmx_max` (511) and a machine with
    /// hundreds of leaked panes exhausts it — so every tmux spawn on that
    /// machine fails until the panes are reaped. That is a machine fault, not a
    /// fixture one; no test-side change creates a pty.
    pub(crate) fn spawn(tmux_bin: &str, name: &str, pane_command: &str) -> Self {
        Self::spawn_in(tmux_bin, name, None, pane_command)
    }

    /// [`ScratchTmuxSession::spawn`] with the pane's working directory pinned.
    ///
    /// Why: [`FixtureTmuxSessions`] claims a session by where its pane sits, so
    /// its own tests need to place a session inside — and outside — a fixture
    /// root on purpose (#6542). Every other caller wants the inherited cwd and
    /// keeps [`ScratchTmuxSession::spawn`].
    /// What: adds `-c <cwd>` when `cwd` is `Some`; otherwise identical.
    /// Test: [`tests::a_session_opened_under_the_root_is_killed_on_drop`].
    pub(crate) fn spawn_in(
        tmux_bin: &str,
        name: &str,
        cwd: Option<&std::path::Path>,
        pane_command: &str,
    ) -> Self {
        // #6116: reap what an earlier hard-killed run leaked, before adding to
        // the namespace. Once per process, mirroring `test_support`'s
        // `sweep_stale_test_dirs`.
        SWEEP_ONCE.call_once(|| sweep_stale_reserved_sessions(tmux_bin));
        let mut args: Vec<String> = ["new-session", "-d", "-s", name]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        if let Some(dir) = cwd {
            args.push("-c".to_string());
            args.push(dir.to_string_lossy().into_owned());
        }
        args.push(pane_command.to_string());
        // #6523: capture tmux's stderr instead of letting it through. `.status()`
        // sent the real cause to the test binary's stderr, unattached to the
        // panic, so five simultaneous failures each read only `exit status: 1`
        // and got misattributed to nested tmux.
        let out = Command::new(tmux_bin)
            .args(&args)
            .output()
            .unwrap_or_else(|e| panic!("spawn `{tmux_bin} new-session -s {name}`: {e}"));
        assert!(
            out.status.success(),
            "`{tmux_bin} new-session -d -s {name}` failed with {}; the session was NOT \
             created, so no guard may claim it. tmux said: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
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

/// Live session names, or an empty set when tmux will not answer.
fn live_session_names(tmux_bin: &str) -> std::collections::BTreeSet<String> {
    let Ok(out) = Command::new(tmux_bin)
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
    else {
        return std::collections::BTreeSet::new();
    };
    if !out.status.success() {
        return std::collections::BTreeSet::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Names of live sessions holding at least one pane whose working directory is
/// inside `root`.
///
/// What: reads `<session-name>\t<pane-path>` from `list-panes -a`. A line that
/// does not split on the tab is SKIPPED, never selected — an unparseable line
/// leaves a session alive rather than killing one this guard cannot identify.
/// `Path::starts_with` compares whole components, so `/tmp/ab` does not match a
/// pane sitting in `/tmp/abc`.
/// Test: [`tests::a_session_outside_the_root_is_left_alone`].
fn sessions_with_pane_under(
    tmux_bin: &str,
    root: &std::path::Path,
) -> std::collections::BTreeSet<String> {
    let Ok(out) = Command::new(tmux_bin)
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{pane_current_path}",
        ])
        .output()
    else {
        return std::collections::BTreeSet::new();
    };
    if !out.status.success() {
        return std::collections::BTreeSet::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.trim_end().split_once('\t'))
        .filter(|(_, pane_path)| std::path::Path::new(pane_path).starts_with(root))
        .map(|(name, _)| name.to_string())
        .collect()
}

/// Sessions the CODE UNDER TEST created inside a fixture directory, killed when
/// this value drops (#6542).
///
/// Why: [`ScratchTmuxSession`] guards a session the TEST spawns and therefore
/// names. The guided-fallback tests spawn none — they call
/// `commands::guided::fallback_protected`, which provisions a worktree and
/// launches a session whose name is derived from a session uuid the test never
/// sees. Two such sessions survived every run of `tests_behavior_b_tests`; 456
/// accumulated on the owner's machine over five days and exhausted its
/// pseudo-terminal pool (#6523).
///
/// What: snapshots the live session names at construction, and on drop kills
/// every session that is BOTH absent from that snapshot AND holding a pane
/// whose working directory is inside `root`. Both conditions are load-bearing —
/// the snapshot alone would claim a session a concurrently running suite
/// created, and the path alone would claim one this process created before the
/// guard existed. `root` is canonicalized at construction because tmux reports
/// `/private/var/…` where a macOS `TempDir` hands back `/var/…`.
///
/// What `Drop` cannot cover is the same gap [`ScratchTmuxSession`]'s module docs
/// describe: a SIGKILL or an aborted run ends the process without unwinding.
/// The sessions this guard reaps are named by the daemon's ordinary
/// `tm-<uuid>` scheme, not the reserved test namespace, so
/// [`sweep_stale_reserved_sessions`] does not reap them on the next run either.
///
/// Test: [`tests::a_session_opened_under_the_root_is_killed_on_drop`],
/// [`tests::a_session_outside_the_root_is_left_alone`],
/// [`tests::a_session_predating_the_guard_is_left_alone`].
pub(crate) struct FixtureTmuxSessions {
    tmux_bin: String,
    root: std::path::PathBuf,
    preexisting: std::collections::BTreeSet<String>,
}

impl FixtureTmuxSessions {
    /// Start owning any session that appears under `root` from now on.
    pub(crate) fn watch(tmux_bin: &str, root: &std::path::Path) -> Self {
        Self {
            tmux_bin: tmux_bin.to_string(),
            root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
            preexisting: live_session_names(tmux_bin),
        }
    }

    /// The sessions this guard currently owns — new since [`Self::watch`] and
    /// rooted under the fixture directory.
    pub(crate) fn spawned(&self) -> Vec<String> {
        sessions_with_pane_under(&self.tmux_bin, &self.root)
            .into_iter()
            .filter(|name| !self.preexisting.contains(name))
            .collect()
    }
}

impl Drop for FixtureTmuxSessions {
    fn drop(&mut self) {
        for name in self.spawned() {
            eprintln!("test-support: killing fixture tmux session '{name}' (#6542)");
            // Best-effort: `Drop` runs during unwinding, where a panic would
            // abort the process and bury the original failure.
            let _ = Command::new(&self.tmux_bin)
                .args(["kill-session", "-t", &format!("={name}")])
                .output();
        }
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
    ///
    /// #6523: the spawn and the precondition sit OUTSIDE the `catch_unwind`. In
    /// the earlier shape both were inside it, so a spawn that FAILED was caught
    /// by the same handler that the deliberate panic was meant to trip:
    /// `outcome.is_err()` held for the wrong reason, and `!exists` then held
    /// trivially because nothing had been created. The test reported `ok` on a
    /// machine that could not spawn a tmux session at all, while its two
    /// siblings failed — a false green that hid a third of the evidence.
    #[test]
    fn drop_kills_the_session_even_when_the_body_panics() {
        if !ScratchTmuxSession::tmux_available(TMUX) {
            eprintln!("tmux not available; skipping");
            return;
        }
        let name = scratch_name("panic");
        let session = ScratchTmuxSession::spawn(TMUX, &name, "sh");
        assert!(
            ScratchTmuxSession::exists(TMUX, session.name()),
            "fixture precondition: the session must be live before the panic"
        );
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            // Moved in, so the guard drops during THIS unwind — the property
            // under test.
            let _owned = session;
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

    /// The defect #6542 reports, inverted: a session the test did not name,
    /// sitting inside the fixture root, must be gone once the guard drops.
    ///
    /// The `owner` guard is a safety net, not the subject — if
    /// [`FixtureTmuxSessions`] were broken, this test must still not leak.
    #[test]
    fn a_session_opened_under_the_root_is_killed_on_drop() {
        if !ScratchTmuxSession::tmux_available(TMUX) {
            eprintln!("tmux not available; skipping");
            return;
        }
        let root = tempfile::tempdir().expect("fixture root");
        let name = scratch_name("under");
        let guard = FixtureTmuxSessions::watch(TMUX, root.path());
        let owner = ScratchTmuxSession::spawn_in(TMUX, &name, Some(root.path()), "sh");
        assert_eq!(
            guard.spawned(),
            vec![name.clone()],
            "the guard must claim the session that appeared under its root"
        );
        drop(guard);
        assert!(
            !ScratchTmuxSession::exists(TMUX, &name),
            "session '{name}' survived the guard — this is #6542, where the \
             guided-fallback tests left one tm-<uuid> session behind per run"
        );
        drop(owner);
    }

    /// The guard's claim is bounded by the fixture root: a session that a
    /// concurrent suite opens elsewhere is neither claimed nor killed.
    #[test]
    fn a_session_outside_the_root_is_left_alone() {
        if !ScratchTmuxSession::tmux_available(TMUX) {
            eprintln!("tmux not available; skipping");
            return;
        }
        let root = tempfile::tempdir().expect("fixture root");
        let elsewhere = tempfile::tempdir().expect("unrelated dir");
        let name = scratch_name("outside");
        let guard = FixtureTmuxSessions::watch(TMUX, root.path());
        let sibling = ScratchTmuxSession::spawn_in(TMUX, &name, Some(elsewhere.path()), "sh");
        assert!(
            guard.spawned().is_empty(),
            "a session outside the fixture root is not this guard's to claim"
        );
        drop(guard);
        assert!(
            ScratchTmuxSession::exists(TMUX, &name),
            "'{name}' sits outside the fixture root and belongs to someone else"
        );
        drop(sibling);
    }

    /// A session already live when the guard is created is not the guard's,
    /// even when it sits inside the root — the snapshot is what makes the
    /// ownership claim honest.
    #[test]
    fn a_session_predating_the_guard_is_left_alone() {
        if !ScratchTmuxSession::tmux_available(TMUX) {
            eprintln!("tmux not available; skipping");
            return;
        }
        let root = tempfile::tempdir().expect("fixture root");
        let name = scratch_name("predates");
        let earlier = ScratchTmuxSession::spawn_in(TMUX, &name, Some(root.path()), "sh");
        let guard = FixtureTmuxSessions::watch(TMUX, root.path());
        assert!(
            guard.spawned().is_empty(),
            "a session that predates the guard is not new, so not the guard's"
        );
        drop(guard);
        assert!(
            ScratchTmuxSession::exists(TMUX, &name),
            "'{name}' predates the guard and must survive it"
        );
        drop(earlier);
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
