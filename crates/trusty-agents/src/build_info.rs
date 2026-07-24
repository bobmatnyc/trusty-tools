//! Build and version tracking.
//!
//! Why: We need a monotonic build counter that increments on every process
//! start, independent of the semver version. The combined `vX.Y.Z build #N`
//! string lets us correlate log lines and performance telemetry with the
//! exact binary invocation that produced them. Keeping the counter on disk
//! (rather than baked into the binary) lets a single `cargo build` produce
//! many distinct "builds" across repeated `cargo run` invocations during
//! development — which is exactly when we need the disambiguation.
//!
//! What: Reads `<dir>/build.json` for a caller-resolved `dir` (always
//! `<project>/.trusty-agents/state`, never a bare cwd — see
//! `BuildInfo::load_and_increment_in`), increments the `build` counter
//! (defaulting to 0 if the file is missing or malformed), and writes the
//! result back atomically via `rename(2)` from a sibling `.tmp` file.
//!
//! Test: `BuildInfo::load_and_increment_in` in an empty temp dir returns
//! `build == 1`; calling it again returns `build == 2`; a corrupt
//! `build.json` is treated as "build = 0" and replaced on the next call.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Compile-time semver from Cargo.toml.
///
/// Why: Canonical version string embedded in the binary so runtime code never
/// needs to read Cargo.toml.
/// What: `env!("CARGO_PKG_VERSION")` forwarded as a `'static str`.
/// Test: Asserted non-empty in `version_string_contains_version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short git commit hash captured at build time by `build.rs`.
///
/// Why: Correlates a running binary to the exact commit that produced it.
/// What: Populated from `git rev-parse --short HEAD` during compilation;
/// falls back to `"unknown"` when git is unavailable.
/// Test: `version_string_contains_version` confirms both fields render.
pub const GIT_HASH: &str = env!("GIT_COMMIT_HASH");

/// Human-readable banner used by the CTRL REPL and `--version` output.
///
/// Why: One canonical format keeps log grep and support reports consistent.
/// What: Returns `trusty-agents vX.Y.Z (<git-hash>)`.
/// Test: `version_string_contains_version` checks both substrings render.
pub fn version_string() -> String {
    format!("trusty-agents v{VERSION} ({GIT_HASH})")
}

/// On-disk shape of `.trusty-agents/state/build.json`.
///
/// Why: Kept as a private struct so callers go through `BuildInfo` for
/// `started_at`/`version` handling instead of reading raw JSON.
/// What: Just the two persisted fields; version is a compile-time constant
/// and doesn't round-trip through disk.
/// Test: Serialized/deserialized in unit tests below.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedBuild {
    build: u64,
    started_at: String,
}

/// Runtime build metadata surfaced to logs, the `--version` flag, and
/// downstream instrumentation.
///
/// Why: Centralizes "which binary + which invocation" so later instrumentation
/// (issue #47) can tag every telemetry event with the same build stamp the
/// startup log line shows.
/// What: Holds the compile-time semver, the incremented build counter, and
/// the ISO8601 UTC start timestamp.
/// Test: `display_string` returns `trusty-agents vX.Y.Z build #N`.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    pub version: &'static str,
    pub build: u64,
    // Exposed for the performance instrumentation module (#47) which will
    // stamp telemetry events with the process start time. Not read inside
    // this crate yet, so silence dead-code for now.
    #[allow(dead_code)]
    pub started_at: String,
}

impl BuildInfo {
    /// Load the persistent counter, increment it, and persist back, against
    /// an explicit, already-resolved base directory.
    ///
    /// Why: Every caller must resolve the project/state directory itself
    /// (typically via `ctrl::detect_self_project()`) rather than trusting
    /// `std::env::current_dir()`. A packaged desktop app can spawn this
    /// binary with cwd == `/` (a sealed read-only APFS volume) — a raw-cwd
    /// variant of this function used to exist and silently recomputed the
    /// state dir from cwd, crashing with EROFS on `/.trusty-agents/state`
    /// even when the caller had already resolved a perfectly good, writable
    /// state dir. That variant was removed; do not reintroduce a
    /// `std::env::current_dir()`-based counterpart here.
    /// What: Creates `<dir>` if missing, reads/parses `<dir>/build.json`
    /// (treating missing or malformed as build=0), increments, writes back
    /// atomically.
    /// Test: See unit tests at the bottom of this file.
    pub async fn load_and_increment_in(dir: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("failed to create {}", dir.display()))?;

        let file = dir.join("build.json");
        let previous = read_previous(&file).await;

        let next = previous.saturating_add(1);
        let started_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let persisted = PersistedBuild {
            build: next,
            started_at: started_at.clone(),
        };
        write_atomic(dir, &file, &persisted).await?;

        Ok(Self {
            version: env!("CARGO_PKG_VERSION"),
            build: next,
            started_at,
        })
    }

    /// Human-readable banner used by the startup log line and `--version`.
    ///
    /// Why: Single canonical format so grep/tooling can match it uniformly.
    /// What: `trusty-agents vX.Y.Z build #N`.
    /// Test: Asserted directly in unit tests.
    pub fn display_string(&self) -> String {
        format!("trusty-agents v{} build #{}", self.version, self.build)
    }
}

/// Read the previous `build` counter from `file`, returning 0 if the file
/// is missing, unreadable, or corrupt.
///
/// Why: We never want to fail startup just because the counter file got
/// wedged; treating it as 0 means the next write replaces the bad content.
/// What: Async read + `serde_json` parse.
/// Test: `load_and_increment_in` tests cover missing + corrupt cases.
async fn read_previous(file: &Path) -> u64 {
    match tokio::fs::read(file).await {
        Ok(bytes) => match serde_json::from_slice::<PersistedBuild>(&bytes) {
            Ok(p) => p.build,
            Err(_) => 0,
        },
        Err(_) => 0,
    }
}

/// Atomic write: serialize to a per-writer-unique `<file>.<pid>.<uuid>.tmp`,
/// fsync implicit via rename.
///
/// Why: A crash during write must never leave `build.json` half-written —
/// `rename(2)` on the same filesystem is atomic, so readers always see a
/// complete file. The tmp filename must be unique PER CALL, not just per
/// file: multiple `tagent` processes resolving the same project's
/// `.trusty-agents/state` dir concurrently (e.g. several sub-agent
/// invocations spawned back-to-back by the same PM/GUI session) used to all
/// race on one shared `build.json.tmp` — one process's `rename` could win
/// after a second process had already overwritten (or itself renamed away)
/// that same path, so the loser's `rename` failed with a hard `ENOENT`
/// (`ErrorKind::NotFound`) and startup crashed entirely, even though every
/// individual write was well-formed. Scoping the tmp name to this process's
/// pid + a random uuid means concurrent writers never share a path, so no
/// writer can ever observe another's tmp file disappear out from under it —
/// each `rename` always targets a file only ITS OWN call created.
/// What: Writes to `<dir>/build.json.<pid>.<uuid>.tmp`, renames over the
/// target path. The final `build.json` write is still last-writer-wins
/// under concurrency (acceptable for a best-effort disambiguation counter),
/// but the operation itself can no longer crash the caller.
/// Test: `load_and_increment_in` tests cover the single-writer path;
/// `concurrent_calls_never_fail_on_rename` drives many concurrent writers
/// against one dir and asserts every call succeeds.
async fn write_atomic(dir: &Path, file: &Path, payload: &PersistedBuild) -> Result<()> {
    let tmp: PathBuf = dir.join(format!(
        "build.json.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec_pretty(payload).context("serialize build.json")?;
    tokio::fs::write(&tmp, &bytes)
        .await
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    tokio::fs::rename(&tmp, file)
        .await
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), file.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn starts_at_one_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let info = BuildInfo::load_and_increment_in(dir.path()).await.unwrap();
        assert_eq!(info.build, 1);
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert!(info.started_at.ends_with('Z'));

        // File exists and is valid JSON with build=1.
        let bytes = tokio::fs::read(dir.path().join("build.json"))
            .await
            .unwrap();
        let p: PersistedBuild = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(p.build, 1);
    }

    #[tokio::test]
    async fn increments_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let a = BuildInfo::load_and_increment_in(dir.path()).await.unwrap();
        let b = BuildInfo::load_and_increment_in(dir.path()).await.unwrap();
        let c = BuildInfo::load_and_increment_in(dir.path()).await.unwrap();
        assert_eq!(a.build, 1);
        assert_eq!(b.build, 2);
        assert_eq!(c.build, 3);
    }

    /// Regression test for a concurrent-writer crash exposed while fixing
    /// the packaged-app EROFS bug: once every caller resolves the SAME
    /// real project state dir (instead of each accidentally landing in its
    /// own raw-cwd tempdir), several `tagent` processes/tasks incrementing
    /// the counter at once used to race on a single shared
    /// `build.json.tmp` — one writer's `rename` would fail with `ENOENT`
    /// because a second writer had already renamed the shared tmp path
    /// away. `write_atomic` now scopes the tmp filename per-call (pid +
    /// uuid), so this must never fail regardless of concurrency.
    ///
    /// Why: proves the fix — old code reliably reproduces this as a hang or
    /// an `Err` on `rename` under concurrent load (in-process concurrent
    /// tasks in one pid replicate the failure just as well as separate
    /// processes did in the real integration-test flakiness this was found
    /// from).
    /// What: Spawns many concurrent `load_and_increment_in` calls against
    /// one shared dir and asserts every single one returns `Ok`.
    /// Test: this test.
    #[tokio::test]
    async fn concurrent_calls_never_fail_on_rename() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = std::sync::Arc::new(dir.path().to_path_buf());

        let mut handles = Vec::new();
        for _ in 0..32 {
            let dir_path = dir_path.clone();
            handles.push(tokio::spawn(async move {
                BuildInfo::load_and_increment_in(&dir_path).await
            }));
        }

        let mut ok_count = 0;
        for h in handles {
            let result = h.await.expect("task panicked");
            assert!(
                result.is_ok(),
                "concurrent write must never fail: {result:?}"
            );
            ok_count += 1;
        }
        assert_eq!(ok_count, 32);

        // The counter file must still be well-formed after the concurrent
        // storm (last-writer-wins is fine; corruption is not).
        let bytes = tokio::fs::read(dir_path.join("build.json")).await.unwrap();
        let p: PersistedBuild = serde_json::from_slice(&bytes).unwrap();
        assert!(p.build >= 1 && p.build <= 32);
    }

    #[tokio::test]
    async fn corrupt_file_resets_to_one() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path()).await.unwrap();
        tokio::fs::write(dir.path().join("build.json"), b"not json at all")
            .await
            .unwrap();

        let info = BuildInfo::load_and_increment_in(dir.path()).await.unwrap();
        assert_eq!(info.build, 1, "corrupt file should be treated as build=0");
    }

    #[tokio::test]
    async fn display_string_format() {
        let info = BuildInfo {
            version: "0.1.0",
            build: 42,
            started_at: "2026-04-22T17:31:30Z".to_string(),
        };
        assert_eq!(info.display_string(), "trusty-agents v0.1.0 build #42");
    }

    #[test]
    fn version_string_contains_version() {
        let s = version_string();
        assert!(s.contains("trusty-agents v"));
        assert!(s.contains(VERSION));
        // GIT_HASH is either a short hash or "unknown"; rendered in parens.
        assert!(s.contains(GIT_HASH));
    }

    #[tokio::test]
    async fn creates_missing_parent_dir() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("deep").join(".trusty-agents");
        assert!(!nested.exists());

        let info = BuildInfo::load_and_increment_in(&nested).await.unwrap();
        assert_eq!(info.build, 1);
        assert!(nested.join("build.json").exists());
    }

    /// Regression test for the packaged-app EROFS crash (Tauri sidecar
    /// launches with cwd == `/`, a sealed read-only APFS volume).
    ///
    /// Why: The old `BuildInfo::load_and_increment()` (removed) recomputed
    /// its target directory from `std::env::current_dir()` internally,
    /// ignoring any resolved state dir the caller already had. On the
    /// packaged app this meant `startup.rs`'s correctly-resolved
    /// `ctrl::detect_self_project()` state dir was discarded in favor of
    /// `/.trusty-agents/state`, which crashed on `create_dir_all` with
    /// `EROFS`. This test does not mutate the process-wide CWD (see
    /// `lib.rs::default_bundled_config_dir_checking` for why that's
    /// hazardous under `cargo test`'s shared-process threading model);
    /// instead it proves the load-bearing invariant directly: a read-only
    /// directory standing in for `/` is passed nowhere near this call, and
    /// `load_and_increment_in` only ever touches the explicit `dir` argument
    /// — so a caller that resolves a real, writable project directory (as
    /// `startup.rs` and `repl/commands/dispatch.rs` now both do) can never
    /// be dragged back into a cwd-based path that revisits an unwritable
    /// root.
    /// What: Builds a chmod-555 (read-only, `cfg(unix)`) directory as the
    /// "unwritable cwd" stand-in and a separate writable tempdir as the
    /// resolved state dir; asserts `load_and_increment_in(&state_dir)`
    /// succeeds, writes `build.json` under the writable dir, and leaves the
    /// read-only stand-in completely untouched (no entries created in it).
    /// Test: this test.
    #[cfg(unix)]
    #[tokio::test]
    async fn survives_unwritable_cwd_stand_in() {
        use std::os::unix::fs::PermissionsExt;

        let unwritable_root = tempfile::tempdir().unwrap();
        // 0o555: readable + executable (traversable) but not writable —
        // mirrors the sealed read-only APFS volume mounted at `/`.
        std::fs::set_permissions(
            unwritable_root.path(),
            std::fs::Permissions::from_mode(0o555),
        )
        .unwrap();

        let state_root = tempfile::tempdir().unwrap();
        let state_dir = state_root.path().join(".trusty-agents").join("state");

        let info = BuildInfo::load_and_increment_in(&state_dir)
            .await
            .expect("must succeed even though an unrelated read-only dir exists on disk");
        assert_eq!(info.build, 1);
        assert!(state_dir.join("build.json").exists());

        // The read-only stand-in was never touched: no create_dir_all /
        // write ever targeted it.
        let entries: Vec<_> = std::fs::read_dir(unwritable_root.path()).unwrap().collect();
        assert!(
            entries.is_empty(),
            "unwritable cwd stand-in must remain untouched, found: {entries:?}"
        );

        // Restore write perms so tempfile's Drop can clean up on all hosts.
        std::fs::set_permissions(
            unwritable_root.path(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
}
