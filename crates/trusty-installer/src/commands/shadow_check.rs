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
/// resolves), probing that file's `--version` best-effort. Returns `None`
/// for [`ShadowVerdict::Clear`] and [`ShadowVerdict::NotOnPath`] — the latter
/// is a distinct, lower-severity condition already surfaced by `install.sh`
/// (see [`ShadowVerdict::NotOnPath`]'s doc).
///
/// Test: `detect_reports_shadow_with_both_paths_and_versions`,
/// `detect_returns_none_when_path_resolves_to_installed_binary`,
/// `detect_returns_none_when_not_on_path`.
pub fn detect(
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
                super::update_engine::installed_version(&resolved.to_string_lossy());
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
    #[test]
    fn detect_reports_shadow_with_both_paths_and_versions() {
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

    #[cfg(unix)]
    #[test]
    fn detect_returns_none_when_path_resolves_to_installed_binary() {
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
        );
        assert!(report.is_none(), "no shadow should be reported: {report:?}");
    }

    #[test]
    fn detect_returns_none_when_not_on_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let install_path = tmp.path().join("local-bin").join("tm");

        // Empty search path — nothing resolves `tm` at all.
        let report = detect("tm", &install_path, Some("0.19.29"), OsStr::new(""));
        assert!(
            report.is_none(),
            "a binary absent from PATH entirely is not 'shadowed': {report:?}"
        );
    }
}
