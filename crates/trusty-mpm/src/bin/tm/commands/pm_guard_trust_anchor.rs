//! Trust-anchor denylist for `tm hook --pm-guard` (issue #3981).
//!
//! Why: `pm_guard`'s own enforcement pivots on two env vars —
//! `TRUSTY_MPM_DISABLE_HOOKS` and `TRUSTY_MPM_PM_UNRESTRICTED` (see
//! `pm_guard.rs` Guards 2/3) — that Claude Code's `settings.json` can set via
//! its top-level `env` object, applied to every session and subprocess it
//! spawns (hooks included), live-reloaded mid-session. `.claude/settings.json`
//! has extension `json`, which is not in `pm_guard::SOURCE_CODE_EXTENSIONS`
//! and is not PM-orchestration state, so a direct `Write`/`Edit` to it used to
//! fall through `evaluate_edit_tool`'s "not source code -> allow" fallback
//! unconditionally and unbudgeted — letting the PM (or any Guard-1/Guard-4
//! exempt caller) self-exempt from ALL pm_guard enforcement in one ordinary
//! permitted write. This module is the fix: a structural denylist checked
//! BEFORE that fallback (from `pm_guard::evaluate_edit_tool`) and again,
//! unconditionally, ahead of the ordinary Bash classifier (from
//! `pm_guard_bash::evaluate_bash_command`) so a shell redirect/`tee`/`sed -i`
//! targeting the same file cannot slip through as a budget-eligible
//! [`crate::commands::pm_guard_bash::SHELL_EDIT_REASON`] instead of an
//! absolute deny.
//!
//! What: [`is_trust_anchor_path`] resolves a (possibly relative, `~`-prefixed,
//! or `..`-traversing) path against the session's cwd and the real
//! filesystem — lexically normalizing `.`/`..` components first, then
//! resolving symlinks in whatever prefix of the path already exists — and
//! matches it against two anchors: (1) any `.claude/settings.json` or
//! `.claude/settings.local.json`, matched *structurally* by "immediate parent
//! directory literally named `.claude`" so it fires for the project-local
//! file under ANY project root and for the real per-user `~/.claude/...`
//! alike, without needing to know which project directory is "the" one; and
//! (2) the managed-session global equivalent directly under
//! `$CLAUDE_CONFIG_DIR` when that env var is set — confirmed against
//! `core::standalone::settings_defaults::ensure_settings_defaults` that
//! `settings.json` sits directly at `<CLAUDE_CONFIG_DIR>/settings.json`, with
//! NO nested `.claude/` component, so anchor (1)'s structural match cannot
//! see it and it needs its own exact-path check.
//!
//! No existing helper in this codebase reads the *live* `CLAUDE_CONFIG_DIR`
//! env var with a real-home fallback — every other reference either SETS it
//! for a spawned `claude` subprocess (`runtime::claude_code`,
//! `core::standalone::run`) or resolves tm's own fixed managed default
//! (`core::trusty_tools_config::managed_claude_config_dir`, which is anchored
//! at `~/.trusty-tools/trusty-mpm/claude-config` regardless of what THIS
//! process's environment actually carries). `pm_guard` runs AS the hook
//! subprocess of the exact session it is protecting, so its own inherited
//! `CLAUDE_CONFIG_DIR` (or its absence) is the correct, and only correct,
//! signal for where that session's global settings file lives — reading it
//! directly here is not a reimplementation of either existing resolver, both
//! of which answer a different question.
//!
//! Test: `is_trust_anchor_path_*` below cover project-local, real-home, and
//! `CLAUDE_CONFIG_DIR` matches, `..`-traversal and symlink resolution, and the
//! non-match cases (source files, `settings.json` NOT under a `.claude` dir,
//! other `.claude/` siblings). Wired end to end via
//! `tests/tm_hook_pm_guard.rs::pm_guard_denies_settings_json_write_*`.

use std::path::{Path, PathBuf};

use crate::commands::pm_guard_bash::normalize_lexically as lexically_normalize;

/// Deny reason for a write to pm_guard's own trust anchor (issue #3981).
///
/// Why: kept as a `pub(crate)` constant (rather than inlined at each call
/// site) so `pm_guard::evaluate_edit_tool` and
/// `pm_guard_bash::evaluate_bash_command` return the IDENTICAL string —
/// `pm_guard`'s budget gate compares a returned reason by value to decide
/// eligibility, and this reason must never match [`crate::commands::pm_guard_bash::SHELL_EDIT_REASON`]
/// / the source-edit reason so it always falls through to the unconditional,
/// non-budget-eligible deny branch (a trust-anchor write must be an absolute
/// prohibition, never something a PM can exhaust its per-turn budget to get
/// away with once).
/// What: names what was blocked, why it matters (it can disable ALL pm_guard
/// enforcement), cites issue #3981, and tells the reader what to do instead.
/// Deliberately does NOT suggest "delegate via Task/Agent tool" — unlike
/// every other deny reason in this file, this one is intentionally pierced
/// through to a dispatched sub-agent too (see `pm_guard::pm_guard`'s
/// absolute pre-Guard-4 check), so delegating would just move the same
/// denial to the sub-agent's own call, not bypass it. The only real remedies
/// are the genuine HUMAN escape hatches: an operator editing the file
/// directly outside this guarded session, or setting
/// `TRUSTY_MPM_PM_UNRESTRICTED=1`/`TRUSTY_MPM_DISABLE_HOOKS=1` in their OWN
/// shell (Guards 2/3 run before this check and still bypass it — see
/// `pm_guard.rs`'s module doc).
pub(crate) const TRUST_ANCHOR_DENY_REASON: &str = "PM must not write pm_guard's own trust \
     anchor — `.claude/settings.json`, `.claude/settings.local.json`, or the global \
     CLAUDE_CONFIG_DIR equivalent (issue #3981). These files carry the `env` block Claude \
     Code applies to every session and subprocess, including hooks — a write here can set \
     TRUSTY_MPM_DISABLE_HOOKS or TRUSTY_MPM_PM_UNRESTRICTED and silently disable every \
     pm_guard rule. This is NOT delegable — a dispatched sub-agent is denied the same way. \
     Ask a human operator to edit the file directly outside this session, or have them set \
     TRUSTY_MPM_PM_UNRESTRICTED=1 in their own shell for this session.";

/// The two trust-anchor file basenames.
///
/// Why: Claude Code reads both `settings.json` (shared/checked-in) and
/// `settings.local.json` (personal/gitignored) as one merged config — either
/// carries the `env` block described above, so both are covered.
/// What: matched via [`is_trust_anchor_path`]'s [`Path::file_name`] check.
const TRUST_ANCHOR_FILENAMES: &[&str] = &["settings.json", "settings.local.json"];

/// Whether `path` resolves to pm_guard's own trust anchor.
///
/// Why: see the module doc — this is the single check shared by both write
/// routes (the Edit/Write tool path via `pm_guard::evaluate_edit_tool`, and
/// the Bash redirect/`tee`/`sed`-`awk`/`patch`/`git apply` path via
/// `pm_guard_bash`).
/// What: resolves `path` to an absolute, lexically-normalized, best-effort
/// symlink-resolved [`PathBuf`] via [`resolve_for_trust_anchor_check`], then
/// returns `true` when its file name is one of [`TRUST_ANCHOR_FILENAMES`] AND
/// either its immediate parent directory is literally named `.claude`
/// (project-local or real-home), or its parent equals the resolved
/// `$CLAUDE_CONFIG_DIR` (or, when unset, `~/.claude` — the same fallback
/// [`global_claude_config_dir`] uses, making this branch a harmless
/// duplicate of the first in the unset case rather than a gap). An empty or
/// unresolvable path returns `false` (fail-open, consistent with the rest of
/// this module).
/// Test: `is_trust_anchor_path_matches_project_local`,
/// `is_trust_anchor_path_matches_real_home`,
/// `is_trust_anchor_path_matches_claude_config_dir_env`,
/// `is_trust_anchor_path_resists_parent_traversal`,
/// `is_trust_anchor_path_resists_symlink_indirection`,
/// `is_trust_anchor_path_allows_unrelated_files`.
pub(crate) fn is_trust_anchor_path(path: &str) -> bool {
    if path.trim().is_empty() {
        return false;
    }
    let resolved = resolve_for_trust_anchor_check(path);
    let Some(file_name) = resolved.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !TRUST_ANCHOR_FILENAMES.contains(&file_name) {
        return false;
    }
    let Some(parent) = resolved.parent() else {
        return false;
    };

    if parent.file_name().and_then(|n| n.to_str()) == Some(".claude") {
        return true;
    }

    if let Some(config_dir) = global_claude_config_dir() {
        let resolved_config_dir = resolve_symlinks_best_effort(&lexically_normalize(&config_dir));
        if parent == resolved_config_dir {
            return true;
        }
    }

    false
}

/// The directory holding the global (non-project) `settings.json` for THIS
/// session.
///
/// Why: see the module doc's explanation of why this reads the live env var
/// directly rather than routing through either existing (differently-scoped)
/// resolver.
/// What: `$CLAUDE_CONFIG_DIR` when set to a non-empty value (managed sessions
/// relocate the entire Claude Code config home there — `settings.json` sits
/// directly inside it, no nested `.claude/`), otherwise `~/.claude` (Claude
/// Code's own documented default, matching every other `dirs::home_dir()`
/// fallback in this crate). `None` only when neither the env var nor the home
/// directory can be resolved.
/// Test: covered indirectly via `is_trust_anchor_path_matches_claude_config_dir_env`
/// (sets the env var) and the real-home cases (leaves it unset).
fn global_claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return Some(dir);
        }
    }
    dirs::home_dir().map(|h| h.join(".claude"))
}

/// Resolve a raw tool-input path to an absolute, traversal- and
/// symlink-resistant [`PathBuf`] for trust-anchor matching.
///
/// Why: the requirement is "match on the resolved path, not a raw string" —
/// a relative path, a `~`-prefixed path, or a path containing `..` components
/// must all resolve to the SAME [`PathBuf`] a literal absolute path to the
/// same file would, so `.claude/../.claude/settings.json` cannot slip past
/// [`is_trust_anchor_path`]'s structural check.
/// What: expands a leading `~` ([`expand_home`]), joins a relative path onto
/// [`std::env::current_dir`] (falling back to the expanded path itself if the
/// cwd cannot be read), lexically normalizes `.`/`..` components
/// ([`lexically_normalize`] — pure, no filesystem access, so it works even
/// when the target file does not exist yet), then resolves symlinks in
/// whatever prefix of the normalized path already exists on disk
/// ([`resolve_symlinks_best_effort`]).
/// Test: `is_trust_anchor_path_resists_parent_traversal`,
/// `is_trust_anchor_path_resists_symlink_indirection`.
fn resolve_for_trust_anchor_check(raw: &str) -> PathBuf {
    let expanded = expand_home(raw);
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&expanded))
            .unwrap_or(expanded)
    };
    resolve_symlinks_best_effort(&lexically_normalize(&absolute))
}

/// Expand a leading `~` (home-relative shell shorthand) to an absolute path.
///
/// Why: Bash commands and, less commonly, tool-input paths use `~/...` for
/// the user's home; leaving it unexpanded would make `~/.claude/settings.json`
/// resolve to a nonsense relative path under the cwd instead of the real
/// trust anchor.
/// What: `~` alone or `~/rest` is replaced with [`dirs::home_dir`] joined to
/// `rest`; any other leading path (including `~other-user`, which this module
/// does not attempt to resolve) passes through unchanged. When the home
/// directory cannot be resolved, the raw string is kept as-is rather than
/// silently dropping the `~` (fail-open, not fail-wrong).
/// Test: covered via `is_trust_anchor_path_matches_real_home`.
fn expand_home(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// Resolve symlinks in whatever prefix of `path` already exists, without
/// requiring the full path to exist.
///
/// Why: a symlinked ancestor directory (e.g. a `.claude` that is actually a
/// symlink to some other tree) must not let a write dodge the structural
/// `.claude`-parent check, but the FILE itself — `settings.json` — is
/// typically being created by the very call under evaluation, so plain
/// [`Path::canonicalize`] (which requires full existence) cannot be used
/// directly on the whole path.
/// What: tries [`Path::canonicalize`] on the whole path first (the common
/// case: the file already exists). On failure, walks up parents collecting
/// the non-existent tail components, canonicalizes the deepest EXISTING
/// ancestor (falling back to that ancestor unresolved if even it cannot be
/// canonicalized — e.g. a permissions error), then rejoins the tail
/// components in original order. Because the input has already been through
/// [`lexically_normalize`], every remaining component is a plain path
/// segment (no `.`/`..` left to confuse the walk).
/// Test: `is_trust_anchor_path_resists_symlink_indirection`.
fn resolve_symlinks_best_effort(path: &Path) -> PathBuf {
    if let Ok(canon) = path.canonicalize() {
        return canon;
    }
    let mut existing = path;
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            break;
        };
        tail.push(name);
        let Some(parent) = existing.parent() else {
            break;
        };
        existing = parent;
    }
    let mut base = existing
        .canonicalize()
        .unwrap_or_else(|_| existing.to_path_buf());
    for comp in tail.into_iter().rev() {
        base.push(comp);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_trust_anchor_path_matches_project_local() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("proj/.claude")).unwrap();
        let target = tmp
            .path()
            .join("proj/.claude/settings.json")
            .to_string_lossy()
            .to_string();
        assert!(is_trust_anchor_path(&target));

        let local = tmp
            .path()
            .join("proj/.claude/settings.local.json")
            .to_string_lossy()
            .to_string();
        assert!(is_trust_anchor_path(&local));
    }

    #[test]
    fn is_trust_anchor_path_matches_relative_path_against_cwd() {
        // Deliberately does NOT mutate the process cwd (`set_current_dir` is
        // global, process-wide state that would race every other test in
        // this binary) — instead builds the expected absolute path directly
        // and hands the RELATIVE form to `is_trust_anchor_path`, confirming
        // it resolves against whatever the real cwd already is.
        let real_cwd = std::env::current_dir().unwrap();
        let unique = format!(
            "trust-anchor-relpath-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let claude_dir = real_cwd.join(&unique).join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();

        let relative = format!("{unique}/.claude/settings.json");
        let result = is_trust_anchor_path(&relative);
        std::fs::remove_dir_all(real_cwd.join(&unique)).ok();
        assert!(result, "a relative path must resolve against the real cwd");
    }

    #[test]
    fn is_trust_anchor_path_resists_parent_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("proj/.claude")).unwrap();
        // `.claude/../.claude/settings.json` must lexically collapse to the
        // same trust-anchor path.
        let target = tmp
            .path()
            .join("proj/.claude/../.claude/settings.json")
            .to_string_lossy()
            .to_string();
        assert!(is_trust_anchor_path(&target));

        // A traversal that lands somewhere else must NOT be caught.
        let elsewhere = tmp
            .path()
            .join("proj/.claude/../not-claude/settings.json")
            .to_string_lossy()
            .to_string();
        assert!(!is_trust_anchor_path(&elsewhere));
    }

    #[test]
    #[cfg(unix)]
    fn is_trust_anchor_path_resists_symlink_indirection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real_claude = tmp.path().join("real/.claude");
        std::fs::create_dir_all(&real_claude).unwrap();
        let link = tmp.path().join("link_dir");
        std::os::unix::fs::symlink(&real_claude, &link).unwrap();

        // The target file does not exist yet (this is a Write call), but the
        // symlinked ancestor does — resolution must still land on `.claude`.
        let target = link.join("settings.json").to_string_lossy().to_string();
        assert!(
            is_trust_anchor_path(&target),
            "a symlink to a .claude dir must not bypass the check"
        );
    }

    #[test]
    #[serial_test::serial(claude_config_dir_env)]
    fn is_trust_anchor_path_matches_claude_config_dir_env() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_dir = tmp.path().join("managed-claude-config");
        std::fs::create_dir_all(&config_dir).unwrap();

        // Serialized (see the attribute above) against every other test in
        // this binary that might observe `CLAUDE_CONFIG_DIR` mid-mutation —
        // `std::env::set_var`/`remove_var` are documented as unsound to race
        // across threads, matching the established convention elsewhere in
        // this crate (e.g. `core::paths`'s `HOME`-mutating tests).
        temp_env_var("CLAUDE_CONFIG_DIR", Some(&config_dir), || {
            let target = config_dir
                .join("settings.json")
                .to_string_lossy()
                .to_string();
            assert!(is_trust_anchor_path(&target));
            let local = config_dir
                .join("settings.local.json")
                .to_string_lossy()
                .to_string();
            assert!(is_trust_anchor_path(&local));
        });
    }

    #[test]
    fn is_trust_anchor_path_allows_unrelated_files() {
        for p in [
            "/x/a.rs",
            "/x/.claude/agents/foo.md",
            "/x/.claude/config.toml",
            "notes.md",
            "settings.json",
            "/x/settings.json",
        ] {
            assert!(
                !is_trust_anchor_path(p),
                "{p} must NOT be classified as the trust anchor"
            );
        }
    }

    #[test]
    fn is_trust_anchor_path_allows_empty() {
        assert!(!is_trust_anchor_path(""));
        assert!(!is_trust_anchor_path("   "));
    }

    #[test]
    fn lexically_normalize_collapses_parent_dirs() {
        // Regression coverage for THIS module's dependency on
        // `pm_guard_bash::normalize_lexically` (imported as
        // `lexically_normalize` above) — `pm_guard_bash` has its own tests
        // for the worktree-guard use case; this pins the contract
        // `resolve_for_trust_anchor_check` relies on.
        assert_eq!(
            lexically_normalize(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            lexically_normalize(Path::new("/a/./b/./c")),
            PathBuf::from("/a/b/c")
        );
        // Cannot traverse above root.
        assert_eq!(lexically_normalize(Path::new("/../a")), PathBuf::from("/a"));
    }

    #[test]
    fn expand_home_replaces_leading_tilde() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_home("~"), home);
            assert_eq!(
                expand_home("~/.claude/settings.json"),
                home.join(".claude/settings.json")
            );
        }
        assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_home("relative/path"), PathBuf::from("relative/path"));
    }

    /// Run `f` with `key` set to `value`'s path for the duration, restoring
    /// (or removing) the prior value afterward — a minimal scoped env-var
    /// helper so `is_trust_anchor_path_matches_claude_config_dir_env` cannot
    /// leak `CLAUDE_CONFIG_DIR` into other tests in this binary.
    fn temp_env_var(key: &str, value: Option<&Path>, f: impl FnOnce()) {
        let prior = std::env::var_os(key);
        match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        f();
        match prior {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}
