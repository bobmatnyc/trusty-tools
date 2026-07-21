//! Version-parity drift detector (issue #3366).
//!
//! Why: on 2026-07-19/20 `trusty-common` and `trusty-agents-common` each
//! carried several commits of source changes that landed on `main` WITHOUT a
//! matching version bump, while the OLD version number was already live on
//! crates.io. `cargo build`/`cargo test` never noticed because the
//! workspace's `[patch.crates-io]` table redirects sibling deps to local
//! paths — the gap only surfaces when `cargo publish` resolves a crate
//! standalone against the real registry, by which point it is a release
//! blocker instead of a merge-time finding. See issue #3366 for the full
//! incident writeup.
//!
//! What: for a given crate, compares the `src/**` file contents on disk
//! against the `src/**` file contents inside the crates.io tarball for the
//! crate's OWN current `Cargo.toml` version. If that version is not yet
//! published, there is nothing to compare and the crate passes trivially
//! (`ParityStatus::NotYetPublished` — this is the normal, safe "just bumped,
//! about to release" state). If the version IS already published, the source
//! trees must match byte-for-byte; any difference is
//! `ParityStatus::Drift(..)` — the exact defect class from #3366.
//!
//! All network access is isolated behind the [`PublishedFetcher`] trait
//! (implemented for real crates.io access in [`fetch::CratesIoFetcher`]) so
//! the drift-detection logic itself (extraction, diffing, status decision) is
//! fully unit-tested here with an in-memory fake — no network required to run
//! `cargo test -p trusty-publish-guard`.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

pub mod fetch;

/// One file-level difference between the local working tree and the
/// published tarball's `src/` contents, keyed by path relative to `src/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftEntry {
    /// Present locally, absent from the published tarball.
    Added(String),
    /// Present in the published tarball, absent locally.
    Removed(String),
    /// Present in both, but byte content differs.
    Modified(String),
}

impl DriftEntry {
    /// The `src/`-relative path this entry refers to, regardless of variant.
    pub fn path(&self) -> &str {
        match self {
            DriftEntry::Added(p) | DriftEntry::Removed(p) | DriftEntry::Modified(p) => p,
        }
    }
}

impl std::fmt::Display for DriftEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriftEntry::Added(p) => write!(f, "+ added (local only, not on crates.io): src/{p}"),
            DriftEntry::Removed(p) => {
                write!(f, "- removed (published only, missing locally): src/{p}")
            }
            DriftEntry::Modified(p) => {
                write!(f, "~ modified (content differs from published): src/{p}")
            }
        }
    }
}

/// The outcome of comparing one crate's local `src/` tree against its
/// published tarball.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParityStatus {
    /// The crate's current `Cargo.toml` version has not been published yet —
    /// nothing to compare against. This is the ordinary "bumped, not yet
    /// released" state and is NOT a failure.
    NotYetPublished,
    /// The local `src/` tree matches the published tarball byte-for-byte.
    Parity,
    /// The local `src/` tree differs from what crates.io serves under the
    /// SAME version number — the #3366 defect.
    Drift(Vec<DriftEntry>),
}

/// Seam separating the (network-dependent, untestable-without-a-live-registry)
/// crates.io access from the (pure, fully-testable) drift-detection logic.
///
/// Implement this against a fake in tests; [`fetch::CratesIoFetcher`] is the
/// only production implementation and talks to the real crates.io API.
pub trait PublishedFetcher {
    /// Returns `true` if `name`@`version` is live on the registry, `false` if
    /// not yet published. Any other outcome (network error, unexpected
    /// response) must be surfaced as `Err` — never silently coerced to
    /// `false`, which would hide a real drift behind a false "not published
    /// yet, nothing to check" pass.
    fn is_version_live(&self, name: &str, version: &str) -> Result<bool>;

    /// Fetches the raw `.crate` tarball (gzip-compressed tar) for
    /// `name`@`version`. Only called after `is_version_live` returned `true`.
    fn fetch_tarball(&self, name: &str, version: &str) -> Result<Vec<u8>>;
}

/// Extracts every regular file under `<package_dir>/src/` from a gzip'd
/// crates.io tarball into an in-memory `path -> bytes` map, keyed by path
/// relative to `src/` (matching [`local_src`]'s keying so the two maps can be
/// diffed directly).
///
/// `package_dir` is the tarball's top-level directory, i.e. `"<name>-<version>"`
/// — crates.io tarballs always nest content one level under a directory named
/// after the package and version.
pub fn extract_published_src(
    tarball_gz: &[u8],
    package_dir: &str,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let decoder = flate2::read::GzDecoder::new(tarball_gz);
    let mut archive = tar::Archive::new(decoder);
    let mut out = BTreeMap::new();
    let prefix = format!("{package_dir}/src/");

    for entry in archive.entries().context("reading tarball entries")? {
        let mut entry = entry.context("reading a tarball entry")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .context("reading entry path")?
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(rel) = path.strip_prefix(&prefix) {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("reading entry contents")?;
            out.insert(rel.to_string(), buf);
        }
    }
    Ok(out)
}

/// Reads every regular file under `<crate_root>/src/` from disk into an
/// in-memory `path -> bytes` map, keyed by path relative to `src/`.
///
/// Returns an empty map (not an error) if the crate has no `src/` directory —
/// callers decide what that means; this function only reports what exists.
pub fn local_src(crate_root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let src_dir = crate_root.join("src");
    let mut out = BTreeMap::new();
    if !src_dir.exists() {
        return Ok(out);
    }
    for entry in walkdir::WalkDir::new(&src_dir) {
        let entry = entry.with_context(|| format!("walking {}", src_dir.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&src_dir)
            .expect("walkdir entries are always under src_dir")
            .to_string_lossy()
            .replace('\\', "/");
        let content = std::fs::read(entry.path())
            .with_context(|| format!("reading {}", entry.path().display()))?;
        out.insert(rel, content);
    }
    Ok(out)
}

/// Compares two `path -> bytes` maps (as produced by [`local_src`] and
/// [`extract_published_src`]) and returns every difference, sorted by path
/// for deterministic output. Empty result means the two trees are identical.
pub fn diff_src(
    local: &BTreeMap<String, Vec<u8>>,
    published: &BTreeMap<String, Vec<u8>>,
) -> Vec<DriftEntry> {
    let mut drift = Vec::new();
    for (path, content) in local {
        match published.get(path) {
            None => drift.push(DriftEntry::Added(path.clone())),
            Some(published_content) if published_content != content => {
                drift.push(DriftEntry::Modified(path.clone()))
            }
            _ => {}
        }
    }
    for path in published.keys() {
        if !local.contains_key(path) {
            drift.push(DriftEntry::Removed(path.clone()));
        }
    }
    drift.sort_by(|a, b| a.path().cmp(b.path()));
    drift
}

/// Runs the full version-parity check for one crate: is its current
/// `Cargo.toml` version already live, and if so, does the local `src/` tree
/// match what was published under that version?
///
/// Precondition: `crate_root` is a directory containing (at minimum) a
/// `src/` subdirectory reflecting the crate's current on-disk state; `name`
/// and `version` are read from that crate's own `Cargo.toml` (the caller is
/// responsible for that lookup — this function only compares, it does not
/// resolve manifests).
pub fn check_crate(
    fetcher: &dyn PublishedFetcher,
    crate_root: &Path,
    name: &str,
    version: &str,
) -> Result<ParityStatus> {
    if !fetcher
        .is_version_live(name, version)
        .with_context(|| format!("checking whether {name} {version} is live on crates.io"))?
    {
        return Ok(ParityStatus::NotYetPublished);
    }

    let tarball = fetcher
        .fetch_tarball(name, version)
        .with_context(|| format!("fetching published tarball for {name} {version}"))?;
    let package_dir = format!("{name}-{version}");
    let published = extract_published_src(&tarball, &package_dir)
        .with_context(|| format!("extracting published src/ for {name} {version}"))?;
    let local = local_src(crate_root)
        .with_context(|| format!("reading local src/ for {name} at {}", crate_root.display()))?;

    let drift = diff_src(&local, &published);
    Ok(if drift.is_empty() {
        ParityStatus::Parity
    } else {
        ParityStatus::Drift(drift)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap as Map;
    use std::io::Write;
    use std::path::Path;

    /// In-memory fake for `PublishedFetcher` — the test seam. No network
    /// access anywhere in this module.
    struct FakeFetcher {
        live: Map<(String, String), Vec<u8>>,
        fail_lookup: bool,
    }

    impl FakeFetcher {
        fn empty() -> Self {
            Self {
                live: Map::new(),
                fail_lookup: false,
            }
        }

        fn with_published(name: &str, version: &str, tarball: Vec<u8>) -> Self {
            let mut live = Map::new();
            live.insert((name.to_string(), version.to_string()), tarball);
            Self {
                live,
                fail_lookup: false,
            }
        }

        fn failing() -> Self {
            Self {
                live: Map::new(),
                fail_lookup: true,
            }
        }
    }

    impl PublishedFetcher for FakeFetcher {
        fn is_version_live(&self, name: &str, version: &str) -> Result<bool> {
            if self.fail_lookup {
                anyhow::bail!("simulated crates.io outage");
            }
            Ok(self
                .live
                .contains_key(&(name.to_string(), version.to_string())))
        }

        fn fetch_tarball(&self, name: &str, version: &str) -> Result<Vec<u8>> {
            self.live
                .get(&(name.to_string(), version.to_string()))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no fake tarball registered for {name} {version}"))
        }
    }

    /// Builds a gzip'd tar in memory shaped like a real crates.io tarball:
    /// every file nested under `<package_dir>/src/`.
    fn build_tarball(package_dir: &str, files: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (rel, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            let path = format!("{package_dir}/src/{rel}");
            builder
                .append_data(&mut header, path, content.as_bytes())
                .expect("appending fake tarball entry");
        }
        let tar_bytes = builder.into_inner().expect("finalizing fake tar");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes)
            .expect("gzip-compressing fake tarball");
        gz.finish().expect("finishing gzip stream")
    }

    fn write_local_src(crate_root: &Path, files: &[(&str, &str)]) {
        for (rel, content) in files {
            let full = crate_root.join("src").join(rel);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
    }

    #[test]
    fn not_yet_published_when_version_absent_from_registry() {
        let tmp = tempfile::tempdir().unwrap();
        write_local_src(tmp.path(), &[("lib.rs", "pub fn x() {}")]);
        let fetcher = FakeFetcher::empty();

        let status = check_crate(&fetcher, tmp.path(), "demo", "0.1.0").unwrap();

        assert_eq!(status, ParityStatus::NotYetPublished);
    }

    #[test]
    fn parity_when_local_source_matches_published_tarball() {
        let tmp = tempfile::tempdir().unwrap();
        write_local_src(tmp.path(), &[("lib.rs", "pub fn x() {}")]);
        let tarball = build_tarball("demo-0.1.0", &[("lib.rs", "pub fn x() {}")]);
        let fetcher = FakeFetcher::with_published("demo", "0.1.0", tarball);

        let status = check_crate(&fetcher, tmp.path(), "demo", "0.1.0").unwrap();

        assert_eq!(status, ParityStatus::Parity);
    }

    /// This is the regression test for issue #3366's actual reported defect,
    /// modeled directly on the confirmed live incident: trusty-common's
    /// Cargo.toml stayed at 0.23.7 (already published) while commit
    /// `12e9d37d` added new embedder-module source on top of it. A checker
    /// that always returned `Parity` (i.e. the pre-fix state — no drift
    /// detection existed at all) would pass this fixture; the real
    /// diff-based implementation must not.
    #[test]
    fn drift_detected_when_local_source_changed_after_publish() {
        let tmp = tempfile::tempdir().unwrap();
        write_local_src(
            tmp.path(),
            &[
                ("lib.rs", "pub fn x() { 2 }"),
                ("new_module.rs", "pub fn y() {}"),
            ],
        );
        let tarball = build_tarball("demo-0.1.0", &[("lib.rs", "pub fn x() { 1 }")]);
        let fetcher = FakeFetcher::with_published("demo", "0.1.0", tarball);

        let status = check_crate(&fetcher, tmp.path(), "demo", "0.1.0").unwrap();

        match status {
            ParityStatus::Drift(entries) => {
                assert!(
                    entries.contains(&DriftEntry::Modified("lib.rs".to_string())),
                    "expected lib.rs to be flagged Modified, got {entries:?}"
                );
                assert!(
                    entries.contains(&DriftEntry::Added("new_module.rs".to_string())),
                    "expected new_module.rs to be flagged Added, got {entries:?}"
                );
            }
            other => panic!("expected Drift, got {other:?}"),
        }
    }

    #[test]
    fn drift_detects_file_removed_relative_to_published_tree() {
        let tmp = tempfile::tempdir().unwrap();
        write_local_src(tmp.path(), &[("lib.rs", "pub fn x() {}")]);
        let tarball = build_tarball(
            "demo-0.1.0",
            &[
                ("lib.rs", "pub fn x() {}"),
                ("legacy.rs", "pub fn old() {}"),
            ],
        );
        let fetcher = FakeFetcher::with_published("demo", "0.1.0", tarball);

        let status = check_crate(&fetcher, tmp.path(), "demo", "0.1.0").unwrap();

        assert_eq!(
            status,
            ParityStatus::Drift(vec![DriftEntry::Removed("legacy.rs".to_string())])
        );
    }

    #[test]
    fn registry_lookup_failure_surfaces_as_error_not_a_silent_pass() {
        let tmp = tempfile::tempdir().unwrap();
        write_local_src(tmp.path(), &[("lib.rs", "pub fn x() {}")]);
        let fetcher = FakeFetcher::failing();

        let result = check_crate(&fetcher, tmp.path(), "demo", "0.1.0");

        assert!(
            result.is_err(),
            "a registry lookup failure must fail closed (Err), never be coerced into \
             NotYetPublished or Parity"
        );
    }

    #[test]
    fn diff_src_is_order_independent_and_deterministic() {
        let mut local = Map::new();
        local.insert("b.rs".to_string(), b"b".to_vec());
        local.insert("a.rs".to_string(), b"a".to_vec());
        let mut published = Map::new();
        published.insert("a.rs".to_string(), b"a-changed".to_vec());
        published.insert("b.rs".to_string(), b"b".to_vec());

        let drift = diff_src(&local, &published);

        assert_eq!(drift, vec![DriftEntry::Modified("a.rs".to_string())]);
    }
}
