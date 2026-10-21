//! The Write/Edit protected-path check of `tm hook --pm-guard` (#8928 step 7).
//!
//! Why: under the Claude Code sandbox, Bash cannot write Claude Code settings
//! files, `.mcp.json`, git config or git hooks — they are sandbox-protected
//! paths. The Write and Edit tools are not sandboxed, so a PM or agent could
//! still use them to turn the sandbox off in `.claude/settings.local.json`, set
//! a pm-guard bypass in a settings `env` block, or plant a hook that runs
//! outside the sandbox. #8928's owner scope ruling (2026-09-30): the check
//! covers exact settings and config files plus frontmatter `hooks:` in skill
//! and agent Markdown; there is no blanket `.claude/**` deny.
//! What: [`evaluate`] judges one edit-tool call ([`EDIT_TOOLS`]). It denies a
//! target that is, as spelled or after symlinks: a `.claude/settings.json` or
//! `.claude/settings.local.json`; a `.mcp.json`; a git `config` or
//! `config.worktree` under a `.git` directory, or anything under a `.git`
//! `hooks` directory; the `settings.json`, `settings.local.json` or
//! `.claude.json` of a user config directory (`CLAUDE_CONFIG_DIR`,
//! `~/.claude`, tm's managed `claude-config`) or `~/.claude.json`; anything
//! under a user config directory's `plugins/` (plugin hook manifests and the
//! scripts they run); or a path a hook or `statusLine` command runs — in the
//! settings files or in skill and agent frontmatter `hooks:` — resolved
//! against `PATH` and the project root (`pm_guard_protected_hooks.rs`).
//! Names compare with Unicode case folding, as APFS does. A skill or agent
//! Markdown file is denied when the write leaves its frontmatter with a
//! `hooks` key it did not have, or a changed one
//! (`pm_guard_protected_hooks.rs`). It holds for every caller but the
//! Architect's main thread, which the call site exempts: the Architect runs
//! unsandboxed (#8928 step 5, `enabled: false`).
//! FAIL-CLOSED: a call naming no target, a relative target with no `cwd`, a
//! target that does not resolve, a settings file that cannot be read or parsed
//! (so the hook-named paths are unknown), and a Markdown write whose result
//! cannot be reproduced each deny.
//! Residual: Bash writes are the sandbox's job, not this rule's; a general
//! write boundary (Write/Edit only inside the sandbox's write roots) is not
//! part of step 7 as #8928 words it.
//! Test: `pm_guard_protected_paths_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_protected_paths_8928.rs`.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::trusty_tools_config::managed_claude_config_dir_at;

use crate::commands::pm_guard::{EDIT_TOOLS, edit_tool_target_path};
use crate::commands::pm_guard_protected_hooks::{hook_named_paths, markdown_hook_verdict};
use crate::commands::pm_guard_trust_anchor_paths::{
    Resolved, eq_fold, resolve, same_ci, starts_with_ci,
};

/// The rule name recorded with each deny.
pub(crate) const PROTECTED_PATH_RULE: &str = "protected-path-write";

/// The project-tier settings names guarded under a `.claude` directory.
const SETTINGS_NAMES: [&str; 2] = ["settings.json", "settings.local.json"];

/// The user-tier config names guarded in each user config directory.
const USER_CONFIG_NAMES: [&str; 3] = ["settings.json", "settings.local.json", ".claude.json"];

/// The hook's ambient inputs, injectable so every arm is testable.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProtectedEnv {
    /// The home directory, when known and absolute.
    pub(crate) home: Option<PathBuf>,
    /// `CLAUDE_PROJECT_DIR`, the session's launch directory.
    pub(crate) project_dir: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR`, when set and absolute.
    pub(crate) claude_config_dir: Option<PathBuf>,
    /// `PATH`, which resolves a bare hook program (#8928 fix round).
    pub(crate) path: Option<std::ffi::OsString>,
}

impl ProtectedEnv {
    /// This process's inputs.
    pub(crate) fn ambient() -> Self {
        let abs = |key: &str| {
            std::env::var_os(key)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        Self {
            home: dirs::home_dir().filter(|home| home.is_absolute()),
            project_dir: abs("CLAUDE_PROJECT_DIR"),
            claude_config_dir: abs("CLAUDE_CONFIG_DIR"),
            path: std::env::var_os("PATH"),
        }
    }

    /// The user config directories: `CLAUDE_CONFIG_DIR`, `~/.claude` and
    /// tm's managed `claude-config`.
    pub(crate) fn user_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self.claude_config_dir.iter().cloned().collect();
        if let Some(home) = &self.home {
            dirs.push(home.join(".claude"));
            dirs.push(managed_claude_config_dir_at(home));
        }
        dirs
    }

    /// The project roots whose `.claude/` settings name hooks: the launch
    /// directory and the call's `cwd`.
    pub(crate) fn project_roots(&self, cwd: Option<&Path>) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = self.project_dir.iter().cloned().collect();
        if let Some(cwd) = cwd
            && !roots.iter().any(|r| same_ci(r, cwd))
        {
            roots.push(cwd.to_path_buf());
        }
        roots
    }
}

/// The verdict for one `PreToolUse` call: `Some(reason)` denies.
///
/// Why/What: see the module doc. `cwd` is the payload's `cwd`; an empty path
/// counts as none.
/// Test: `each_protected_file_class_is_denied`,
/// `a_symlink_onto_a_protected_file_is_denied`,
/// `an_ordinary_write_is_allowed`, `every_fail_closed_arm_denies`.
pub(crate) fn evaluate(
    tool_name: &str,
    tool_input: Option<&Value>,
    cwd: Option<&Path>,
    env: &ProtectedEnv,
) -> Option<String> {
    if !EDIT_TOOLS.contains(&tool_name) {
        return None;
    }
    let cwd = cwd.filter(|c| !c.as_os_str().is_empty());
    // #8928: a call naming no target cannot be placed, so it is refused.
    let Some(spelled) = edit_tool_target_path(tool_input).filter(|p| !p.is_empty()) else {
        return Some(deny("(no target path)", "a call naming no target path"));
    };
    let lexical = if Path::new(spelled).is_absolute() {
        normalize(Path::new(spelled))
    } else if let Some(cwd) = cwd {
        normalize(&cwd.join(spelled))
    } else {
        return Some(deny(spelled, "a relative path with no working directory"));
    };
    let Resolved::Path(resolved) = resolve(&lexical) else {
        return Some(deny(
            spelled,
            "a path that does not resolve (a symlink loop or an unreadable directory)",
        ));
    };
    let user_dirs = env.user_dirs();
    for path in [&lexical, &resolved] {
        if let Some(class) = protected_class(path, &user_dirs, env.home.as_deref()) {
            return Some(deny(spelled, class));
        }
    }
    let named = match hook_named_paths(env, cwd) {
        Ok(named) => named,
        Err(file) => {
            return Some(deny(
                spelled,
                &format!(
                    "unknown to be safe, because `{}` cannot be read or parsed, so the paths its \
                     hooks name are unknown",
                    file.display()
                ),
            ));
        }
    };
    if named
        .iter()
        .any(|p| same_ci(p, &resolved) || same_ci(p, &lexical))
    {
        return Some(deny(spelled, "a path a hook or `statusLine` command runs"));
    }
    if is_hookable_markdown(&lexical, &user_dirs) || is_hookable_markdown(&resolved, &user_dirs) {
        return markdown_hook_verdict(tool_input, &resolved).map(|why| deny(spelled, &why));
    }
    None
}

/// The protected class `path` falls in, or `None`.
fn protected_class(
    path: &Path,
    user_dirs: &[PathBuf],
    home: Option<&Path>,
) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    let parent = path.parent()?;
    let parent_name = parent.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // #8928 fix round: Unicode case folding, as APFS compares names.
    let is = |a: &str, b: &str| eq_fold(a, b);
    if is(parent_name, ".claude") && SETTINGS_NAMES.iter().any(|n| is(n, name)) {
        return Some("a Claude Code project settings file");
    }
    if is(name, ".mcp.json") {
        return Some("an `.mcp.json` MCP server file");
    }
    let names = normal_names(path);
    if let Some(git) = names.iter().position(|n| is(n, ".git")) {
        if names[git + 1..].iter().any(|n| is(n, "hooks")) {
            return Some("a git hook");
        }
        if is(name, "config") || is(name, "config.worktree") {
            return Some("a git config file");
        }
    }
    // #8928 fix round: plugin manifests and the scripts their hooks run.
    if user_dirs
        .iter()
        .any(|dir| starts_with_ci(path, &dir.join("plugins")))
    {
        return Some("a Claude Code plugin file");
    }
    let user_file = user_dirs
        .iter()
        .any(|dir| same_ci(parent, dir) && USER_CONFIG_NAMES.iter().any(|n| is(n, name)));
    let home_json = home.is_some_and(|h| same_ci(parent, h)) && is(name, ".claude.json");
    (user_file || home_json).then_some("a Claude Code user settings file")
}

/// Whether `path` is skill or agent Markdown Claude Code reads hooks from:
/// a `.md` under `.claude/skills` or `.claude/agents`, or under a user config
/// directory's `skills` or `agents`.
pub(crate) fn is_hookable_markdown(path: &Path, user_dirs: &[PathBuf]) -> bool {
    let md = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| eq_fold(e, "md"));
    if !md {
        return false;
    }
    let is_kind = |n: &str| eq_fold(n, "skills") || eq_fold(n, "agents");
    let names = normal_names(path);
    let in_project = names
        .windows(2)
        .any(|w| eq_fold(&w[0], ".claude") && is_kind(&w[1]));
    let in_user = user_dirs.iter().any(|dir| {
        ["skills", "agents"]
            .iter()
            .any(|kind| starts_with_ci(path, &dir.join(kind)))
    });
    in_project || in_user
}

/// The normal components of `path`, as strings.
fn normal_names(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// `path` with `.` dropped and `..` applied lexically.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The closing sentence every deny carries.
const REMEDY: &str = "The Claude Code sandbox keeps Bash away from settings files, `.mcp.json`, \
     git config and hooks; this rule keeps the Write and Edit tools away from them too, along with \
     every path a hook or `statusLine` command runs and frontmatter `hooks:` in skill and agent \
     Markdown. It binds the PM and every agent; only the Architect session's main thread is \
     exempt. Ask the operator to make the change.";

/// The deny for `target`, which is `what`.
fn deny(target: &str, what: &str) -> String {
    format!("Protected-path write denied (#8928): `{target}` is {what}. {REMEDY}")
}

#[cfg(test)]
#[path = "pm_guard_protected_paths_tests.rs"]
mod tests;
