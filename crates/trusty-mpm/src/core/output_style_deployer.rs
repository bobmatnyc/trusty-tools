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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
