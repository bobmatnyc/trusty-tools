//! Shared test-only synchronization primitives and executable-file helpers.
//!
//! Why: Multiple unit tests across different modules (`init::tests`, etc.)
//! sandbox `$HOME` with `std::env::set_var` to redirect file I/O into a
//! tempdir. `set_var` is a process-wide mutation and `cargo test` runs unit
//! tests on a multi-threaded executor by default, so two concurrent tests
//! sandboxing HOME stomp on each other and one will observe the other's
//! tempdir (or restore HOME mid-flight). The classic fix is a per-test-module
//! `static Mutex`, but that only serializes tests WITHIN one module —
//! cross-module races (e.g. `init::seed_skills_*` vs another module's
//! HOME-mutating test) still flake, and a test that skips the lock races
//! everyone. The durable fix is to inject the path instead of mutating HOME
//! (see `mistake_log`'s `record_at` seam, #2709); `HOME_LOCK` remains for
//! tests not yet migrated to injection.
//! What: Exposes a single process-wide `HOME_LOCK` that every test mutating
//! `$HOME` must hold. Also provides `write_executable_script` and
//! `spawn_script` helpers to avoid ETXTBSY races (#1528).
//! Compiled only under `#[cfg(test)]` so it costs nothing in release builds.
//! Test: Used by `init::tests::*` and other modules that still sandbox HOME.
//! Such tests were order-dependent before this lock — passing in isolation but
//! failing under `cargo test`.

#![cfg(test)]

use std::cell::Cell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Process-wide mutex serializing tests that mutate `$HOME`.
///
/// Why: `std::env::set_var` is a process-global mutation; tokio's default
/// multi-threaded test runtime causes interleaved tests to overwrite each
/// other's HOME before they restore the original value. A single static
/// Mutex shared across all modules keeps such tests sequential without
/// forcing `--test-threads=1` for the whole crate.
/// What: Use `let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());`
/// at the top of any test that calls `std::env::set_var("HOME", ...)`.
/// `unwrap_or_else(into_inner)` ensures a panic in one test doesn't poison
/// the lock for siblings. Prefer [`lock_home`] for any NEW test whose
/// production code path is guarded by [`home_lock_held_by_this_thread`]
/// (currently `listeners::store::events_dir`) — it does everything this
/// raw acquisition does, plus marks per-thread ownership that guard needs.
pub static HOME_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    /// Set for the lifetime of THIS thread's own [`HomeLockGuard`]. See
    /// [`home_lock_held_by_this_thread`]'s docs for why per-thread ownership,
    /// not global contention, is the question a recurrence guard needs
    /// answered (issue #3922 follow-up).
    static HOME_LOCK_HELD_HERE: Cell<bool> = const { Cell::new(false) };
}

/// RAII guard returned by [`lock_home`]: holds `HOME_LOCK` for its scope AND
/// marks this OS thread as the current holder, so
/// [`home_lock_held_by_this_thread`] can answer "do *I* hold it?" instead of
/// "is it contended by ANYONE?".
///
/// Deliberately `!Send` (it owns a `std::sync::MutexGuard`, which is `!Send`)
/// — see [`assert_home_lock_guard_is_not_send`] for why that negative bound
/// is load-bearing and compile-time asserted rather than merely inherited.
pub struct HomeLockGuard {
    _inner: MutexGuard<'static, ()>,
}

impl Drop for HomeLockGuard {
    fn drop(&mut self) {
        HOME_LOCK_HELD_HERE.with(|c| c.set(false));
    }
}

// #3957: layer 1 of 2 — COMPILE-TIME. `HomeLockGuard`'s thread-local
// ownership flag is only meaningful while the guard cannot leave the thread
// that created it: if the guard were moved to another thread, `Drop` would
// clear the flag over THERE while the creating thread's flag stayed
// permanently `true`, and any later, genuinely unguarded caller that happened
// to run on that thread would read the stale `true` and sail past
// `listeners::store::events_dir`'s recurrence guard — silently reintroducing
// exactly the masking bug #3922/#3943 exist to close. Today the property is
// inherited for free from `MutexGuard: !Send`, and that inheritance is
// precisely what makes `tokio::spawn(async { let _g = lock_home(); .await })`
// a COMPILE error (a spawned future must be `Send`). This assertion pins the
// property so a future refactor of the guard's innards (e.g. swapping in a
// `parking_lot` guard, or boxing state behind an `Arc`) cannot quietly make
// it `Send` and reopen the hole; it fails at `cargo test` compile time, not
// at review time.
//
// The mechanism is `static_assertions::assert_not_impl_any!`'s trick,
// inlined because the workspace does not depend on that crate: two blanket
// impls of a private helper trait — one for all types, one for `Send` types —
// leave the `AmbiguousIfSend<_>` inference variable UNRESOLVABLE when (and
// only when) the subject type is `Send`, so a `Send` guard fails to compile
// with E0282/E0283.
#[expect(
    dead_code,
    reason = "#3957: compile-time-only assertion; never executed"
)]
fn assert_home_lock_guard_is_not_send() {
    trait AmbiguousIfSend<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
    let _ = <HomeLockGuard as AmbiguousIfSend<_>>::some_item;
}

/// Panic if the calling thread is running inside a MULTI-THREADED tokio
/// runtime, where [`HOME_LOCK_HELD_HERE`]'s per-thread bookkeeping is not
/// sound (#3957).
///
/// Why: the thread-local ownership model is correct only because a
/// current-thread runtime pins a test's ENTIRE execution — every `.await`
/// point included — to the one OS thread that `block_on`s it. On a
/// multi-threaded runtime that pinning disappears: a task can resume on a
/// different pool worker, the creating thread's `Cell` can stay `true`
/// after the logical owner has moved on, and a later, genuinely unguarded
/// task scheduled onto that reused thread reads the stale `true` and
/// silently defeats `listeners::store::events_dir`'s guard. `tokio`'s
/// `features = ["full"]` makes `rt-multi-thread` available to any future
/// test in this crate, and prose in a doc comment does not stop anyone —
/// so this refuses at the exact moment of danger with a message naming the
/// alternatives. `Handle::try_current()` returning `Err` (no ambient
/// runtime — a plain `#[test]`, or a `std::thread`) is fine: there is no
/// scheduler that could migrate anything, so the thread is stable.
/// What: reads `Handle::try_current()`'s `runtime_flavor()` and panics on
/// `RuntimeFlavor::MultiThread`. Deliberately checked BEFORE `HOME_LOCK` is
/// acquired, so a rejected caller never holds (nor poisons) the mutex.
/// Note this also (intentionally) rejects `spawn_blocking` bodies on a
/// multi-threaded runtime: those run on a reused pool thread, which is the
/// same hazard.
/// Test: `test_env::tests::lock_home_rejects_a_multi_thread_runtime`.
fn assert_single_threaded_runtime_context() {
    if let Ok(handle) = tokio::runtime::Handle::try_current()
        && handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread
    {
        panic!(
            "crate::test_env::lock_home(): called from a MULTI-THREADED tokio runtime \
             (issue #3957). This guard tracks lock ownership in a thread-local, which is \
             only sound while a task cannot migrate between OS threads — a multi-threaded \
             runtime can leave the flag permanently set on a worker thread and let a later, \
             genuinely unguarded caller read it as `true`, defeating \
             `listeners::store::events_dir`'s recurrence guard (issue #3922). Use the \
             default current-thread `#[tokio::test]` (no `flavor = \"multi_thread\"`), or \
             — preferably — stop sandboxing $HOME entirely and inject a tempdir via an \
             `*_at(dir, ..)` seam (see `listeners::store::EventStore`)."
        );
    }
}

/// Acquire [`HOME_LOCK`] AND mark this thread as the holder for
/// [`home_lock_held_by_this_thread`].
///
/// Why (issue #3922 follow-up, code-critic HIGH on PR #3943): a
/// `#[cfg(test)]` recurrence guard that wants to assert "the CALLING test
/// currently honours the crate's `$HOME`-sandboxing convention" cannot
/// answer that from `HOME_LOCK.try_lock()` alone — that only reports
/// whether the mutex is CONTENDED, i.e. whether *some* thread holds it,
/// which says nothing about whether the thread asking the question is that
/// holder. The critic proved this concretely: a background thread takes
/// `HOME_LOCK` for an unrelated reason and holds it; a second, completely
/// lock-less thread then reaches the guarded code path, and
/// `try_lock().is_ok()` reports `false` (busy) — read by the old guard as
/// "the convention is being honoured" — even though the ACTUAL caller
/// plainly is not participating at all. This function fixes that by
/// marking per-thread ownership instead of relying on global contention.
/// What: identical acquisition to the raw `HOME_LOCK.lock()` idiom used
/// throughout this crate (`unwrap_or_else(into_inner)` so one test's panic
/// never poisons the lock for siblings), wrapped in a guard that also flips
/// a thread-local flag on acquire and clears it on drop.
/// `cargo test`'s default current-thread `#[tokio::test]` runtime (used by
/// every test in this crate) pins an async test's ENTIRE execution,
/// including every `.await` point, to the one OS thread that `block_on`s
/// it, so a thread-local set for this guard's lifetime correctly tracks
/// "this test currently holds the lock" without needing a heavier
/// `tokio::task_local!` (which would additionally force every call site to
/// restructure its body into a `.scope(...)` future — a much larger,
/// crate-wide change this fix does not need).
///
/// #3957: that pinning precondition is no longer a grep-verified assumption
/// that a future test could silently break. It is now enforced twice — at
/// compile time by [`assert_home_lock_guard_is_not_send`] (the guard cannot
/// be moved to, or `tokio::spawn`ed onto, another thread) and at run time by
/// [`assert_single_threaded_runtime_context`] (acquiring inside a
/// multi-threaded runtime panics on the spot). See both for the full
/// argument.
///
/// Test: `listeners::store::tests::events_dir_panics_when_a_different_thread_holds_home_lock`
/// (the exact masking scenario this seam exists to close) plus every
/// migrated caller in `listeners::poll`, `api::server::tests::listener_events`,
/// and `slack::tests::eventstream_tests`; the #3957 guards themselves in
/// `test_env::tests`.
pub fn lock_home() -> HomeLockGuard {
    // #3957: reject the unsound context BEFORE taking the mutex.
    assert_single_threaded_runtime_context();
    let guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    HOME_LOCK_HELD_HERE.with(|c| c.set(true));
    HomeLockGuard { _inner: guard }
}

/// Whether THIS thread currently holds `HOME_LOCK` via [`lock_home`].
///
/// Why: see [`lock_home`]'s docs for the full story — this is the narrower,
/// per-caller question a recurrence guard actually needs answered, in place
/// of `HOME_LOCK.try_lock().is_ok()`'s global-contention signal.
/// What: `true` only while the CURRENT thread's own `lock_home()` guard is
/// still alive; `false` in every other case, INCLUDING "some other thread
/// holds `HOME_LOCK`" — that distinction is the entire point.
///
/// #3957: this reader needs no runtime-flavor check of its own. A stale
/// `true` is the only unsafe answer it could give, and the flag can only be
/// set by [`lock_home`], which refuses to run in the one context that could
/// strand it. A `false` read from an unexpected thread is always safe: it
/// makes `listeners::store::events_dir` panic loudly rather than pass
/// silently.
pub fn home_lock_held_by_this_thread() -> bool {
    HOME_LOCK_HELD_HERE.with(|c| c.get())
}

/// Process-wide mutex serializing tests that mutate LLM credential env vars
/// (#250). Same rationale as `HOME_LOCK` — `std::env::set_var` is global so
/// concurrent credential-routing tests would race.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Force `trusty_common::credentials::load_env_local_once` to
/// have already run in this process, before any test-local `remove_var`.
///
/// Why (#3464): that loader is a process-global `OnceLock` — it fires at
/// most once per test binary, on whichever call (from ANY test, in ANY
/// module) happens to be first, and folds `.env.local` content into the
/// process environment (searched upward from cwd — which, for this repo's
/// own linked-worktree layout, resolves all the way up to the shared main
/// checkout root — AND from `$HOME`). A credential test that clears a few
/// env vars and then calls production code that transitively calls
/// `resolve_key` implicitly assumes that call is a no-op; if it is instead
/// the FIRST call in the whole process, the loader can silently re-populate
/// a var the test just removed, in the middle of the very function under
/// test, purely because a `.env.local` happens to exist on that machine (a
/// real, gitignored dev file — confirmed live as the cause of #3464's
/// `other_configured_providers_empty_when_nothing_else_configured` flake).
/// Calling this FIRST, before any `remove_var`, guarantees the one-time load
/// has already happened (whether it was this call or an earlier test's), so
/// every subsequent `remove_var` in the same locked critical section is the
/// last word — no future call in this process can ever repopulate it.
/// What: thin wrapper over `load_env_local_once()` (itself idempotent).
/// Callers should hold `ENV_LOCK` (and `HOME_LOCK` if they also sandbox
/// `$HOME`) for their entire body, same convention as every other
/// env-mutating test; async tests that cannot hold a `std::sync::Mutex`
/// guard across `.await` may call this un-guarded and rely on `#[serial]`
/// instead, matching the existing convention for those tests.
/// Test: exercised indirectly via every test that now calls this before
/// clearing credential env vars (`llm::credentials::tests::clear_all`,
/// `llm::helpers::tests::create_client_*`, `llm::adapter::tests::*_when_env_absent`,
/// `llm::http::tests::send_raw_completion_*`, `ctrl::ctrl_turn::dispatch::tests::clear_creds_env`,
/// `runtime::cli_def::tests::credential_banner::clear_all`) — not
/// independently unit tested since it depends on real process/filesystem
/// state, exactly like `load_env_local_once` itself.
pub fn force_env_local_loaded() {
    trusty_common::credentials::load_env_local_once();
}

/// Clear the process env var for every provider the shared inference
/// registry maps to a credential env var, plus `CLAUDE_CODE_OAUTH_TOKEN`
/// (ctrl/PM-only, not in the registry).
///
/// Why (#3464): `llm::credentials::other_configured_providers()` checks
/// EVERY registry provider (`fireworks`, `atlascloud`, `openai`, `together`,
/// ...), not just the three `openrouter`/`anthropic`/`claude-code` names
/// `pick_credentials` cares about. A test asserting "nothing else is
/// configured" must clear all of them, not a hand-picked subset — otherwise
/// it silently depends on the local machine (or CI runner) never having any
/// of those provider credentials configured anywhere resolvable (env,
/// `.env.local`, or the secure store). Must be called AFTER
/// [`force_env_local_loaded`], never before — see that function's docs.
/// What: iterates `trusty_common::inference::registry::all()`, removing
/// each provider's `env_var_for` mapping when one exists, then removes
/// `CLAUDE_CODE_OAUTH_TOKEN` explicitly (it has no registry entry — it's
/// ctrl/PM's own OAuth routing credential, not an inference provider).
/// Test: `llm::credentials::tests::other_configured_providers_empty_when_nothing_else_configured`.
pub fn clear_all_credential_env_vars() {
    for caps in trusty_common::inference::registry::all() {
        if let Some(var) = trusty_common::credentials::env_var_for(caps.id.as_str()) {
            // SAFETY: caller holds `ENV_LOCK` for the whole body (documented
            // requirement, same convention as every other env mutation in
            // this module).
            unsafe {
                std::env::remove_var(var);
            }
        }
    }
    // SAFETY: see above.
    unsafe {
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
    }
}

/// Write a shell script to `dir/name`, flush and close the file handle, THEN
/// set the execute bit — eliminating the ETXTBSY race (#1528).
///
/// Why: The Linux kernel rejects `execve` with ETXTBSY (os error 26) when
/// ANY process holds an open writable fd to the target inode — including the
/// writing process itself. Closing/dropping the `File` before `set_permissions`
/// and before `spawn` removes that fd, satisfying the kernel's constraint.
/// The safe pattern is: open → write → flush → **drop the `File`** → chmod
/// → spawn. Without the explicit drop, the writable fd can still be open when
/// `execve` is called, triggering ETXTBSY even under light load.
/// What: Creates the file at `dir.join(name)`, writes `contents`, syncs,
/// drops the file handle, sets mode 0o755, and returns the absolute path.
/// Test: All tests in `subprocess::tests` and `agents::claude_code_runner::tests`
/// that spawn a mock shell script call this helper; ETXTBSY flakiness should
/// not recur after the refactor.
#[cfg(unix)]
pub fn write_executable_script(dir: &Path, name: &str, contents: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    {
        // Scope the File so it is closed before set_permissions.
        let mut f = std::fs::File::create(&path)
            .unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
        f.write_all(contents.as_bytes())
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        f.flush()
            .unwrap_or_else(|e| panic!("flush {}: {e}", path.display()));
        // `f` drops here — fd is closed before chmod.
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
    path
}

/// Spawn a shell script at `path` with stdin=null, stdout=piped, stderr=inherit,
/// retrying on ETXTBSY (os error 26) up to 3 times with exponential back-off.
///
/// Why: Even after closing the write handle before chmod, kernel page-cache
/// flush timing under heavy CI load can delay the execute permission becoming
/// visible to `execve`. A small bounded retry (≤3 attempts, 5 ms back-off)
/// acts as belt-and-suspenders without hiding real failures.
/// What: Constructs a `tokio::process::Command` fresh on each attempt
/// (required because `Command` is not `Clone`) and calls `.spawn()`.
/// On ETXTBSY it backs off and retries; on any other error or after
/// `MAX_ATTEMPTS` ETXTBSY hits it returns the error immediately.
/// The stdio setup (stdin null / stdout piped / stderr inherited) matches
/// the pattern used by all subprocess and claude-code-runner tests.
/// Test: Covered indirectly by every async test that calls `spawn_script`;
/// a direct unit test would require artificially injecting ETXTBSY which is
/// impractical in pure Rust tests. The helper's correctness is validated by
/// the absence of CI failures after the refactor.
#[cfg(unix)]
pub async fn spawn_script(path: &std::path::Path) -> std::io::Result<tokio::process::Child> {
    use std::process::Stdio;

    const MAX_ATTEMPTS: u32 = 3;
    const BACKOFF_MS: u64 = 5;

    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        let mut cmd = tokio::process::Command::new(path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(
                    BACKOFF_MS * (1 << attempt),
                ))
                .await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap())
}

/// Tests for this module's own #3957 durability guards.
///
/// Why: `lock_home`'s thread-local ownership model is only sound under a
/// single-threaded execution context, and #3957 asked for that precondition
/// to be enforced rather than merely documented. An enforcement mechanism
/// nobody exercises rots exactly like the prose it replaced, so both the
/// rejection path and the accepted paths are pinned here.
/// What: the multi-threaded-runtime rejection, plus the two contexts that
/// must keep working (no ambient runtime, and the default current-thread
/// `#[tokio::test]` runtime every other test in this crate uses).
/// Test: this IS the test module.
#[cfg(test)]
mod tests {
    use super::*;

    /// #3957: acquiring `lock_home()` inside a multi-threaded tokio runtime
    /// must panic, not quietly hand back a guard whose thread-local flag
    /// can be stranded on a pool worker. Also asserts the rejection is
    /// clean — the flag is untouched and (because the check runs before the
    /// acquisition) no `HOME_LOCK` guard was ever created.
    #[test]
    fn lock_home_rejects_a_multi_thread_runtime() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("multi-thread runtime");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rt.block_on(async {
                let _guard = lock_home();
            });
        }));

        assert!(
            result.is_err(),
            "lock_home() must refuse a multi-threaded runtime — its thread-local \
             ownership flag cannot survive task migration (#3957)"
        );
        let msg = result
            .err()
            .and_then(|e| {
                e.downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
            })
            .unwrap_or_default();
        assert!(
            msg.contains("#3957"),
            "the panic must name the issue and the remedy, got: {msg}"
        );
        assert!(
            !home_lock_held_by_this_thread(),
            "a rejected acquisition must leave no ownership flag behind"
        );
    }

    /// #3957: the rejection must not catch the contexts every real caller
    /// uses — a plain `#[test]` with no ambient runtime at all, where no
    /// scheduler exists to migrate anything.
    #[test]
    fn lock_home_accepts_a_thread_with_no_ambient_runtime() {
        assert!(!home_lock_held_by_this_thread());
        {
            let _guard = lock_home();
            assert!(
                home_lock_held_by_this_thread(),
                "the guard must mark this thread as the holder"
            );
        }
        assert!(
            !home_lock_held_by_this_thread(),
            "dropping the guard must clear this thread's ownership flag"
        );
    }

    /// #3957: likewise for the default current-thread `#[tokio::test]`
    /// runtime — the flavor every `$HOME`-sandboxing test in this crate
    /// runs on, which pins the whole test (every `.await` included) to one
    /// OS thread and so keeps the thread-local honest.
    // `await_holding_lock` is the whole point here: holding a sync guard
    // across `.await` is exactly what this crate's HOME-sandboxing tests do,
    // and this test asserts that doing so stays sound on this flavor.
    #[allow(
        clippy::await_holding_lock,
        reason = "#3957: the held-across-await case IS the property under test"
    )]
    #[tokio::test]
    async fn lock_home_accepts_the_default_current_thread_runtime() {
        let _guard = lock_home();
        assert!(home_lock_held_by_this_thread());
        tokio::task::yield_now().await;
        assert!(
            home_lock_held_by_this_thread(),
            "ownership must survive an .await on a current-thread runtime"
        );
    }
}
