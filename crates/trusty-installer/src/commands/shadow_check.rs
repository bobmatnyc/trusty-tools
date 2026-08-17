//! PATH-shadow detection for a just-installed binary (#3554).
//!
//! Why: `tctl install` writes a binary to a concrete install directory
//! (`~/.local/bin` for the prebuilt path, `$CARGO_HOME/bin`/`~/.cargo/bin`
//! for the `cargo install` fallback) and health-gates THAT exact file (see
//! `install::install_one` / `trusty_common::update::verify_installed_binary_at_path`,
//! the primary #3554 fix). But a passing health gate says nothing about what
//! a user's actual shell will run next: if an OLDER copy of the same binary
//! name sits in a directory that precedes the install directory on `$PATH`
//! (the exact #3554 shape — a pre-existing `cargo install`-managed
//! `~/.cargo/bin/tm` shadowing a fresh `~/.local/bin/tm`), the operator's
//! `tm` keeps resolving to the stale binary even though the install itself
//! succeeded and reported the right version. This module makes that
//! condition loud and actionable instead of a silent, successful-looking
//! install (the secondary #3554 requirement).
//!
//! What: [`classify`] is the pure decision over a PATH-resolved path vs. the
//! concrete install path; [`detect`] composes it with the PATH resolution
//! (via an explicit search-path argument, never the live `$PATH` directly,
//! so it stays testable) and a best-effort version probe of the shadowing
//! binary into a [`ShadowReport`] the caller prints and folds into the
//! install exit code (see `install::install_all`).
//!
//! Test: `classify_*` covers the pure decision; `detect_*` exercises the
//! full resolve+classify+version-probe path against synthetic PATH
//! directories and fake executables — the real `$PATH`/filesystem locations
//! are never touched.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The verdict of comparing what `$PATH` resolves a binary name to against
/// the concrete file the installer just wrote.
///
/// Test: `classify_clear_when_paths_match`, `classify_shadowed_when_paths_differ`,
/// `classify_not_on_path_when_absent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShadowVerdict {
    /// `$PATH` resolves the binary name to the exact file just installed.
    Clear,
    /// `$PATH` does not resolve the binary name to anything — the install
    /// directory itself is not on `$PATH` yet. Distinct from `Shadowed`: no
    /// OTHER binary is being run instead, so this is not the #3554 failure
    /// mode (a fresh install onto an empty `$PATH` slot is a normal "add
    /// this directory to your PATH" case, already covered by `install.sh`'s
    /// own hint) and [`detect`] does not warn on it.
    NotOnPath,
    /// `$PATH` resolves the binary name to a DIFFERENT file than the one
    /// just installed — THE #3554 shadowing condition.
    Shadowed {
        /// The path a plain shell invocation of the binary name would run.
        resolved: PathBuf,
    },
}

/// Pure decision: does `on_path` refer to the same file as `install_path`?
///
/// Why: isolating the comparison from the PATH-resolution side effect keeps
/// the decision itself exhaustively unit-testable.
/// What: `None` (nothing resolved) -> [`ShadowVerdict::NotOnPath`]; `Some(p)`
/// that canonicalizes to the same file as `install_path` ->
/// [`ShadowVerdict::Clear`]; otherwise -> [`ShadowVerdict::Shadowed`].
/// Test: `classify_clear_when_paths_match`, `classify_shadowed_when_paths_differ`,
/// `classify_not_on_path_when_absent`.
pub fn classify(on_path: Option<&Path>, install_path: &Path) -> ShadowVerdict {
    match on_path {
        None => ShadowVerdict::NotOnPath,
        Some(p) if same_file(p, install_path) => ShadowVerdict::Clear,
        Some(p) => ShadowVerdict::Shadowed {
            resolved: p.to_path_buf(),
        },
    }
}

/// Compare two paths for referring to the same on-disk file.
///
/// Why: canonicalizing first means a symlink, a `./` prefix, or any other
/// non-canonical spelling difference does not register as a false
/// "shadowed" verdict. Falls back to a plain equality check when either path
/// cannot be canonicalized (e.g. it does not exist) rather than panicking.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// Resolve `binary_name` against an EXPLICIT search path, never the live
/// process `$PATH` directly.
///
/// Why: production passes the real `$PATH`; tests pass a synthetic one — the
/// function itself has no ambient dependency on process global state, which
/// is what makes [`detect`] deterministically testable.
/// What: thin wrapper over `which::which_in`; returns `None` when the binary
/// is not found in any of `search_path`'s directories.
fn resolve_on_path(binary_name: &str, search_path: &OsStr) -> Option<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    which::which_in(binary_name, Some(search_path), cwd).ok()
}

/// The actionable PATH-shadow report for a just-installed binary (#3554).
///
/// Test: `detect_reports_shadow_with_both_paths_and_versions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadowReport {
    /// The binary name checked (e.g. `tm`).
    pub binary_name: String,
    /// The concrete path the installer just wrote.
    pub install_path: PathBuf,
    /// The version the installer believes it just installed there.
    pub install_version: Option<String>,
    /// The path a plain shell invocation resolves to instead.
    pub shadowing_path: PathBuf,
    /// The version reported by the shadowing binary (best-effort — `None` if
    /// it could not be probed).
    pub shadowing_version: Option<String>,
}

impl ShadowReport {
    /// Render the loud, actionable warning text (#3554) — names both paths,
    /// both versions, and what to do about it, per the issue's requirement
    /// that a shadow condition must never be a silent success.
    ///
    /// Test: `detect_reports_shadow_with_both_paths_and_versions` asserts the
    /// rendered message names every required detail.
    pub fn message(&self) -> String {
        let install_dir = self
            .install_path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        format!(
            "PATH SHADOWED: installed {bin} {installed_v} to {install_path}, but the shell \
             currently resolves `{bin}` to {shadow_path} ({shadow_v}) instead — the new \
             version will NOT take effect until you remove/upgrade {shadow_path} or put \
             {install_dir} EARLIER on $PATH, then start a new shell (or run `hash -r`).",
            bin = self.binary_name,
            installed_v = self.install_version.as_deref().unwrap_or("<unknown>"),
            install_path = self.install_path.display(),
            shadow_path = self.shadowing_path.display(),
            shadow_v = self.shadowing_version.as_deref().unwrap_or("<unknown>"),
        )
    }
}

/// Detect PATH shadowing for a just-installed binary (#3554).
///
/// Why: the single entry point production code calls right after the health
/// gate passes on `install_path` — see `install::install_all`. A silent
/// success here is exactly the #3554 failure mode: the install genuinely
/// succeeded (the right bytes are on disk, health-gated correctly) but the
/// operator's shell still runs something else.
///
/// What: resolves `binary_name` against `search_path` (the real `$PATH` in
/// production, a synthetic one in tests); returns `Some(ShadowReport)` only
/// for the genuine [`ShadowVerdict::Shadowed`] case (a DIFFERENT file
/// resolves). Returns `None` for [`ShadowVerdict::Clear`] and
/// [`ShadowVerdict::NotOnPath`] — the latter is a distinct, lower-severity
/// condition already surfaced by `install.sh` (see [`ShadowVerdict::NotOnPath`]'s
/// doc).
///
/// #3554 review (HIGH): the shadowing file is discovered via `$PATH` — an
/// arbitrary, untrusted file that is strictly LESS trustworthy than the
/// binary the health gate just verified. Probing its `--version` therefore
/// goes through the SAME timeout-guarded primitive the health gate uses
/// (`trusty_common::update::verify_installed_binary_at_path`, a 10-second
/// `tokio::time::timeout`), never the un-timed-out
/// `update_engine::installed_version`. A shadowing binary that hangs on
/// `--version` (a shell shim blocked on stdin, a renamed/broken executable,
/// a non-CLI program under the same name) must never hang the unattended
/// `curl -sSf … | sh -s -- -y` flow on the very step meant to make failure
/// loud — a timed-out or failed probe degrades to `shadowing_version: None`
/// (the SHADOW itself is still reported; only the version detail is best
/// effort).
///
/// Test: `detect_reports_shadow_with_both_paths_and_versions`,
/// `detect_returns_none_when_path_resolves_to_installed_binary`,
/// `detect_returns_none_when_not_on_path`,
/// `detect_does_not_hang_on_an_unresponsive_shadowing_binary`.
pub async fn detect(
    binary_name: &str,
    install_path: &Path,
    install_version: Option<&str>,
    search_path: &OsStr,
) -> Option<ShadowReport> {
    let resolved = resolve_on_path(binary_name, search_path);
    match classify(resolved.as_deref(), install_path) {
        ShadowVerdict::Clear | ShadowVerdict::NotOnPath => None,
        ShadowVerdict::Shadowed { resolved } => {
            let shadowing_version =
                trusty_common::update::verify_installed_binary_at_path(&resolved)
                    .await
                    .ok()
                    .and_then(|raw| super::update_engine::extract_version_from_line(&raw));
            Some(ShadowReport {
                binary_name: binary_name.to_owned(),
                install_path: install_path.to_owned(),
                install_version: install_version.map(str::to_owned),
                shadowing_path: resolved,
                shadowing_version,
            })
        }
    }
}

/// Detect PATH shadowing for EVERY binary a member just placed (#5805).
///
/// Why: [`detect`] takes one name, and `install`/`upgrade` passed
/// `StableMember::binary` — the health-probe name. Three members ship a second
/// binary, and for trusty-installer the unprobed one is `tctl`, the name
/// operators actually type and the one `--dry-run` now advertises. A stale
/// `tctl` earlier on `$PATH` therefore kept winning every shell invocation
/// while the install reported clear, which is precisely the #3554 failure class
/// the primary-name check exists to catch.
///
/// What: derives each sibling's path from `primary_path`'s directory — the
/// install directory both write paths use — and runs [`detect`] on each. A name
/// with no file at that path is SKIPPED, not reported: a shadow claim compares
/// against a binary that was actually placed, and a name the tarball never
/// shipped was never placed. Returns every genuine report, in `binary_names`
/// order.
///
/// # Postconditions
/// - Empty result means no placed binary of this member is shadowed.
/// - No report names a path that does not exist on disk.
///
/// Test: `detect_all_reports_a_shadowed_alias_when_the_primary_is_clear`,
/// `detect_all_skips_a_binary_that_was_never_placed`.
pub async fn detect_all(
    binary_names: &[String],
    primary_path: &Path,
    install_version: Option<&str>,
    search_path: &OsStr,
) -> Vec<ShadowReport> {
    let Some(dir) = primary_path.parent() else {
        return Vec::new();
    };
    let mut reports = Vec::new();
    for name in binary_names {
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        if let Some(r) = detect(name, &path, install_version, search_path).await {
            reports.push(r);
        }
    }
    reports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn write_versioned_binary(path: &Path, version_line: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\necho '{version_line}'\nexit 0\n"))
            .expect("write fake binary");
        let mut perms = std::fs::metadata(path)
            .expect("stat fake binary")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod fake binary");
    }

    // ── classify (pure) ─────────────────────────────────────────────────

    #[test]
    fn classify_clear_when_paths_match() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let p = tmp.path().join("tm");
        std::fs::write(&p, b"x").expect("write");
        assert_eq!(classify(Some(&p), &p), ShadowVerdict::Clear);
    }

    #[test]
    fn classify_shadowed_when_paths_differ() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::write(&a, b"x").expect("write a");
        std::fs::write(&b, b"y").expect("write b");
        assert_eq!(
            classify(Some(&a), &b),
            ShadowVerdict::Shadowed { resolved: a }
        );
    }

    #[test]
    fn classify_not_on_path_when_absent() {
        let install_path = Path::new("/nonexistent/install/tm");
        assert_eq!(classify(None, install_path), ShadowVerdict::NotOnPath);
    }

    // ── detect (resolve + classify + version probe) ────────────────────

    /// THE #3554 regression shape: a stale binary in a directory placed
    /// EARLIER on the search path than the just-installed one. Must report
    /// `Shadowed`, naming both paths and both versions.
    #[cfg(unix)]
    #[tokio::test]
    async fn detect_reports_shadow_with_both_paths_and_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let early_dir = tmp.path().join("early-cargo-bin");
        std::fs::create_dir_all(&early_dir).expect("mkdir early");
        write_versioned_binary(&early_dir.join("tm"), "trusty-mpm 0.19.26");

        let install_dir = tmp.path().join("local-bin");
        std::fs::create_dir_all(&install_dir).expect("mkdir install");
        let install_path = install_dir.join("tm");
        write_versioned_binary(&install_path, "trusty-mpm 0.19.29");

        let search_path = format!("{}:{}", early_dir.display(), install_dir.display());

        let report = detect(
            "tm",
            &install_path,
            Some("0.19.29"),
            OsStr::new(&search_path),
        )
        .await
        .expect("shadow must be detected — an older copy precedes the install dir on PATH");

        assert_eq!(report.shadowing_path, early_dir.join("tm"));
        assert_eq!(report.shadowing_version.as_deref(), Some("0.19.26"));
        assert_eq!(report.install_path, install_path);
        assert_eq!(report.install_version.as_deref(), Some("0.19.29"));

        let msg = report.message();
        assert!(
            msg.contains("0.19.29"),
            "message must name the install version: {msg}"
        );
        assert!(
            msg.contains("0.19.26"),
            "message must name the shadowing version: {msg}"
        );
        assert!(
            msg.contains(&early_dir.join("tm").display().to_string()),
            "message must name the shadowing path: {msg}"
        );
        assert!(
            msg.contains(&install_path.display().to_string()),
            "message must name the install path: {msg}"
        );
    }

    /// #3554 review (HIGH): a shadowing binary that hangs on `--version`
    /// (here, a shell script that sleeps well past the health gate's 10s
    /// timeout) must NOT hang `detect` indefinitely — it must complete in
    /// bounded time, still reporting the SHADOW itself (the operator's shell
    /// still resolves to a different file — that fact doesn't depend on
    /// being able to read its version), with `shadowing_version: None`
    /// because the probe couldn't complete.
    ///
    /// Why this reproduces the reviewed risk: prior to routing this probe
    /// through `verify_installed_binary_at_path`'s timeout-guarded
    /// primitive, the un-timed-out `update_engine::installed_version` would
    /// block on `Command::output()` for as long as the child process runs —
    /// i.e. forever, for a shell shim blocked on stdin or any other hung
    /// `--version` invocation, on the very step meant to make failure loud
    /// right after the health gate passed.
    ///
    /// `start_paused = true` puts the TIMEOUTS on Tokio's virtual clock while
    /// the `sleep 30` child still runs on the real one — the child is really
    /// spawned and really never answers, so the runtime goes idle and Tokio
    /// auto-advances to the probe's 10s deadline at no wall-clock cost (was
    /// 10.0s). The `elapsed` assertion below is what keeps that honest: it
    /// proves `None` came from the probe timing out after a full 10s, not
    /// from an early spawn failure that never exercised the hang at all.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn detect_does_not_hang_on_an_unresponsive_shadowing_binary() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let early_dir = tmp.path().join("early-hanging-bin");
        std::fs::create_dir_all(&early_dir).expect("mkdir early");
        // Sleeps well past the 10s health-gate timeout, then would exit 0 —
        // the test must never wait for that exit.
        std::fs::write(
            early_dir.join("tm"),
            "#!/bin/sh\nsleep 30\necho 'trusty-mpm 9.9.9'\nexit 0\n",
        )
        .expect("write hanging fake binary");
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(early_dir.join("tm"))
                .expect("stat")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(early_dir.join("tm"), perms).expect("chmod");
        }

        let install_dir = tmp.path().join("local-bin");
        std::fs::create_dir_all(&install_dir).expect("mkdir install");
        let install_path = install_dir.join("tm");
        write_versioned_binary(&install_path, "trusty-mpm 0.19.29");

        let search_path = format!("{}:{}", early_dir.display(), install_dir.display());

        // Bound the WHOLE test well under what an indefinite hang would look
        // like (the pre-fix behaviour), but comfortably above the 10s
        // internal health-gate timeout `detect` now goes through.
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            detect(
                "tm",
                &install_path,
                Some("0.19.29"),
                OsStr::new(&search_path),
            ),
        )
        .await
        .expect("detect must return within 20s, never hang on an unresponsive shadowing binary");
        let elapsed = started.elapsed();

        let report = result.expect("the shadow itself must still be reported");
        assert_eq!(report.shadowing_path, early_dir.join("tm"));
        assert_eq!(
            report.shadowing_version, None,
            "an unprobeable (timed-out) shadowing binary must degrade to None, not block"
        );
        assert!(
            elapsed >= std::time::Duration::from_secs(10),
            "`shadowing_version: None` must come from the health gate's 10s probe timeout \
             expiring, but `detect` gave up after only {elapsed:?} — the probe never ran \
             against the hanging binary at all, so this test would no longer catch the \
             un-timed-out `installed_version` regression"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detect_returns_none_when_path_resolves_to_installed_binary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let install_dir = tmp.path().join("local-bin");
        std::fs::create_dir_all(&install_dir).expect("mkdir");
        let install_path = install_dir.join("tm");
        write_versioned_binary(&install_path, "trusty-mpm 0.19.29");

        let report = detect(
            "tm",
            &install_path,
            Some("0.19.29"),
            OsStr::new(&install_dir.display().to_string()),
        )
        .await;
        assert!(report.is_none(), "no shadow should be reported: {report:?}");
    }

    /// Why (#5805): the shadow check probed `StableMember::binary` only, so
    /// `tctl` — the name operators type, and the second binary trusty-installer
    /// places — was never checked. A stale `tctl` earlier on `$PATH` kept
    /// winning while the install reported clear.
    /// What: places both installer binaries, shadows ONLY `tctl` from an
    /// earlier directory, and asserts `detect_all` reports exactly that one
    /// while `detect` on the primary name alone finds nothing.
    /// Test: This is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn detect_all_reports_a_shadowed_alias_when_the_primary_is_clear() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let early_dir = tmp.path().join("early-bin");
        std::fs::create_dir_all(&early_dir).expect("mkdir early");
        write_versioned_binary(&early_dir.join("tctl"), "tctl 0.8.0");

        let install_dir = tmp.path().join("cargo-bin");
        std::fs::create_dir_all(&install_dir).expect("mkdir install");
        let primary = install_dir.join("trusty-installer");
        write_versioned_binary(&primary, "trusty-installer 0.9.0");
        write_versioned_binary(&install_dir.join("tctl"), "tctl 0.9.0");

        let search_path = format!("{}:{}", early_dir.display(), install_dir.display());
        let names = vec!["trusty-installer".to_owned(), "tctl".to_owned()];

        assert!(
            detect(
                "trusty-installer",
                &primary,
                Some("0.9.0"),
                OsStr::new(&search_path)
            )
            .await
            .is_none(),
            "the primary name is clear — checking it alone is what missed the shadow"
        );

        let reports = detect_all(&names, &primary, Some("0.9.0"), OsStr::new(&search_path)).await;
        assert_eq!(
            reports.len(),
            1,
            "exactly the shadowed alias must be reported: {reports:?}"
        );
        assert_eq!(reports[0].binary_name, "tctl");
        assert_eq!(reports[0].shadowing_path, early_dir.join("tctl"));
        assert_eq!(reports[0].shadowing_version.as_deref(), Some("0.8.0"));
    }

    /// Why (#5805): a name in the member's binary list that the tarball never
    /// shipped was never placed, so there is no installed file to compare
    /// against. Reporting it as "shadowed" would turn a missing binary into a
    /// false PATH warning naming a path that does not exist.
    /// What: lists an alias with no file on disk and asserts `detect_all`
    /// reports nothing for it, even with a same-named binary earlier on PATH.
    /// Test: This is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn detect_all_skips_a_binary_that_was_never_placed() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let early_dir = tmp.path().join("early-bin");
        std::fs::create_dir_all(&early_dir).expect("mkdir early");
        write_versioned_binary(&early_dir.join("tctl"), "tctl 0.8.0");

        let install_dir = tmp.path().join("cargo-bin");
        std::fs::create_dir_all(&install_dir).expect("mkdir install");
        let primary = install_dir.join("trusty-installer");
        write_versioned_binary(&primary, "trusty-installer 0.9.0");
        // No `tctl` in the install dir — it was listed but never placed.

        let search_path = format!("{}:{}", early_dir.display(), install_dir.display());
        let names = vec!["trusty-installer".to_owned(), "tctl".to_owned()];

        let reports = detect_all(&names, &primary, Some("0.9.0"), OsStr::new(&search_path)).await;
        assert!(
            reports.is_empty(),
            "an unplaced binary has nothing to be shadowed: {reports:?}"
        );
    }

    #[tokio::test]
    async fn detect_returns_none_when_not_on_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let install_path = tmp.path().join("local-bin").join("tm");

        // Empty search path — nothing resolves `tm` at all.
        let report = detect("tm", &install_path, Some("0.19.29"), OsStr::new("")).await;
        assert!(
            report.is_none(),
            "a binary absent from PATH entirely is not 'shadowed': {report:?}"
        );
    }
}
