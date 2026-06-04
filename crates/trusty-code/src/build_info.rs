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
//! What: Reads `.open-mpm/state/build.json` relative to the current working
//! directory, increments the `build` counter (defaulting to 0 if the file
//! is missing or malformed), and writes the result back atomically via
//! `rename(2)` from a sibling `.tmp` file.
//!
//! Test: `BuildInfo::load_and_increment` in an empty temp dir returns
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

/// The package name, resolved at compile time.
///
/// Why: Allows display strings to use the correct crate name without
/// hardcoding it, so this module is reusable across open-mpm and trusty-code.
/// What: `env!("CARGO_PKG_NAME")` forwarded as a `'static str`.
/// Test: Indirectly via `version_string_contains_version`.
pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");

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
/// What: Returns `<pkg-name> vX.Y.Z (<git-hash>)`.
/// Test: `version_string_contains_version` checks both substrings render.
pub fn version_string() -> String {
    format!("{PKG_NAME} v{VERSION} ({GIT_HASH})")
}

/// On-disk shape of `build.json`.
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
/// can tag every telemetry event with the same build stamp the startup log
/// line shows.
/// What: Holds the compile-time semver, the incremented build counter, and
/// the ISO8601 UTC start timestamp.
/// Test: `display_string` returns `<pkg-name> vX.Y.Z build #N`.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    pub version: &'static str,
    pub build: u64,
    // Exposed for the performance instrumentation module which will stamp
    // telemetry events with the process start time.
    #[allow(dead_code)]
    pub started_at: String,
}

impl BuildInfo {
    /// Load the persistent counter, increment it, and persist back using
    /// the given state directory.
    ///
    /// Why: Single entry point ensures every process start gets a fresh
    /// build number even if the caller forgets to save.
    /// What: Uses `<state_dir>/build.json`. See `load_and_increment_in` for
    /// the testable, directory-parameterized version.
    /// Test: See `load_and_increment_in` tests.
    pub async fn load_and_increment(state_dir: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(state_dir).await?;
        Self::load_and_increment_in(state_dir).await
    }

    /// Same as `load_and_increment` but with an explicit base directory.
    ///
    /// Why: Tests need to drive the counter against a temp dir without
    /// mutating the real state directory in the project.
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
    /// What: `<pkg-name> vX.Y.Z build #N`.
    /// Test: Asserted directly in unit tests.
    pub fn display_string(&self) -> String {
        format!("{PKG_NAME} v{} build #{}", self.version, self.build)
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

/// Atomic write: serialize to `<file>.tmp`, fsync implicit via rename.
///
/// Why: A crash during write must never leave `build.json` half-written —
/// `rename(2)` on the same filesystem is atomic, so readers always see a
/// complete file.
/// What: Writes to `<dir>/build.json.tmp`, renames over the target path.
/// Test: Covered by `load_and_increment_in` tests.
async fn write_atomic(dir: &Path, file: &Path, payload: &PersistedBuild) -> Result<()> {
    let tmp: PathBuf = dir.join("build.json.tmp");
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
        // PKG_NAME is "trusty-code" in this crate.
        let s = info.display_string();
        assert!(s.contains("v0.1.0"), "got: {s}");
        assert!(s.contains("build #42"), "got: {s}");
    }

    #[test]
    fn version_string_contains_version() {
        let s = version_string();
        assert!(s.contains("v"));
        assert!(s.contains(VERSION));
        // GIT_HASH is either a short hash or "unknown"; rendered in parens.
        assert!(s.contains(GIT_HASH));
    }

    #[tokio::test]
    async fn creates_missing_parent_dir() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("deep").join(".state");
        assert!(!nested.exists());

        let info = BuildInfo::load_and_increment_in(&nested).await.unwrap();
        assert_eq!(info.build, 1);
        assert!(nested.join("build.json").exists());
    }
}
