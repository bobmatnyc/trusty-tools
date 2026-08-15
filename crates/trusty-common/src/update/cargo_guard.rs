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
//! Test: `tests` below — happy path, restore-on-failure, the
//! skip-restores-rather-than-deletes case, and concurrent guards on the same
//! binary set.

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
    /// Test: `move_aside_then_commit_removes_asides`,
    /// `move_aside_failure_restores_already_moved` is covered indirectly by
    /// `restore_puts_every_binary_back` (same restore path).
    pub(crate) fn move_aside(bin_dir: &Path, binaries: &[String]) -> anyhow::Result<Self> {
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        for name in binaries {
            let dest = bin_dir.join(name);
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
    /// `commit_restores_when_cargo_skipped_writing`.
    pub(crate) fn commit(self) -> anyhow::Result<()> {
        let mut failures: Vec<String> = Vec::new();
        for (dest, aside) in &self.moved {
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
    pub(crate) fn restore(self) -> anyhow::Result<()> {
        restore_pairs(&self.moved)
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
}
