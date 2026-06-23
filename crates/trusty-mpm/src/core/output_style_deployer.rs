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

use crate::core::agent_manifest::{atomic_write, checksum};
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
/// sha256 checksum.  Files whose checksum already matches are skipped (idempotent);
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

        // Idempotency: skip write when the on-disk content is already current.
        let on_disk_matches = std::fs::read_to_string(&target)
            .ok()
            .map(|existing| checksum(&existing) == checksum(style.content))
            .unwrap_or(false);

        if on_disk_matches {
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
    #[test]
    fn deploy_output_styles_idempotent() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        // First call populates the directory.
        deploy_output_styles(&cfg).unwrap();

        // Record mtimes before the second call.
        let styles_dir = cfg.join("output-styles");
        let mtimes_before: Vec<_> = OUTPUT_STYLES
            .iter()
            .map(|s| {
                std::fs::metadata(styles_dir.join(s.file_name))
                    .unwrap()
                    .modified()
                    .unwrap()
            })
            .collect();

        // Second call must leave files untouched.
        let result = deploy_output_styles(&cfg).unwrap();

        assert!(
            result.deployed.is_empty(),
            "second deploy must not overwrite any file; deployed: {:?}",
            result.deployed
        );
        assert_eq!(
            result.unchanged.len(),
            OUTPUT_STYLES.len(),
            "all files must be reported unchanged on idempotent call"
        );

        // mtimes must not have changed.
        for (style, mtime_before) in OUTPUT_STYLES.iter().zip(mtimes_before.iter()) {
            let mtime_after = std::fs::metadata(styles_dir.join(style.file_name))
                .unwrap()
                .modified()
                .unwrap();
            assert_eq!(
                mtime_before, &mtime_after,
                "unchanged style {} must not be rewritten (mtime changed)",
                style.file_name
            );
        }
    }

    /// A stale on-disk copy (content differs from bundled) must be overwritten.
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

    /// deploy_output_styles must NOT write outside the given claude_config_dir.
    ///
    /// Why: isolation invariant (SPEC-STANDALONE-MPM-04) — the managed driver
    /// must never write to the real `~/.claude*`.  This test points HOME at a
    /// fresh temp dir and asserts nothing lands there.
    /// What: redirects HOME, calls deploy_output_styles, asserts no
    /// `.claude` dir or `.claude.json` file appears in the fake home.
    /// Test: this function IS the test.
    #[serial_test::serial]
    #[test]
    fn deploy_output_styles_does_not_write_to_home() {
        struct HomeGuard(Option<String>);
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                match self.0 {
                    Some(ref p) => unsafe { std::env::set_var("HOME", p) },
                    None => unsafe { std::env::remove_var("HOME") },
                }
            }
        }

        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();

        let fake_home = TempDir::new().unwrap();
        let _home_guard = {
            let prev = std::env::var("HOME").ok();
            unsafe { std::env::set_var("HOME", fake_home.path()) };
            HomeGuard(prev)
        };

        deploy_output_styles(&cfg).unwrap();

        // styles must land inside cfg, not in fake_home.
        assert!(
            cfg.join("output-styles").exists(),
            "output-styles dir must exist inside the given config dir"
        );
        assert!(
            !fake_home.path().join(".claude").exists(),
            "deploy_output_styles must NOT write to $HOME/.claude (isolation)"
        );
        assert!(
            !fake_home.path().join("output-styles").exists(),
            "deploy_output_styles must NOT write output-styles to $HOME (isolation)"
        );
    }
}
