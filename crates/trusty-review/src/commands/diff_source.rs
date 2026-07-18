//! Shared local-diff-mode resolution for the `run` and `compare` subcommands.
//!
//! Why: both subcommands accept three ways to pick a diff without touching
//! GitHub — a local file (`--local-diff <PATH>`), stdin (`--local-diff -`),
//! or an arbitrary git ref range (`--base <REF> [--head <REF>]`) — and both
//! must apply the identical selection rule.  Centralising it here (issue
//! #2993) avoids `resolve_diff_source_run` and `resolve_diff_source_compare`
//! drifting apart on which flag wins or how the git range defaults `head`.
//!
//! What: [`LocalDiffFlags`] bundles the raw flag values; [`resolve_local_diff_source`]
//! returns `Some(DiffSource)` for `LocalFile`/`Stdin`/`GitRange` when a local
//! mode flag was set, or `None` so the caller falls through to GitHub-PR
//! resolution.  `--base`/`--local-diff` mutual exclusion is enforced by clap
//! (`conflicts_with`) on `RunArgs`/`CompareArgs`, not here.
//!
//! Test: `resolve_local_diff_source_*` below.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use trusty_review::pipeline::DiffSource;

/// The local-diff-mode CLI flag values shared by `RunArgs` and `CompareArgs`.
///
/// Why: bundles the three flags so `resolve_local_diff_source` has one
/// signature usable from both subcommands' arg structs.
/// What: borrowed views over the parsed clap fields — no ownership transfer.
/// Test: constructed inline by every `resolve_local_diff_source_*` test.
pub struct LocalDiffFlags<'a> {
    /// `--local-diff <PATH>` value; `Some("-")` means stdin.
    pub local_diff: Option<&'a Path>,
    /// `--base <REF>` value (git-range mode).
    pub base: Option<&'a str>,
    /// `--head <REF>` value; `None` defaults to `HEAD` inside `DiffSource::GitRange`.
    pub head: Option<&'a str>,
}

/// Resolve `flags` into a `DiffSource`, or `None` when neither local mode was set.
///
/// Why: single funnel so `run` and `compare` apply the identical local-vs-GitHub
/// selection and the identical git-range defaulting (issue #2993).
/// What: `--local-diff -` → `DiffSource::Stdin`; any other `--local-diff <PATH>`
/// → `DiffSource::LocalFile`; `--base <REF>` (with clap enforcing it cannot
/// coexist with `--local-diff`) → `DiffSource::GitRange` rooted at the current
/// working directory, `head` passed through as-is (`None` → `HEAD` downstream).
/// Neither flag set → `Ok(None)`, telling the caller to resolve a GitHub PR
/// source instead.
/// Test: `resolve_local_diff_source_prefers_local_file`,
/// `resolve_local_diff_source_dash_is_stdin`,
/// `resolve_local_diff_source_base_builds_git_range`,
/// `resolve_local_diff_source_neither_flag_is_none`.
pub fn resolve_local_diff_source(flags: LocalDiffFlags<'_>) -> Result<Option<DiffSource>> {
    if let Some(path) = flags.local_diff {
        if path.as_os_str() == "-" {
            return Ok(Some(DiffSource::Stdin));
        }
        return Ok(Some(DiffSource::LocalFile {
            path: path.to_path_buf(),
        }));
    }

    if let Some(base) = flags.base {
        let repo_root: PathBuf = std::env::current_dir()
            .context("failed to determine current directory for --base git-range diff")?;
        return Ok(Some(DiffSource::GitRange {
            repo_root,
            base: base.to_string(),
            head: flags.head.map(str::to_string),
        }));
    }

    Ok(None)
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_local_diff_source_prefers_local_file() {
        let path = Path::new("/tmp/some.diff");
        let source = resolve_local_diff_source(LocalDiffFlags {
            local_diff: Some(path),
            base: None,
            head: None,
        })
        .expect("resolve")
        .expect("Some(source)");
        match source {
            DiffSource::LocalFile { path: p } => assert_eq!(p, path),
            other => panic!("expected LocalFile, got {other:?}"),
        }
    }

    #[test]
    fn resolve_local_diff_source_dash_is_stdin() {
        let source = resolve_local_diff_source(LocalDiffFlags {
            local_diff: Some(Path::new("-")),
            base: None,
            head: None,
        })
        .expect("resolve")
        .expect("Some(source)");
        assert!(
            matches!(source, DiffSource::Stdin),
            "expected Stdin, got {source:?}"
        );
    }

    #[test]
    fn resolve_local_diff_source_base_builds_git_range() {
        let source = resolve_local_diff_source(LocalDiffFlags {
            local_diff: None,
            base: Some("origin/main"),
            head: Some("feature-x"),
        })
        .expect("resolve")
        .expect("Some(source)");
        match source {
            DiffSource::GitRange {
                repo_root,
                base,
                head,
            } => {
                assert_eq!(base, "origin/main");
                assert_eq!(head.as_deref(), Some("feature-x"));
                assert!(
                    repo_root.is_absolute(),
                    "current_dir() must return an absolute path: {repo_root:?}"
                );
            }
            other => panic!("expected GitRange, got {other:?}"),
        }
    }

    #[test]
    fn resolve_local_diff_source_base_without_head_leaves_head_none() {
        let source = resolve_local_diff_source(LocalDiffFlags {
            local_diff: None,
            base: Some("main"),
            head: None,
        })
        .expect("resolve")
        .expect("Some(source)");
        match source {
            DiffSource::GitRange { head, .. } => {
                assert!(head.is_none(), "no --head must pass through as None");
            }
            other => panic!("expected GitRange, got {other:?}"),
        }
    }

    #[test]
    fn resolve_local_diff_source_neither_flag_is_none() {
        let result = resolve_local_diff_source(LocalDiffFlags {
            local_diff: None,
            base: None,
            head: None,
        })
        .expect("resolve");
        assert!(result.is_none(), "no local flags → caller resolves GitHub");
    }
}
