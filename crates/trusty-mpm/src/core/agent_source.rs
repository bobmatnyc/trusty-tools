//! Self-healing materialization + auto-deploy of the bundled agent *source*
//! directory (`~/.trusty-mpm/framework/agents/`) — issue #4840.
//!
//! Why: `crates/trusty-mpm/src/assets/agents/*.md` is compiled into the binary
//! (`bundle::ALL`), but the ONLY code path that ever wrote it to disk was the
//! separate, manual `tm install`. Rebuilding the binary did not trigger it;
//! starting a session did not trigger it. So a merged, compiled-in instruction
//! change (e.g. a new `BASE-AGENT.md` rule) silently had no effect until
//! someone remembered to re-run `tm install` — measured on 2026-08-04 as a
//! deployed `BASE-AGENT.md` three days older than the binary containing the
//! fix, with no warning anywhere. Skills already solved exactly this
//! (`skill_source::ensure_skill_source_fresh`, issue #1917); agents were the
//! missing half.
//!
//! What: [`agent_bundle_stamp`] fingerprints the compiled-in `agents/*` slice
//! of [`bundle::ALL`] via sha256 — this is the cheap gate, so the common
//! "nothing changed" case costs one file read and one string compare.
//! [`materialize_agent_artifacts`] writes every bundled agent into the source
//! directory and prunes any `.md` file the current table no longer lists (a
//! renamed or removed agent). [`ensure_agent_source_fresh`] compares the stamp
//! against a marker file and re-materializes only on a mismatch.
//! [`autodeploy_agents`] is the entry point session provisioning calls: it
//! refreshes the source, deploys it, and returns a report — it NEVER returns
//! an error, because a broken deploy must not block a session (a stale file is
//! the strictly lesser failure). Anything it declined to overwrite is named in
//! [`AgentAutodeploy::warnings`], closing the other half of #4840: the
//! deployer's user-edited-file protection previously skipped silently.
//!
//! Overwrite policy (#4840, decided): a BUNDLED-origin file whose checksum
//! drifted IS overwritten — the deploy tracker (`.trusty-mpm-manifest.json`)
//! records `origin`, and `agents::deployer` already treats framework-owned
//! drift as corruption rather than user ownership (#4408). Auto-deploy
//! inherits that unchanged. Only an UNTRACKED file that differs, or a
//! user-owned (seed-once tier) entry that was edited, is preserved — and now
//! warned about by name.
//!
//! Test: `crates/trusty-mpm/src/core/agent_source_tests.rs`.

use std::collections::HashSet;
use std::path::Path;

use crate::core::agent_deployer::{DeployResult, deploy_agents};
use crate::core::agent_manifest::{atomic_write, checksum};
use crate::core::bundle;
use crate::core::error::Result;
use crate::core::paths::FrameworkPaths;

/// Marker file recording the last-materialized bundle stamp, written directly
/// under the agent source directory (`<agents_dir>/.bundle-stamp`).
///
/// Mirrors `skill_source::STAMP_FILE_NAME`; hidden (leading `.`) so the prune
/// sweep never considers it a stale agent.
const STAMP_FILE_NAME: &str = ".bundle-stamp";

/// Outcome of one [`autodeploy_agents`] run.
///
/// Why: the caller needs to know whether anything changed (to log it) and,
/// critically, WHICH files were left stale — #4840's second half is that a
/// declined overwrite was silent.
/// What: `refreshed` is `true` when the source directory was re-materialized
/// from the compiled-in bundle this run; `deployed` lists the composed agent
/// files actually (re)written into the target; `warnings` carries one
/// human-readable line per file left stale, plus one line if the refresh or
/// the deploy itself failed outright.
/// Test: every `autodeploy_*` case in `agent_source_tests.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentAutodeploy {
    /// Whether the source directory was re-materialized from the bundle.
    pub refreshed: bool,
    /// Composed agent filenames (re)written into the target this run.
    pub deployed: Vec<String>,
    /// One line per file left stale, or per step that failed. Always safe to
    /// print verbatim; empty means nothing was declined and nothing failed.
    pub warnings: Vec<String>,
}

/// Compute a stable sha256 fingerprint over every `agents/*` entry in the
/// compiled-in [`bundle::ALL`] table.
///
/// Why: this is the gate that makes auto-deploy cheap. It detects "this binary
/// embeds different agent content than what is on disk" without depending on
/// file mtimes, which a backup/restore (or a `cargo install` that preserves
/// timestamps) can shuffle.
/// What: concatenates `<rel_path>\0<contents>\n` for every table entry whose
/// `rel_path` starts with `"agents/"`, in table order, and returns the sha256
/// hex digest.
/// Test: `agent_bundle_stamp_is_stable_across_calls`,
/// `agent_bundle_stamp_differs_from_skill_stamp`.
pub fn agent_bundle_stamp() -> String {
    let mut buf = String::new();
    for artifact in bundle::ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("agents/"))
    {
        buf.push_str(artifact.rel_path);
        buf.push('\0');
        buf.push_str(artifact.contents);
        buf.push('\n');
    }
    checksum(&buf)
}

/// Write every bundled `agents/*` artifact into `agents_dir`, pruning any
/// `.md` file on disk the current table no longer lists.
///
/// Why: `agents_dir` (`~/.trusty-mpm/framework/agents/`) is a framework-owned
/// artifact directory — every entry is written exclusively by trusty-mpm's own
/// installer/self-heal path, never hand-edited (the USER-level agent source is
/// the separate `~/.trusty-mpm/agents/`). So it is safe, and necessary for the
/// renamed-agent case, to prune rather than only add.
/// What: creates `agents_dir` if absent, atomically (over)writes each
/// `agents/*` artifact to `<agents_dir>/<basename>`, then removes any
/// remaining top-level `*.md` file that is not one of the artifacts just
/// written. Hidden files (leading `.`, e.g. the stamp marker and the deploy
/// manifest) are never touched. Returns the basenames written.
/// Test: `materialize_agent_artifacts_writes_all_agents`,
/// `materialize_agent_artifacts_prunes_files_not_in_table`.
pub fn materialize_agent_artifacts(agents_dir: &Path) -> Result<Vec<String>> {
    std::fs::create_dir_all(agents_dir)?;

    let mut written = Vec::new();
    let mut keep: HashSet<String> = HashSet::new();
    for artifact in bundle::ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("agents/"))
    {
        let basename = artifact
            .rel_path
            .strip_prefix("agents/")
            .unwrap_or(artifact.rel_path);
        let dest = agents_dir.join(basename);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        atomic_write(&dest, artifact.contents)?;
        keep.insert(basename.to_string());
        written.push(basename.to_string());
    }

    for entry in std::fs::read_dir(agents_dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with('.') || !name.ends_with(".md") {
            continue;
        }
        if !keep.contains(name.as_str()) {
            std::fs::remove_file(entry.path())?;
        }
    }

    Ok(written)
}

/// Ensure `agents_dir` reflects the binary's currently-embedded agent bundle,
/// re-materializing it when missing or stale.
///
/// Why: the self-healing entry point behind [`autodeploy_agents`] — it removes
/// the dependency on a prior, separate `tm install` having refreshed the
/// source directory (#4840).
/// What: compares [`agent_bundle_stamp`] against `<agents_dir>/.bundle-stamp`;
/// when they differ (including "stamp file absent", which covers a missing or
/// never-materialized directory), calls [`materialize_agent_artifacts`] and
/// rewrites the stamp. Returns `true` when a refresh happened, `false` when
/// the source was already current.
/// Test: `ensure_agent_source_fresh_materializes_when_missing`,
/// `ensure_agent_source_fresh_is_noop_when_current`,
/// `ensure_agent_source_fresh_prunes_renamed_files`.
pub fn ensure_agent_source_fresh(agents_dir: &Path) -> Result<bool> {
    let stamp_path = agents_dir.join(STAMP_FILE_NAME);
    let current = agent_bundle_stamp();
    if std::fs::read_to_string(&stamp_path).ok().as_deref() == Some(current.as_str()) {
        return Ok(false);
    }

    materialize_agent_artifacts(agents_dir)?;
    atomic_write(&stamp_path, &current)?;
    Ok(true)
}

/// One warning line per file the deploy declined to overwrite.
///
/// Why: #4840's second half. `agents::deployer` preserves an untracked-and-
/// differing file, and a user-owned (seed-once tier) entry that was edited,
/// **silently** — which is precisely what let a three-day-stale `BASE-AGENT.md`
/// go unnoticed. Whatever auto-deploy declines to fix, it must name.
/// What: maps [`DeployResult::skipped`] to lines, distinguishing the
/// untracked-and-modified case (which `tm install --reset-agents` resolves)
/// from a tracked user-owned edit. Returns an empty vector when nothing was
/// skipped. Pure — no I/O, no logging.
/// Test: `skip_warning_lines_names_each_skipped_file`,
/// `skip_warning_lines_is_empty_when_nothing_skipped`.
pub fn skip_warning_lines(result: &DeployResult) -> Vec<String> {
    let untracked: HashSet<&str> = result
        .untracked_modified
        .iter()
        .map(String::as_str)
        .collect();
    result
        .skipped
        .iter()
        .map(|file| {
            if untracked.contains(file.as_str()) {
                format!(
                    "warning: agent {file} is stale — it is not tracked by the deploy manifest \
                     and differs from the bundled version, so it was NOT updated. \
                     Run `tm install --reset-agents {}` to adopt it.",
                    file.strip_suffix(".md").unwrap_or(file)
                )
            } else {
                format!(
                    "warning: agent {file} is stale — it is user-owned and edited, \
                     so it was NOT updated with the bundled version."
                )
            }
        })
        .collect()
}

/// Refresh the bundled agent source from the compiled-in bundle and deploy it,
/// reporting anything left stale. Never fails.
///
/// Why: this is what makes a merged `BASE-AGENT.md` change reach a running
/// agent without a manual `tm install` (#4840). It is called from session
/// provisioning, so it MUST fail open: if the refresh or the deploy cannot
/// run, the session still starts with whatever is already on disk. A stale
/// file is a strictly better failure than a session that will not launch.
/// What: (1) [`ensure_agent_source_fresh`] on `source_dir` — cheap when the
/// stamp matches; (2) [`deploy_agents`] from `source_dir` into `target_dir`,
/// which composes and writes only the files it safely may (bundled-origin
/// drift IS overwritten — see the module doc's overwrite policy); (3) collects
/// [`skip_warning_lines`] for everything it declined. Every failure is
/// converted into a warning line rather than an `Err`.
/// Test: `autodeploy_agents_deploys_when_bundle_differs`,
/// `autodeploy_agents_is_a_noop_when_already_current`,
/// `autodeploy_agents_fails_open_when_target_is_unwritable`,
/// `autodeploy_agents_warns_when_it_skips_a_user_modified_file`.
pub fn autodeploy_agents(source_dir: &Path, target_dir: &Path) -> AgentAutodeploy {
    let mut out = AgentAutodeploy::default();

    match ensure_agent_source_fresh(source_dir) {
        Ok(refreshed) => out.refreshed = refreshed,
        Err(err) => out.warnings.push(format!(
            "warning: could not refresh the bundled agent source at {} ({err}) — \
             continuing with whatever is already on disk",
            source_dir.display()
        )),
    }

    match deploy_agents(source_dir, target_dir) {
        Ok(result) => {
            out.warnings.extend(skip_warning_lines(&result));
            out.deployed = result.deployed;
        }
        Err(err) => out.warnings.push(format!(
            "warning: could not deploy agents into {} ({err}) — \
             this session runs with the previously deployed agents",
            target_dir.display()
        )),
    }

    out
}

/// [`autodeploy_agents`] against a resolved framework layout, with the
/// git-submodule guard applied.
///
/// Why: [`FrameworkPaths::agent_source_dir`] prefers the `agents/agents` git
/// submodule when a source checkout has one. That directory is git-tracked and
/// authoritative on its own — materializing the compiled-in bundle over it
/// would be destructive and wrong. Callers holding a `FrameworkPaths` should
/// use this instead of [`autodeploy_agents`] so they cannot get that wrong.
/// (Mirrors `skill_source::ensure_skill_source_fresh`'s identical guard.)
/// What: when the resolved source is the framework-owned
/// [`FrameworkPaths::agents`] directory, delegates to [`autodeploy_agents`];
/// when it is the submodule, deploys without refreshing the source. Never
/// fails, for the same fail-open reason.
/// Test: `autodeploy_agents_for_skips_source_refresh_on_submodule`.
pub fn autodeploy_agents_for(paths: &FrameworkPaths, target_dir: &Path) -> AgentAutodeploy {
    let source_dir = paths.agent_source_dir();
    if source_dir != paths.agents {
        let mut out = AgentAutodeploy::default();
        match deploy_agents(&source_dir, target_dir) {
            Ok(result) => {
                out.warnings.extend(skip_warning_lines(&result));
                out.deployed = result.deployed;
            }
            Err(err) => out.warnings.push(format!(
                "warning: could not deploy agents into {} ({err}) — \
                 this session runs with the previously deployed agents",
                target_dir.display()
            )),
        }
        return out;
    }
    autodeploy_agents(&source_dir, target_dir)
}

#[cfg(test)]
#[path = "agent_source_tests.rs"]
mod tests;
