//! Cargo ownership guard: move downloader-placed binaries aside so
//! `cargo install` never exits 101 on a file it does not own (#5777).
//!
//! Why: `cargo install` refuses to overwrite a binary in `$CARGO_HOME/bin`
//! that Cargo's own `.crates2.json` does not record — and after the #4964
//! Phase 3 destination flip, every prebuilt-download install places exactly
//! such files there. Without this guard the first `cargo install` after a
//! prebuilt install fails with `binary <name> already exists in destination`
//! (exit 101) on all nine cargo reach points, including the in-daemon
//! `upgrade` commands and MCP tools that never consult the downloader.
//!
//! What: [`OwnershipGuard::move_aside`] renames each existing destination
//! binary to a hidden, uniquely-suffixed aside name in the SAME directory
//! (same-filesystem rename — atomic, and never a `cp` over an on-PATH binary,
//! per the macOS cdhash rule). After the cargo run the caller settles the
//! guard exactly once:
//! - [`OwnershipGuard::commit`] (cargo exited 0) — removes each aside whose
//!   final path was re-written by cargo, and RESTORES any aside whose final
//!   path is still missing (cargo's "already installed, ignoring" skip writes
//!   nothing; deleting the aside there would delete the only copy).
//! - [`OwnershipGuard::restore`] (cargo failed) — renames every aside back so
//!   the pre-existing binary keeps working.
//!
//! Errors: any settle step that cannot complete returns `Err` naming every
//! aside file left behind, so a binary is never silently lost — the aside
//! copy still exists on disk under its hidden name.
//!
//! Safety net: a guard that is never settled — the `cargo install` future is
//! dropped (daemon shutdown kills the detached `tokio::spawn` upgrade tasks),
//! the process unwinds, or Ctrl-C aborts a `tctl` cargo fallback — restores
//! its asides from [`Drop`], and [`OwnershipGuard::move_aside`] sweeps stale
//! asides left by a SIGKILL'd process (dead pid) on the next run.
//!
//! Test: `tests` below — happy path, restore-on-failure, the
//! skip-restores-rather-than-deletes case, drop-without-settle, the
//! stale-aside sweep, and concurrent guards on the same binary set.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic per-process counter so two concurrent guards in one process can
/// never collide on an aside name even for the same binary.
static ASIDE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Binaries moved aside ahead of a `cargo install`, awaiting settle.
///
/// Why/What/Test: see the module doc — this is the module's only type.
#[derive(Debug)]
pub(crate) struct OwnershipGuard {
    /// `(final path, aside path)` pairs, in move order.
    moved: Vec<(PathBuf, PathBuf)>,
}

impl OwnershipGuard {
    /// A guard that moved nothing (no bin dir resolvable, or nothing existed).
    pub(crate) fn inert() -> Self {
        Self { moved: Vec::new() }
    }

    /// Move every existing `bin_dir/<name>` aside, atomically per file.
    ///
    /// Why: clearing the destination BEFORE the cargo spawn is what turns the
    /// exit-101 refusal into a clean install; renaming (not deleting) is what
    /// makes failure recoverable.
    /// What: for each name, when `bin_dir/<name>` exists it is renamed to
    /// `bin_dir/.<name>.pre-cargo.<pid>.<seq>`. On any rename failure the
    /// already-moved files are restored and the error is returned — the guard
    /// never leaves a half-cleared destination behind.
    /// Also sweeps stale `.{name}.pre-cargo.*` asides whose owning pid is
    /// dead (see [`sweep_stale_asides`]) so a SIGKILL — where [`Drop`] never
    /// runs — is recovered on the NEXT upgrade instead of never.
    /// Test: `move_aside_then_commit_removes_asides`,
    /// `sweep_restores_stale_aside_when_destination_missing`,
    /// `sweep_deletes_stale_aside_when_destination_exists`,
    /// `sweep_leaves_live_process_asides_alone`;
    /// `move_aside_failure_restores_already_moved` is covered indirectly by
    /// `restore_puts_every_binary_back` (same restore path).
    pub(crate) fn move_aside(bin_dir: &Path, binaries: &[String]) -> anyhow::Result<Self> {
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        for name in binaries {
            let dest = bin_dir.join(name);
            // #5777: recover asides orphaned by a SIGKILL'd process BEFORE
            // the existence check — a stale aside with the destination
            // missing is exactly the state where the only copy of the
            // binary is hiding under the aside name.
            sweep_stale_asides(bin_dir, name, &dest);
            if !dest.exists() {
                continue;
            }
            let seq = ASIDE_SEQ.fetch_add(1, Ordering::Relaxed);
            let aside = bin_dir.join(format!(".{name}.pre-cargo.{}.{seq}", std::process::id()));
            if let Err(e) = std::fs::rename(&dest, &aside) {
                // TOCTOU with a concurrent guard: the file existed a moment
                // ago but another upgrade already moved it aside. That guard
                // owns it now; nothing here to protect.
                if e.kind() == std::io::ErrorKind::NotFound {
                    continue;
                }
                // Roll back what was already moved before propagating.
                let rollback = restore_pairs(&moved);
                let mut err = anyhow::anyhow!(
                    "cargo ownership guard could not move {} aside: {e}",
                    dest.display()
                );
                if let Err(r) = rollback {
                    err = err.context(format!("and rollback was incomplete: {r}"));
                }
                return Err(err);
            }
            tracing::debug!(
                dest = %dest.display(),
                aside = %aside.display(),
                "moved untracked binary aside before cargo install (#5777)"
            );
            moved.push((dest, aside));
        }
        Ok(Self { moved })
    }

    /// Settle after a SUCCESSFUL cargo run.
    ///
    /// What: for each moved pair — if cargo re-wrote the final path, the aside
    /// is deleted; if the final path is still missing (cargo's
    /// already-installed skip), the aside is renamed back so the binary is not
    /// lost. Collects failures rather than stopping at the first.
    /// Test: `move_aside_then_commit_removes_asides`,
    /// `commit_restores_when_cargo_skipped_writing`,
    /// `drop_after_commit_never_restores_over_cargo_output`.
    pub(crate) fn commit(mut self) -> anyhow::Result<()> {
        // #5777: take the pairs so the Drop safety net sees an empty guard
        // and cannot double-settle after this normal settle.
        let moved = std::mem::take(&mut self.moved);
        let mut failures: Vec<String> = Vec::new();
        for (dest, aside) in &moved {
            if dest.exists() {
                if let Err(e) = std::fs::remove_file(aside) {
                    failures.push(format!("could not remove aside {}: {e}", aside.display()));
                }
            } else if let Err(e) = std::fs::rename(aside, dest) {
                failures.push(format!(
                    "cargo wrote nothing at {} and the aside copy {} could not be \
                     restored: {e}",
                    dest.display(),
                    aside.display()
                ));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "cargo ownership guard settle incomplete: {}",
                failures.join("; ")
            ))
        }
    }

    /// Settle after a FAILED cargo run: rename every aside back.
    ///
    /// Test: `restore_puts_every_binary_back`.
    pub(crate) fn restore(mut self) -> anyhow::Result<()> {
        // #5777: take the pairs so Drop no-ops after this normal settle.
        let moved = std::mem::take(&mut self.moved);
        restore_pairs(&moved)
    }
}

/// Last-resort settle: restore the asides of a guard that was never settled.
///
/// Why (#5777, code-critic round on PR #5778): between `move_aside` and the
/// explicit settle sits the entire multi-minute `cargo install` `.await`.
/// If that future is dropped — both daemon MCP upgrade paths run in detached
/// `tokio::spawn` tasks that die with the runtime on daemon shutdown — or the
/// process unwinds on panic or a Ctrl-C-triggered abort during a `tctl`
/// cargo-fallback build, no settle ever runs. Without this impl the
/// destination is left EMPTY and the only copy of the binary sits under a
/// hidden aside name nothing restores: an interruption that pre-guard left
/// the old binary intact would post-guard remove it from `PATH` and break the
/// next launchd respawn. Restoring in `Drop` makes the guard
/// infallible-by-construction for every in-process exit path; the dead-pid
/// sweep in [`OwnershipGuard::move_aside`] covers SIGKILL, where `Drop`
/// cannot run.
///
/// What: best-effort [`restore_pairs`] over whatever pairs are still held.
/// [`OwnershipGuard::commit`] and [`OwnershipGuard::restore`] `mem::take` the
/// pairs first, so after a normal settle this is a no-op. `Drop` cannot
/// return errors and must never panic, so failures are logged via `tracing`
/// — the aside copies named in the log still exist on disk.
///
/// Test: `dropped_guard_restores_its_asides`,
/// `drop_after_commit_never_restores_over_cargo_output`.
impl Drop for OwnershipGuard {
    fn drop(&mut self) {
        if self.moved.is_empty() {
            return;
        }
        let moved = std::mem::take(&mut self.moved);
        match restore_pairs(&moved) {
            Ok(()) => tracing::warn!(
                restored = moved.len(),
                "cargo ownership guard dropped without settling; asides \
                 restored from Drop (#5777)"
            ),
            Err(e) => tracing::error!(
                error = %e,
                "cargo ownership guard dropped without settling and the \
                 Drop restore was incomplete — aside copies remain on disk \
                 (#5777)"
            ),
        }
    }
}

/// Rename each `(dest, aside)` pair back to `dest`, collecting failures.
fn restore_pairs(moved: &[(PathBuf, PathBuf)]) -> anyhow::Result<()> {
    let mut failures: Vec<String> = Vec::new();
    for (dest, aside) in moved {
        if let Err(e) = std::fs::rename(aside, dest) {
            failures.push(format!(
                "could not restore {} from {}: {e}",
                dest.display(),
                aside.display()
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "cargo ownership guard restore incomplete (aside copies left on \
             disk): {}",
            failures.join("; ")
        ))
    }
}

/// Recover `.{name}.pre-cargo.<pid>.<seq>` asides orphaned by a dead process.
///
/// Why (#5777): `Drop` covers every in-process exit path, but a SIGKILL (or
/// power loss) kills the process before `Drop` can run, stranding the moved
/// binary under its hidden aside name forever — and, when cargo never wrote
/// the destination, leaving the bin dir with NO visible copy at all. Sweeping
/// at the start of the next `move_aside` bounds the damage to one interrupted
/// run and stops aside litter accumulating in `~/.cargo/bin`.
///
/// What: scans `bin_dir` for entries named `.{name}.pre-cargo.<pid>.<seq>`
/// (sorted for determinism). Entries whose pid is this process or still
/// alive belong to a concurrent guard and are left alone. For a dead pid:
/// when `dest` is missing the aside is renamed back (it is the only copy);
/// otherwise the aside is deleted. Entirely best-effort — every failure is
/// logged via `tracing` and never propagated, because an unswept aside must
/// not block the upgrade that is about to run.
///
/// Test: `sweep_restores_stale_aside_when_destination_missing`,
/// `sweep_deletes_stale_aside_when_destination_exists`,
/// `sweep_leaves_live_process_asides_alone`.
fn sweep_stale_asides(bin_dir: &Path, name: &str, dest: &Path) {
    let prefix = format!(".{name}.pre-cargo.");
    let entries = match std::fs::read_dir(bin_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(
                bin_dir = %bin_dir.display(),
                "stale-aside sweep skipped, could not read dir: {e}"
            );
            return;
        }
    };
    let mut stale: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let rest = file_name.strip_prefix(&prefix)?;
            let pid: u32 = rest.split('.').next()?.parse().ok()?;
            (pid != std::process::id() && !pid_is_alive(pid)).then(|| entry.path())
        })
        .collect();
    stale.sort();
    for aside in stale {
        let result = if dest.exists() {
            std::fs::remove_file(&aside)
        } else {
            std::fs::rename(&aside, dest)
        };
        match result {
            Ok(()) => tracing::warn!(
                aside = %aside.display(),
                dest = %dest.display(),
                "recovered stale pre-cargo aside from a dead process (#5777)"
            ),
            Err(e) => tracing::warn!(
                aside = %aside.display(),
                "could not recover stale pre-cargo aside: {e}"
            ),
        }
    }
}

/// Whether a process with `pid` is currently running.
///
/// What: `kill(pid, 0)` — signal 0 performs the permission/existence check
/// without delivering anything. `0` means alive; `EPERM` means alive but not
/// ours. Only `ESRCH` counts as dead — every other error is treated as alive
/// so the sweep fails CLOSED (never reclaims an aside a live guard owns).
/// Test: exercised through the `sweep_*` tests (dead pid from a reaped
/// child, live pid from the test process itself).
#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // Safety: kill(2) with signal 0 only probes; it cannot affect the target.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Non-unix fallback: report every pid alive, so the sweep never runs.
///
/// Why: no portable cheap liveness probe; failing CLOSED (treat as alive)
/// merely leaves litter, while a wrong "dead" answer could steal a live
/// guard's aside.
#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::write(path, content).expect("write fixture");
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Why: the happy path — cargo replaced the binaries, so the asides must
    /// be gone and the fresh files untouched.
    /// What: moves two binaries aside, simulates cargo writing new ones,
    /// commits, and asserts new content survives with no hidden litter.
    #[test]
    fn move_aside_then_commit_removes_asides() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp.path().join("tm"), "old-tm");
        write(&tmp.path().join("trusty-mpm"), "old-mpm");

        let guard =
            OwnershipGuard::move_aside(tmp.path(), &names(&["tm", "trusty-mpm"])).expect("move");
        assert!(!tmp.path().join("tm").exists(), "destination must be clear");

        // Simulate `cargo install` writing fresh binaries.
        write(&tmp.path().join("tm"), "new-tm");
        write(&tmp.path().join("trusty-mpm"), "new-mpm");

        guard.commit().expect("commit");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("tm")).expect("read"),
            "new-tm"
        );
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no aside litter after commit: {leftovers:?}"
        );
    }

    /// Why (#5777 failure path): a cargo failure must leave the machine
    /// exactly as it was — the downloader-placed binary keeps working.
    /// What: moves aside, does NOT write replacements (cargo "failed"),
    /// restores, and asserts the original bytes are back at the final path.
    #[test]
    fn restore_puts_every_binary_back() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp.path().join("tctl"), "old-tctl");
        write(&tmp.path().join("trusty-installer"), "old-ti");

        let guard = OwnershipGuard::move_aside(tmp.path(), &names(&["tctl", "trusty-installer"]))
            .expect("move");
        assert!(!tmp.path().join("tctl").exists());

        guard.restore().expect("restore");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("tctl")).expect("read"),
            "old-tctl"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("trusty-installer")).expect("read"),
            "old-ti"
        );
    }

    /// Why: `cargo install --locked` exits 0 WITHOUT writing when
    /// `.crates2.json` already records the exact version ("already installed,
    /// ignoring"). Deleting the aside in that case would delete the only copy
    /// of the binary — the one data-loss shape a naive delete-aside has.
    /// What: commits with the final path still missing and asserts the aside
    /// was renamed back rather than removed.
    #[test]
    fn commit_restores_when_cargo_skipped_writing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp.path().join("tagent"), "only-copy");

        let guard = OwnershipGuard::move_aside(tmp.path(), &names(&["tagent"])).expect("move");
        // Simulated cargo run: exit 0, nothing written (skip case).
        guard.commit().expect("commit");

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("tagent")).expect("read"),
            "only-copy",
            "the skip case must restore the aside, never delete the only copy"
        );
    }

    /// Why: a missing destination is the common case for a crate never
    /// installed before; the guard must be a no-op, not an error.
    #[test]
    fn move_aside_skips_missing_binaries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let guard = OwnershipGuard::move_aside(tmp.path(), &names(&["not-there"])).expect("move");
        guard.commit().expect("commit is a no-op");
    }

    /// Why (#5777 concurrency): two upgrades racing over the same bin dir
    /// (e.g. a daemon MCP `upgrade` tool and a manual CLI upgrade) must never
    /// lose the binary or collide on aside names. Unique per-process sequence
    /// suffixes plus rename semantics mean exactly one thread owns the file
    /// at any time; whichever guard holds it restores it.
    /// What: N threads each move-aside + restore the same binary name in the
    /// same dir; afterwards the file must exist with its original content and
    /// no aside litter may remain.
    #[test]
    fn concurrent_guards_never_lose_the_binary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin = tmp.path().join("trusty-search");
        write(&bin, "v1");

        std::thread::scope(|s| {
            for _ in 0..8 {
                let dir = tmp.path().to_path_buf();
                s.spawn(move || {
                    for _ in 0..25 {
                        let guard = OwnershipGuard::move_aside(&dir, &names(&["trusty-search"]))
                            .expect("move_aside must not error");
                        guard.restore().expect("restore must not error");
                    }
                });
            }
        });

        assert_eq!(
            std::fs::read_to_string(&bin).expect("binary must survive the race"),
            "v1"
        );
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no aside litter after the race: {leftovers:?}"
        );
    }

    /// Collect the hidden (dot-prefixed) entries left in `dir`.
    fn hidden_entries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect()
    }

    /// Spawn and reap a short-lived child, returning its (now dead) pid.
    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");
        let pid = child.id();
        child.wait().expect("wait for `true`");
        pid
    }

    /// Why (#5778 code-critic HIGH): an unsettled guard — dropped future,
    /// panic unwind, Ctrl-C abort — must restore its asides, or the
    /// destination is left empty and a launchd respawn breaks.
    /// What: moves a binary aside, drops the guard WITHOUT settling, and
    /// asserts the destination is back with the original contents and no
    /// aside litter remains.
    #[test]
    fn dropped_guard_restores_its_asides() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp.path().join("tm"), "only-copy");

        let guard = OwnershipGuard::move_aside(tmp.path(), &names(&["tm"])).expect("move");
        assert!(!tmp.path().join("tm").exists(), "destination cleared");
        drop(guard);

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("tm")).expect("read"),
            "only-copy",
            "Drop must restore an unsettled guard's asides"
        );
        assert!(
            hidden_entries(tmp.path()).is_empty(),
            "no aside litter after Drop"
        );
    }

    /// Why: `commit`/`restore` `mem::take` the pairs, so Drop must be a
    /// no-op after a normal settle — it must never rename the (deleted)
    /// aside back over cargo's freshly-written binary.
    /// What: settles via `commit` with cargo output in place, and asserts
    /// the NEW content survives the guard's drop.
    #[test]
    fn drop_after_commit_never_restores_over_cargo_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp.path().join("tga"), "old");

        let guard = OwnershipGuard::move_aside(tmp.path(), &names(&["tga"])).expect("move");
        write(&tmp.path().join("tga"), "new"); // simulated cargo output
        guard.commit().expect("commit"); // guard is dropped inside commit

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("tga")).expect("read"),
            "new",
            "Drop after commit must not restore the old binary"
        );
        assert!(hidden_entries(tmp.path()).is_empty());
    }

    /// Why (#5778 code-critic HIGH, SIGKILL shape): Drop never runs under
    /// SIGKILL; the next `move_aside` must restore an orphaned aside whose
    /// destination is missing — it holds the only copy of the binary.
    /// What: plants a stale aside under a reaped child's pid with no
    /// destination file, runs `move_aside` + `restore`, and asserts the
    /// binary is back at the destination with the stale copy's contents.
    #[test]
    fn sweep_restores_stale_aside_when_destination_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let stale = tmp.path().join(format!(".tctl.pre-cargo.{}.7", dead_pid()));
        write(&stale, "stranded-only-copy");

        let guard = OwnershipGuard::move_aside(tmp.path(), &names(&["tctl"])).expect("move");
        // The sweep restored the aside to the destination, so the guard then
        // moved the recovered file aside like any pre-existing binary.
        guard.restore().expect("restore");

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("tctl")).expect("read"),
            "stranded-only-copy",
            "a dead-pid aside with no destination must be restored"
        );
        assert!(hidden_entries(tmp.path()).is_empty(), "no litter remains");
    }

    /// Why: when the destination EXISTS, a dead-pid aside is pure litter —
    /// deleting it (rather than restoring) is what stops asides accumulating
    /// in `~/.cargo/bin` across interrupted runs.
    /// What: plants a stale aside next to a live destination, runs
    /// `move_aside` + `restore`, and asserts the destination is unchanged and
    /// the litter is gone.
    #[test]
    fn sweep_deletes_stale_aside_when_destination_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp.path().join("tagent"), "current");
        let stale = tmp
            .path()
            .join(format!(".tagent.pre-cargo.{}.3", dead_pid()));
        write(&stale, "old-litter");

        let guard = OwnershipGuard::move_aside(tmp.path(), &names(&["tagent"])).expect("move");
        guard.restore().expect("restore");

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("tagent")).expect("read"),
            "current",
            "the live destination must win over dead-pid litter"
        );
        assert!(hidden_entries(tmp.path()).is_empty(), "litter deleted");
    }

    /// Why: an aside owned by a LIVE process belongs to a concurrent guard —
    /// stealing it would race that guard's own settle.
    /// What: plants an aside under this test process's (live) pid and asserts
    /// the sweep leaves it untouched.
    #[test]
    fn sweep_leaves_live_process_asides_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let live = tmp
            .path()
            .join(format!(".tm.pre-cargo.{}.9999", std::process::id()));
        write(&live, "owned-by-a-live-guard");

        let guard = OwnershipGuard::move_aside(tmp.path(), &names(&["tm"])).expect("move");
        guard.commit().expect("commit (nothing moved)");

        assert!(live.exists(), "a live process's aside must never be swept");
        assert!(!tmp.path().join("tm").exists(), "nothing restored");
    }
}
