//! Output-style filesystem deployer for the managed CLAUDE_CONFIG_DIR (WI-2 follow-up).
//!
//! Why: the managed config dir (`<managed_root>/claude-config/`) starts empty.
//! Without deploying the bundled output-style definitions into it, Claude Code
//! sessions launched via `tm run`/`tm load`/`tm login` cannot resolve the
//! `"outputStyle": "trusty-mpm"` key in `settings.json` — Claude Code only
//! honours that key if a matching file exists under the config dir's
//! `output-styles/` subdirectory (refs #1553, epic #1548 WI-2 follow-up).
//! What: [`deploy_output_styles`] writes every style in [`OUTPUT_STYLES`] to
//! `<claude_config_dir>/output-styles/<file_name>`, idempotently — it only
//! writes when content differs from the on-disk copy.  Source is the
//! compile-time–bundled constants in [`crate::core::bundle`] so no
//! framework-root installation is required before the deploy runs (unlike the
//! agent/skill deployers which read from `<managed_root>/framework/`).
//! Test: inline `#[cfg(test)]` block; isolation invariant in
//! `tests/standalone_isolation.rs`.

use std::path::Path;

use crate::core::agent_manifest::atomic_write;
use crate::core::bundle::OUTPUT_STYLES;

/// Summary of one [`deploy_output_styles`] run.
///
/// Why: callers (and tests) need to know which styles were freshly written vs.
/// left untouched, so they can assert the deployer behaved correctly without
/// inspecting the filesystem directly.
/// What: counts split into freshly written and unchanged (checksum already
/// matched).  There is no "skipped" category — output styles are always
/// framework-owned, so the deployer unconditionally overwrites stale copies.
/// Test: every test in this module asserts on these fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputStyleDeployResult {
    /// File names successfully (re)written this run.
    pub deployed: Vec<String>,
    /// File names left untouched because their checksum already matched.
    pub unchanged: Vec<String>,
}

/// Deploy all bundled output styles into `<claude_config_dir>/output-styles/`.
///
/// Why: `tm run`/`tm load`/`tm login` sessions set `"outputStyle": "trusty-mpm"`
/// in `settings.json`, but Claude Code only resolves that name against style files
/// present under the active `CLAUDE_CONFIG_DIR/output-styles/`.  Deploying the
/// bundled styles on every managed-driver bootstrap ensures the setting is always
/// honoured, without requiring the user to first run `tm install` (closes #1553).
/// What: creates `<claude_config_dir>/output-styles/` if absent, then iterates
/// [`OUTPUT_STYLES`], comparing each style's content to the on-disk copy via a
/// byte-exact comparison (reads raw bytes so the check is valid even for
/// non-UTF-8 on-disk content, and surfaces IO errors rather than swallowing them).
/// Files whose bytes already match the bundled content are skipped (idempotent);
/// others are written atomically (temp-then-rename) via [`atomic_write`].  All
/// logging goes to stderr.
/// Test: `deploy_output_styles_populates_output_styles_dir` (happy path),
/// `deploy_output_styles_idempotent` (no spurious write on second call),
/// `deploy_output_styles_refreshes_stale_file` (overwrite when source changed).
pub fn deploy_output_styles(claude_config_dir: &Path) -> anyhow::Result<OutputStyleDeployResult> {
    let styles_dir = claude_config_dir.join("output-styles");
    std::fs::create_dir_all(&styles_dir)?;

    let mut result = OutputStyleDeployResult::default();

    for style in OUTPUT_STYLES {
        let target = styles_dir.join(style.file_name);
        let bundled_bytes = style.content.as_bytes();

        // Idempotency guard: read the on-disk bytes and compare directly.
        // Using `read` (raw bytes) rather than `read_to_string` avoids two
        // hazards: (a) silently treating non-UTF-8 on-disk content as absent
        // (which would trigger a spurious rewrite), and (b) swallowing genuine
        // IO errors (e.g. permissions) via `.ok()`.  Only `NotFound` (the file
        // genuinely does not exist yet) is treated as "needs write"; all other
        // IO errors are propagated so the caller sees them.
        let needs_write = match std::fs::read(&target) {
            Ok(disk_bytes) => disk_bytes != bundled_bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "failed to read on-disk style '{}' for idempotency check: {e}",
                    target.display()
                ));
            }
        };

        if !needs_write {
            result.unchanged.push(style.file_name.to_string());
            continue;
        }

        // Write atomically (temp-then-rename) so a crash between writes cannot
        // leave a half-written style file.
        atomic_write(&target, style.content).map_err(|e| {
            anyhow::anyhow!(
                "failed to deploy output style '{}' to {}: {e}",
                style.id,
                target.display()
            )
        })?;
        result.deployed.push(style.file_name.to_string());
    }

    Ok(result)
}

/// How one bundled output style compares to its deployed copy (#5866).
///
/// Why: the doctor report and the `tm doctor --fix` repair must agree
/// file-for-file about what is stale, or the preview names a file the repair
/// leaves alone. Two independent byte-comparisons is how that drifts, so both
/// consume [`output_style_drift`] and neither re-derives the predicate.
/// What: the three states that matter to a caller. `Unreadable` is deliberately
/// distinct from `Drifted` — a permissions failure is not evidence of staleness,
/// and the repair must not overwrite on it.
/// Test: `drift_reports_missing_drifted_and_unreadable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StyleDrift {
    /// The deployed file's bytes differ from the bundled content.
    Drifted,
    /// No file is deployed at this path.
    Missing,
    /// The file exists but could not be read; the string says why.
    Unreadable(String),
}

/// Compare every bundled style against `styles_dir`, reporting only the
/// non-matching ones (#5866).
///
/// Why: see [`StyleDrift`]. `check_output_style_staleness` reports drift and
/// `core::doctor_repair::repair_output_style` fixes it; sharing this scan is
/// what lets the remedy string name a command that provably acts on exactly the
/// files the report listed.
/// What: for each [`OUTPUT_STYLES`] entry, byte-compares
/// `<styles_dir>/<file_name>` against the bundled `content`. Returns
/// `(file_name, StyleDrift)` for every entry that is not byte-identical, in
/// [`OUTPUT_STYLES`] order; an in-sync style produces no entry. Reads only —
/// it never creates `styles_dir`.
/// Test: `drift_is_empty_when_in_sync`, `drift_reports_missing_drifted_and_unreadable`.
pub fn output_style_drift(styles_dir: &Path) -> Vec<(&'static str, StyleDrift)> {
    OUTPUT_STYLES
        .iter()
        .filter_map(|style| {
            let state = match std::fs::read(styles_dir.join(style.file_name)) {
                Ok(bytes) if bytes == style.content.as_bytes() => return None,
                Ok(_) => StyleDrift::Drifted,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => StyleDrift::Missing,
                Err(e) => StyleDrift::Unreadable(e.to_string()),
            };
            Some((style.file_name, state))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn drift_is_empty_when_in_sync() {
        let tmp = TempDir::new().unwrap();
        deploy_output_styles(tmp.path()).unwrap();
        assert!(
            output_style_drift(&tmp.path().join("output-styles")).is_empty(),
            "a freshly deployed tier must report no drift"
        );
    }

    #[test]
    fn drift_reports_missing_drifted_and_unreadable() {
        // #5866: the repair must be able to tell a stale file from an absent one
        // — it writes both — and must never treat an unreadable file as stale.
        let tmp = TempDir::new().unwrap();
        let styles = tmp.path().join("output-styles");
        std::fs::create_dir_all(&styles).unwrap();

        // Nothing deployed at all: every style is Missing.
        let all_missing = output_style_drift(&styles);
        assert_eq!(all_missing.len(), OUTPUT_STYLES.len());
        assert!(
            all_missing
                .iter()
                .all(|(_, state)| *state == StyleDrift::Missing)
        );

        deploy_output_styles(tmp.path()).unwrap();
        let first = &OUTPUT_STYLES[0];
        std::fs::write(styles.join(first.file_name), "stale text").unwrap();

        let drift = output_style_drift(&styles);
        assert_eq!(
            drift,
            vec![(first.file_name, StyleDrift::Drifted)],
            "only the rewritten file may be reported"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_style_is_not_reported_as_drifted() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        deploy_output_styles(tmp.path()).unwrap();
        let styles = tmp.path().join("output-styles");
        let target = styles.join(OUTPUT_STYLES[0].file_name);
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&target).is_ok() {
            eprintln!("skipping: cannot deny read on this platform/privilege level");
            return;
        }

        let drift = output_style_drift(&styles);
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));

        assert_eq!(drift.len(), 1);
        assert!(
            matches!(drift[0].1, StyleDrift::Unreadable(_)),
            "an IO failure is not evidence of staleness: {drift:?}"
        );
    }

    /// Deploy to a fresh directory must write all three bundled styles.
    ///
    /// Why: confirms the happy-path deploy populates the output-styles dir.
    /// What: calls deploy_output_styles on an empty temp dir and asserts every
    /// bundled style is written with the correct content.
    /// Test: this function IS the test.
    #[test]
    fn deploy_output_styles_populates_output_styles_dir() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        let result = deploy_output_styles(&cfg).unwrap();

        // All three bundled styles must be deployed on a first run.
        assert_eq!(
            result.deployed.len(),
            OUTPUT_STYLES.len(),
            "all bundled styles must be deployed on first run; got {:?}",
            result.deployed
        );
        assert!(
            result.unchanged.is_empty(),
            "no file should be unchanged on first deploy"
        );

        // Every style file must exist and contain the bundled content.
        let styles_dir = cfg.join("output-styles");
        for style in OUTPUT_STYLES {
            let target = styles_dir.join(style.file_name);
            assert!(
                target.exists(),
                "output-styles/{} must exist after deploy",
                style.file_name
            );
            let content = std::fs::read_to_string(&target).unwrap();
            assert_eq!(
                content, style.content,
                "deployed content of {} must match the bundled constant",
                style.file_name
            );
        }
    }

    /// A second deploy with no source changes must not rewrite any file.
    ///
    /// Why: idempotency is the key safety property — avoids spurious mtime
    /// bumps and thrashing on every managed-session bootstrap.
    /// What: calls deploy_output_styles twice and asserts the second call
    /// returns `deployed.is_empty()` and `unchanged.len() == OUTPUT_STYLES.len()`.
    /// Relies on the `OutputStyleDeployResult` fields rather than mtime
    /// comparisons (mtime assertions are racy on coarse-grained CI filesystems).
    /// Test: this function IS the test.
    #[test]
    fn deploy_output_styles_idempotent() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        // First call populates the directory.
        deploy_output_styles(&cfg).unwrap();

        // Second call must leave files untouched — assert via result fields,
        // not mtime (mtime checks are racy on coarse-grained filesystems).
        let result = deploy_output_styles(&cfg).unwrap();

        assert!(
            result.deployed.is_empty(),
            "second deploy must not overwrite any file; deployed: {:?}",
            result.deployed
        );
        assert_eq!(
            result.unchanged.len(),
            OUTPUT_STYLES.len(),
            "all files must be reported unchanged on idempotent call; \
             got unchanged={:?} deployed={:?}",
            result.unchanged,
            result.deployed
        );
    }

    /// A stale on-disk copy (content differs from bundled) must be overwritten.
    ///
    /// Why: when the framework upgrades a style, the deployer must replace the
    /// stale on-disk copy, not leave it in place.
    /// What: writes deliberately stale content to the first style's target path,
    /// calls deploy_output_styles, and asserts the result reports the file as
    /// deployed and the on-disk content now matches the bundle.
    /// Test: this function IS the test.
    #[test]
    fn deploy_output_styles_refreshes_stale_file() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        let styles_dir = cfg.join("output-styles");
        std::fs::create_dir_all(&styles_dir).unwrap();

        // Write a stale copy of the first style.
        let first = &OUTPUT_STYLES[0];
        let target = styles_dir.join(first.file_name);
        std::fs::write(&target, "stale content that does not match the bundle").unwrap();

        let result = deploy_output_styles(&cfg).unwrap();

        // The stale file must be refreshed (deployed).
        assert!(
            result.deployed.contains(&first.file_name.to_string()),
            "stale style must be refreshed; deployed: {:?}",
            result.deployed
        );

        // Content must now match the bundle.
        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(
            content, first.content,
            "refreshed file content must match bundled constant"
        );
    }

    /// Non-UTF-8 bytes on disk must trigger a rewrite, not a spurious OK.
    ///
    /// Why: the idempotency guard reads raw bytes (not UTF-8 string) so that
    /// a corrupt or binary on-disk file is correctly detected as "stale" and
    /// replaced, rather than silently swallowed.
    /// What: writes raw non-UTF-8 bytes to the first style's path, calls
    /// deploy_output_styles, and asserts the file is reported as deployed.
    /// Test: this function IS the test.
    #[test]
    fn deploy_output_styles_handles_non_utf8_on_disk() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        let styles_dir = cfg.join("output-styles");
        std::fs::create_dir_all(&styles_dir).unwrap();

        // Write non-UTF-8 bytes (invalid in UTF-8) to the first style path.
        let first = &OUTPUT_STYLES[0];
        let target = styles_dir.join(first.file_name);
        std::fs::write(&target, b"\xff\xfe invalid utf-8 bytes \x80\x81").unwrap();

        let result = deploy_output_styles(&cfg).unwrap();

        // The file with non-UTF-8 content must be treated as stale and refreshed.
        assert!(
            result.deployed.contains(&first.file_name.to_string()),
            "non-UTF-8 on-disk file must be refreshed; deployed: {:?}",
            result.deployed
        );

        // After refresh, content must match the bundled constant.
        let refreshed = std::fs::read_to_string(&target).unwrap();
        assert_eq!(
            refreshed, first.content,
            "after refresh, content must match bundled constant"
        );
    }
}
