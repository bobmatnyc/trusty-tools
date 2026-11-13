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
//! What: [`cwd_rewrite_decision`] picks the shape. Outside an isolation
//! worktree it is the unchanged brace-group rewrite. Inside one it is
//! [`isolated_rewrite`], which also decides the permission the rewrite carries
//! from the ORIGINAL command, as the build-lease rewrite does, so a `deny` rule
//! on the original is never bypassed. An unreadable cwd gets no rewrite.
//! Test: `the_isolated_shape_is_one_simple_command`,
//! `a_program_that_is_not_an_executable_is_not_rewritten`,
//! `an_isolated_rewrite_refuses_what_the_brace_rewrite_refuses`,
//! `an_isolated_rewrite_skips_an_env_prefixed_or_wrapped_command`,
//! `an_isolated_rewrite_skips_a_denied_command_and_keeps_an_allow`,
//! `outside_a_worktree_the_brace_rewrite_is_unchanged`, and end to end
//! `hook_compresses_a_filter_covered_command_inside_an_isolation_worktree`.

use std::path::Path;

use super::hook_rewrite::{
    compressible_tool_decision, compression_rewrite_decision, is_isolation_worktree, shlex_words,
    strip_wrapper_prefix,
};
use super::pm_guard_response::RewriteDecision;
use trusty_mpm::core::savings_opportunity::SkipReason;

/// The rewritten command plus the permission decision it must carry.
pub(crate) struct CwdRewrite {
    /// The command Claude Code runs instead of the original.
    pub(crate) command: String,
    /// `Some` only for the isolated shape; see [`isolated_rewrite`].
    pub(crate) permission: Option<RewriteDecision>,
}

/// The compression rewrite for `command` run from `cwd`, or why none applies.
///
/// Why (#10973): see the module doc. An undecidable cwd gets no rewrite, as
/// before #10973: the project's permission rules are read from the cwd's
/// ancestors, so without one a `deny` rule could not be honoured (#10981 review).
/// What: `cwd` absent or empty → `Err(NoCwd)`; inside an isolation worktree →
/// [`isolated_rewrite`]; otherwise `compression_rewrite_decision`, the brace
/// rewrite with D60's `cd <dir> && ` split, with no permission decision. A
/// refusal names the guard that refused.
/// Test: `outside_a_worktree_the_brace_rewrite_is_unchanged`,
/// `the_isolated_shape_is_one_simple_command`,
/// `an_isolated_refusal_names_its_guard`,
/// `an_isolated_worktree_refusal_is_named_on_the_row`.
pub(crate) fn cwd_rewrite_decision(
    command: &str,
    cwd: Option<&Path>,
) -> Result<CwdRewrite, SkipReason> {
    match cwd.filter(|c| !c.as_os_str().is_empty()) {
        // #10973: the brace rewrite's own decision, D60's `cd` split included.
        Some(dir) if !is_isolation_worktree(dir) => {
            compression_rewrite_decision(command).map(|command| CwdRewrite {
                command,
                permission: None,
            })
        }
        Some(dir) => isolated_rewrite(command, dir),
        // #10981 review: no cwd, no permission rules to decide from.
        None => Err(SkipReason::NoCwd),
    }
}

/// `tm compress --tool "<tool>" -- <command>`, when that runs the same program.
///
/// Why (#7477, #10973): the protected case is that the classifier-refused
/// shape never reaches an isolation worktree. This shape carries none of its
/// parts: `tm compress` runs the program and returns its exit status, so no
/// brace group, `printf`, `$?` or pipe is needed.
/// What: `Err` with the refusing guard (#10973) when
/// [`compressible_tool_decision`] refuses the command (every guard of the
/// brace rewrite applies unchanged), when the first word is not the program —
/// a `KEY=value` prefix or a wrapper such as `sudo`/`env`/`timeout`, which
/// `tm compress` would exec as the program (`WrapperPrefix`) — when that
/// program does not resolve to an executable file
/// ([`resolves_to_an_executable`]; a shell function or alias would be lost;
/// `NotExecutable`), or when the original matches a `deny` permission rule
/// (`Denied`). The permission decision is otherwise the one the build-lease
/// rewrite computes for the original.
/// Test: `an_isolated_rewrite_refuses_what_the_brace_rewrite_refuses`,
/// `a_program_that_is_not_an_executable_is_not_rewritten`,
/// `an_isolated_rewrite_skips_an_env_prefixed_or_wrapped_command`,
/// `an_isolated_rewrite_skips_a_denied_command_and_keeps_an_allow`.
fn isolated_rewrite(command: &str, cwd: &Path) -> Result<CwdRewrite, SkipReason> {
    let trimmed = command.trim();
    let tool = compressible_tool_decision(trimmed)?;
    // #10973: the program must be the first word, since `tm compress` execs it.
    let words = shlex_words(trimmed);
    if strip_wrapper_prefix(&words) != Some(0) {
        return Err(SkipReason::WrapperPrefix);
    }
    // #10981 review: `tm compress` execs the program without a shell, so only
    // an executable file keeps the command's meaning.
    let program = words.first().ok_or(SkipReason::Unknown)?;
    if !resolves_to_an_executable(program, cwd) {
        return Err(SkipReason::NotExecutable);
    }
    // #10973: decided from the original, so a deny rule is never bypassed.
    let permission =
        super::pm_guard_build_lease::rewrite_permission(trimmed, cwd).ok_or(SkipReason::Denied)?;
    Ok(CwdRewrite {
        command: format!("tm compress --tool \"{tool}\" -- {trimmed}"),
        permission,
    })
}

/// Whether `program` names an executable file the way the shell would find it.
///
/// Why (#10981 review): the Claude Code shell snapshot can define `rg` or `ls`
/// as a function or alias. Run through `tm compress`, which execs the word
/// directly, such a command fails with 127. A word with no executable file
/// behind it is therefore left to the shell.
/// What: a word containing `/` is resolved against `cwd` (an absolute word
/// stays absolute); any other word is searched on `PATH`. True only for a
/// regular file with an execute bit. A leading `\` (alias bypass) is ignored.
/// Residual: a function that shadows a real binary of the same name still
/// loses to the binary.
/// Test: `a_program_that_is_not_an_executable_is_not_rewritten`.
fn resolves_to_an_executable(program: &str, cwd: &Path) -> bool {
    let program = program.strip_prefix('\\').unwrap_or(program);
    if program.contains('/') {
        return is_executable_file(&cwd.join(program));
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| is_executable_file(&dir.join(program)))
    })
}

/// A regular file with an execute bit (any file, off Unix).
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::super::hook_rewrite::rewrite_bash_command_for_compression;
    use super::*;

    const WORKTREE: &str = "/repo/.claude/worktrees/agent-a/crates/x";

    fn isolated(command: &str) -> Option<String> {
        cwd_rewrite_decision(command, Some(Path::new(WORKTREE)))
            .ok()
            .map(|r| r.command)
    }

    /// #7477: the protected case — nothing the classifier refused reaches a
    /// worktree. #10973 critic finding 2: with no cwd the project's deny rules
    /// cannot be read, so an unknown cwd is not rewritten at all.
    #[test]
    fn the_isolated_shape_is_one_simple_command() {
        let out = isolated("git diff HEAD~1").expect("a covered command is rewritten");
        assert_eq!(out, "tm compress --tool \"git diff\" -- git diff HEAD~1");
        for refused in ["{", "}", "|", ";", "printf", "$?", "&", ">"] {
            assert!(!out.contains(refused), "{refused} in {out}");
        }
        for cwd in [Some(Path::new("")), None] {
            assert!(
                cwd_rewrite_decision("git diff HEAD~1", cwd).is_err(),
                "an unknown cwd {cwd:?} must not be rewritten"
            );
        }
    }

    /// #10973 critic finding 3: `tm compress` execs the program, so a shell
    /// function or alias of that name would be lost. Only a program that
    /// resolves to an executable is rewritten.
    #[test]
    fn a_program_that_is_not_an_executable_is_not_rewritten() {
        assert!(
            isolated("/bin/ls -la").is_some(),
            "an absolute executable path"
        );
        assert!(isolated("git diff").is_some(), "a program on PATH");
        assert_eq!(isolated("./no-such-dir-10973/git diff"), None);
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
            cwd_rewrite_decision("git log -p", Some(&tree)).is_err(),
            "a denied original must run unchanged so Claude Code denies it"
        );
        let allowed = cwd_rewrite_decision("git diff", Some(&tree)).expect("rewritten");
        assert_eq!(allowed.permission, Some(RewriteDecision::Allow));
    }

    /// #10973: each isolated-path refusal names its own guard.
    #[test]
    fn an_isolated_refusal_names_its_guard() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        let tree = repo.join(".claude/worktrees/agent-a");
        std::fs::create_dir_all(&tree).expect("mkdir");
        std::fs::write(
            repo.join(".claude/settings.json"),
            r#"{"permissions":{"deny":["Bash(git log:*)"]}}"#,
        )
        .expect("settings");
        let reason = |command: &str, cwd: Option<&Path>| cwd_rewrite_decision(command, cwd).err();
        assert_eq!(
            reason("env git diff", Some(&tree)),
            Some(SkipReason::WrapperPrefix)
        );
        assert_eq!(
            reason("./no-such-dir-10973/git diff", Some(&tree)),
            Some(SkipReason::NotExecutable)
        );
        assert_eq!(reason("git log -p", Some(&tree)), Some(SkipReason::Denied));
        assert_eq!(
            reason("git diff | head", Some(&tree)),
            Some(SkipReason::Chained)
        );
        // D60's `cd <dir> && ` form stays refused in the isolated shape.
        assert_eq!(
            reason("cd /tmp && git diff", Some(&tree)),
            Some(SkipReason::Chained)
        );
        assert_eq!(reason("git diff", None), Some(SkipReason::NoCwd));
        assert_eq!(reason("git diff", Some(&tree)), None, "rewritten");
    }

    #[test]
    fn outside_a_worktree_the_brace_rewrite_is_unchanged() {
        let out =
            cwd_rewrite_decision("git diff", Some(Path::new("/repo/crates/x"))).expect("rewritten");
        assert_eq!(
            Some(out.command),
            rewrite_bash_command_for_compression("git diff")
        );
        assert_eq!(out.permission, None);
    }
}
