//! The `PreToolUse` compression rewrite for a Bash call in an isolation worktree.
//!
//! Why (#10973): #7477 stopped `tm hook` from rewriting any Bash call whose cwd
//! sits under `.claude/worktrees/`, because Claude Code's worktree-isolation
//! classifier refuses the `{ <cmd>; printf …; } | tm compress --tool "…"` shape
//! as "too complex to verify". Delegated agents run almost entirely in those
//! trees, so the `compress` ledger fell from hundreds of rows a day to tens.
//! This module restores compression there with a shape that has no brace
//! group, no pipe, no `;` and no `$?`: one simple command, `tm compress` running
//! the original program itself (`commands::compress_run`).
//! What: [`rewrite_bash_command_for_cwd`] picks the shape. Outside an isolation
//! worktree it is the unchanged brace-group rewrite. Inside one, and when the
//! cwd cannot be read, it is [`isolated_rewrite`], which also decides the
//! permission the rewrite carries from the ORIGINAL command, as the build-lease
//! rewrite does, so a `deny` rule on the original is never bypassed.
//! Test: `the_isolated_shape_is_one_simple_command`,
//! `an_isolated_rewrite_refuses_what_the_brace_rewrite_refuses`,
//! `an_isolated_rewrite_skips_an_env_prefixed_or_wrapped_command`,
//! `an_isolated_rewrite_skips_a_denied_command_and_keeps_an_allow`,
//! `outside_a_worktree_the_brace_rewrite_is_unchanged`, and end to end
//! `hook_compresses_a_filter_covered_command_inside_an_isolation_worktree`.

use std::path::Path;

use super::hook_rewrite::{
    compressible_tool_name, is_isolation_worktree, rewrite_bash_command_for_compression,
    shlex_words, strip_wrapper_prefix,
};
use super::pm_guard_response::RewriteDecision;

/// The rewritten command plus the permission decision it must carry.
pub(crate) struct CwdRewrite {
    /// The command Claude Code runs instead of the original.
    pub(crate) command: String,
    /// `Some` only for the isolated shape; see [`isolated_rewrite`].
    pub(crate) permission: Option<RewriteDecision>,
}

/// The compression rewrite for `command` run from `cwd`, if one applies.
///
/// Why (#10973): see the module doc. The undecidable cwd takes the isolated
/// shape because that shape is valid everywhere, while the brace group is the
/// one #7477 showed the classifier refuses.
/// What: `cwd` absent, empty, or inside an isolation worktree →
/// [`isolated_rewrite`]; otherwise [`rewrite_bash_command_for_compression`]
/// with no permission decision, exactly as before.
/// Test: `outside_a_worktree_the_brace_rewrite_is_unchanged`,
/// `the_isolated_shape_is_one_simple_command`.
pub(crate) fn rewrite_bash_command_for_cwd(
    command: &str,
    cwd: Option<&Path>,
) -> Option<CwdRewrite> {
    match cwd.filter(|c| !c.as_os_str().is_empty()) {
        Some(dir) if !is_isolation_worktree(dir) => rewrite_bash_command_for_compression(command)
            .map(|command| CwdRewrite {
                command,
                permission: None,
            }),
        dir => isolated_rewrite(command, dir.unwrap_or(Path::new(""))),
    }
}

/// `tm compress --tool "<tool>" -- <command>`, when that runs the same program.
///
/// Why (#7477, #10973): the protected case is that the classifier-refused
/// shape never reaches an isolation worktree. This shape carries none of its
/// parts: `tm compress` runs the program and returns its exit status, so no
/// brace group, `printf`, `$?` or pipe is needed.
/// What: `None` when [`compressible_tool_name`] refuses the command (every
/// guard of the brace rewrite applies unchanged), when the first word is not
/// the program — a `KEY=value` prefix or a wrapper such as `sudo`/`env`/
/// `timeout`, which `tm compress` would exec as the program — or when the
/// original matches a `deny` permission rule. The permission decision is
/// otherwise the one the build-lease rewrite computes for the original.
/// Test: `an_isolated_rewrite_refuses_what_the_brace_rewrite_refuses`,
/// `an_isolated_rewrite_skips_an_env_prefixed_or_wrapped_command`,
/// `an_isolated_rewrite_skips_a_denied_command_and_keeps_an_allow`.
fn isolated_rewrite(command: &str, cwd: &Path) -> Option<CwdRewrite> {
    let trimmed = command.trim();
    let tool = compressible_tool_name(trimmed)?;
    // #10973: the program must be the first word, since `tm compress` execs it.
    if strip_wrapper_prefix(&shlex_words(trimmed)) != Some(0) {
        return None;
    }
    // #10973: decided from the original, so a deny rule is never bypassed.
    let permission = super::pm_guard_build_lease::rewrite_permission(trimmed, cwd)?;
    Some(CwdRewrite {
        command: format!("tm compress --tool \"{tool}\" -- {trimmed}"),
        permission,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKTREE: &str = "/repo/.claude/worktrees/agent-a/crates/x";

    fn isolated(command: &str) -> Option<String> {
        rewrite_bash_command_for_cwd(command, Some(Path::new(WORKTREE))).map(|r| r.command)
    }

    /// #7477: the protected case — nothing the classifier refused reaches a worktree.
    #[test]
    fn the_isolated_shape_is_one_simple_command() {
        for cwd in [Some(Path::new(WORKTREE)), Some(Path::new("")), None] {
            let out = rewrite_bash_command_for_cwd("git diff HEAD~1", cwd)
                .map(|r| r.command)
                .expect("a covered command is rewritten");
            assert_eq!(out, "tm compress --tool \"git diff\" -- git diff HEAD~1");
            for refused in ["{", "}", "|", ";", "printf", "$?", "&", ">"] {
                assert!(!out.contains(refused), "{refused} in {out}");
            }
        }
    }

    #[test]
    fn an_isolated_rewrite_refuses_what_the_brace_rewrite_refuses() {
        for command in [
            "",
            "make build",
            "git diff | head",
            "git diff && ls",
            "git diff > out.txt",
            "git diff # note",
            "cat -n src/lib.rs",
            "git status",
        ] {
            assert_eq!(isolated(command), None, "{command}");
            assert_eq!(
                rewrite_bash_command_for_compression(command),
                None,
                "{command}"
            );
        }
    }

    #[test]
    fn an_isolated_rewrite_skips_an_env_prefixed_or_wrapped_command() {
        for command in ["GIT_PAGER=cat git diff", "env git diff", "sudo git diff"] {
            assert_eq!(isolated(command), None, "{command}");
        }
    }

    #[test]
    fn an_isolated_rewrite_skips_a_denied_command_and_keeps_an_allow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        let tree = repo.join(".claude/worktrees/agent-a");
        std::fs::create_dir_all(&tree).expect("mkdir");
        std::fs::write(
            repo.join(".claude/settings.json"),
            r#"{"permissions":{"deny":["Bash(git log:*)"],"allow":["Bash(git diff:*)"]}}"#,
        )
        .expect("settings");
        assert!(
            rewrite_bash_command_for_cwd("git log -p", Some(&tree)).is_none(),
            "a denied original must run unchanged so Claude Code denies it"
        );
        let allowed = rewrite_bash_command_for_cwd("git diff", Some(&tree)).expect("rewritten");
        assert_eq!(allowed.permission, Some(RewriteDecision::Allow));
    }

    #[test]
    fn outside_a_worktree_the_brace_rewrite_is_unchanged() {
        let out = rewrite_bash_command_for_cwd("git diff", Some(Path::new("/repo/crates/x")))
            .expect("rewritten");
        assert_eq!(
            Some(out.command),
            rewrite_bash_command_for_compression("git diff")
        );
        assert_eq!(out.permission, None);
    }
}
