//! Source writes into a main checkout, denied for the PM and for every agent
//! it dispatches ([ADR-0044](../../../../../../docs/adr/0044-main-checkout-write-boundary-and-agent-worktree-ownership.md),
//! enforced by [ADR-0048](../../../../../../docs/adr/0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md)).
//!
//! Why: ADR-0044 decision 1 says a main-checkout session may write documents
//! and configuration only, and decision 2 says the restriction is enforced
//! mechanically rather than by convention. Only the destructive-git half was
//! ever built (`pm_guard_bash::main_checkout`), which covers `git reset --hard`
//! and its siblings and nothing else — an ordinary `Write` to a `.rs` file in
//! the shared checkout passed every guard in the process. That is the write the
//! reported incident was made of.
//!
//! What: [`evaluate_main_checkout_write`] denies an [`EDIT_TOOLS`] call whose
//! target file is source code AND lives in a main checkout. The two halves are
//! deliberately different questions from the ones `evaluate_edit_tool` asks:
//! that rule is about WHO is writing (the PM, subject to a per-turn budget, and
//! exempt for dispatched agents), while this one is about WHERE the write
//! lands, and it holds for everyone. Documents and configuration stay writable
//! because [`is_source_code_path`] does not classify `.md`, `.toml`, `.json`,
//! or an extension-less file as source — that is the same list ADR-0044's
//! "documents and configuration" boundary was written against, so the two agree
//! by construction rather than by a second list kept in step by hand.
//!
//! **It pierces the subagent exemptions**, exactly as the destructive-git rule
//! next door does and for the same reason: ADR-0044 binds "the PM and every
//! agent it dispatches", and both `CLAUDE_MPM_SUB_AGENT` (Guard 1) and the
//! `agent_id` dispatch marker (Guard 4) return ALLOW precisely for the agents
//! this rule exists to bind. A version placed after either would be a no-op for
//! its whole population. The operator escape hatches (Guard 2's
//! `TRUSTY_MPM_DISABLE_HOOKS`, Guard 3's `TRUSTY_MPM_PM_UNRESTRICTED`) still
//! lift it, unchanged, along with every other rule — #3981.
//!
//! **Fail-open, decided per branch, and this one does not consult a daemon** —
//! so unlike `pm_guard_dispatch` it has no unreachable-daemon arm to degrade
//! through, and its answer does not depend on anything else running on the
//! machine. The indeterminate arms all resolve to ALLOW, and each is a case
//! where nothing was positively identified: a tool call with no readable target
//! path names no file; a target with no `.git` ancestor is not a checkout; a
//! non-source extension is not this rule's business. The guard denies only on
//! positive evidence of both halves.
//!
//! **A `Bash` write lands here too (#7399).** A shell write used to reach only
//! `pm_guard_bash`'s `SHELL_EDIT_REASON`, which asks WHO is writing: it is
//! budget-tiered and both subagent exemptions skip it, so `git diff
//! --output=<file>` and `echo … > <file>` each landed a source file in a shared
//! main checkout within budget. `Bash` now takes the same two halves as an edit
//! tool, reading its target from
//! [`pm_guard_bash::shell_write_target`](super::pm_guard_bash::shell_write_target)
//! — the redirect-or-git-write-option half of the one detector `pm_guard_bash`
//! already keeps, never a second parser. `SHELL_EDIT_REASON` is unchanged and
//! still fires for the PM on every shell write; this rule adds the WHERE
//! dimension ADR-0048's Consequences recorded as open.
//!
//! Residual bypasses, stated rather than hidden: the path is resolved
//! lexically, so a symlink into a checkout is not followed — the same limit
//! [`is_main_checkout`] carries and documents. The `Bash` half sees only a
//! write it can positively identify, so a write performed by an interpreter
//! (`python -c`), by a verb whose target sits in a trailing position
//! (`sed -i`), or through a variable the guard does not expand is not resolved
//! into a path and keeps only the `SHELL_EDIT_REASON` treatment.
//!
//! Test: `denies_*`, `allows_*` below; `pm_guard_denies_a_source_write_in_a_main_checkout`
//! and siblings in `tests/tm_hook_pm_guard.rs` run the real binary, including
//! the subagent-marked payload.

use std::path::{Path, PathBuf};

use trusty_mpm::core::project_aliases::is_main_checkout;

use super::pm_guard::{EDIT_TOOLS, edit_tool_target_path, is_source_code_path};
use super::pm_guard_bash::shell_write_target;

/// Deny a source-file write whose target lives in a project's main checkout.
///
/// Why: the one entry point `pm_guard` calls, ordered cheapest test first so
/// the overwhelming majority of tool calls — everything that is neither an edit
/// nor a `Bash` call — costs one slice comparison and nothing else.
/// What: `Some(reason)` when the call names a write target, that target
/// [`is_source_code_path`], and the directory it resolves into
/// [`is_main_checkout`]. The target comes from [`edit_tool_target_path`] for an
/// [`EDIT_TOOLS`] member and from [`shell_write_target`] for `Bash` (#7399).
/// `None` (ALLOW) in every other case.
/// Test: `denies_a_source_write_in_a_main_checkout`,
/// `allows_documents_and_configuration`, `allows_a_write_inside_a_worktree`,
/// `denies_a_git_output_write_in_a_main_checkout`,
/// `allows_a_git_read_in_a_main_checkout`.
pub(crate) fn evaluate_main_checkout_write(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    cwd: &Path,
) -> Option<String> {
    let target = write_target(tool_name, tool_input)?;
    if !is_source_code_path(&target) {
        return None;
    }
    let resolved = resolve_write_target(&target, cwd);
    is_main_checkout(&resolved).then(|| deny_reason(&target))
}

/// The file this tool call would write, whichever tool named it.
///
/// Why (#7399): the boundary's question is WHERE a write lands, and the answer
/// is the same question for an edit tool and for a shell write — only the place
/// the path is written down differs. Resolving both here keeps one deny, one
/// message and one pair of halves rather than a second rule for `Bash`.
/// What: `tool_input.file_path` for an [`EDIT_TOOLS`] member, and for `Bash`
/// the positively identified write target of its command
/// ([`shell_write_target`]). `None` for every other tool, and for a command
/// that names no write.
/// Test: `denies_a_git_output_write_in_a_main_checkout`,
/// `denies_a_shell_redirect_into_a_main_checkout`.
fn write_target(tool_name: &str, tool_input: Option<&serde_json::Value>) -> Option<String> {
    if EDIT_TOOLS.contains(&tool_name) {
        return edit_tool_target_path(tool_input).map(str::to_owned);
    }
    if tool_name != "Bash" {
        return None;
    }
    let command = tool_input?.get("command")?.as_str()?;
    shell_write_target(command)
}

/// The directory a write to `target` would land in.
///
/// Why: [`is_main_checkout`] answers about a DIRECTORY, and the target names a
/// file that may not exist yet. Asking about the file's parent is what makes
/// the answer well defined for a `Write` that creates a new file, which is the
/// common case for the write this rule is trying to stop.
/// What: `target` resolved against `cwd` when relative, then its parent. A
/// target with no parent component (a bare filename at the filesystem root)
/// falls back to the resolved path itself.
/// Test: `resolves_a_relative_target_against_the_hook_cwd`.
fn resolve_write_target(target: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(target);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    absolute
        .parent()
        .map_or_else(|| absolute.clone(), Path::to_path_buf)
}

/// Build the deny message.
///
/// Why: a bare refusal leaves the model guessing, and the guessed retry is
/// worse than the original call — the observed shape is an agent that reaches
/// for `Bash` and `cat >` after an `Edit` is refused. So the text names what
/// was blocked, why this directory is different from every other directory the
/// agent can write to, and the two remedies that actually exist. It says what
/// IS still writable in the same breath, because "read-only" reads as "you can
/// do nothing here" and that is not what ADR-0044 decided.
/// Test: `deny_reason_names_the_file_and_both_remedies`.
fn deny_reason(target: &str) -> String {
    format!(
        "Source write denied in a main checkout (ADR-0044): `{target}` is a source file in a \
         project's main checkout, which is read-only apart from documents and configuration. \
         Other sessions stand in this same directory — the reported failure is branches \
         switching under each other and a commit landing on a workstream it did not belong to, \
         with no error at any step. Do this work in a worktree instead: if you are a dispatched \
         agent, ask the PM to re-dispatch you with `isolation: \"worktree\"`, which gets you \
         your own tree; if you are the PM, dispatch the change to an agent rather than writing \
         it here. Documents (`.md`), configuration (`.toml`, `.json`, `.yaml`), and everything \
         under `.claude/worktrees/**` stay writable and are not affected by this rule."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A directory that answers `is_main_checkout`: a `.git` DIRECTORY, which
    /// is how git marks a main checkout and never a linked worktree.
    fn main_checkout() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".git")).expect("mkdir .git");
        dir
    }

    fn write_input(path: &Path) -> serde_json::Value {
        serde_json::json!({"file_path": path.to_string_lossy(), "content": "fn main() {}"})
    }

    #[test]
    fn denies_a_source_write_in_a_main_checkout() {
        let dir = main_checkout();
        for tool in EDIT_TOOLS {
            let reason = evaluate_main_checkout_write(
                tool,
                Some(&write_input(&dir.path().join("src/lib.rs"))),
                dir.path(),
            )
            .unwrap_or_else(|| panic!("{tool} on a source file in a main checkout must be denied"));
            assert!(reason.contains("ADR-0044"), "{reason}");
        }
    }

    #[test]
    fn allows_documents_and_configuration() {
        // ADR-0044 decision 1 and decision 3: documents and configuration are
        // what a main-checkout session is FOR, and framework deployment writes
        // `.claude/` and `TASK.md` on every launch.
        let dir = main_checkout();
        for name in [
            "README.md",
            "CLAUDE.md",
            "Cargo.toml",
            ".claude/settings.json",
            "TASK.md",
            "docs/adr/0044-x.md",
            "Makefile",
        ] {
            assert_eq!(
                evaluate_main_checkout_write(
                    "Write",
                    Some(&write_input(&dir.path().join(name))),
                    dir.path()
                ),
                None,
                "{name} is a document or configuration and must stay writable"
            );
        }
    }

    #[test]
    fn allows_a_write_inside_a_worktree() {
        // The whole point of the rule is that there IS somewhere to write. A
        // linked worktree carries a `.git` FILE rather than a directory.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".git"), "gitdir: /elsewhere").expect("write .git");
        assert_eq!(
            evaluate_main_checkout_write(
                "Write",
                Some(&write_input(&dir.path().join("src/lib.rs"))),
                dir.path()
            ),
            None
        );
    }

    #[test]
    fn allows_a_write_outside_any_repository() {
        // Not a checkout at all, so there is nothing for this rule to protect.
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            evaluate_main_checkout_write(
                "Write",
                Some(&write_input(&dir.path().join("scratch.rs"))),
                dir.path()
            ),
            None
        );
    }

    #[test]
    fn allows_every_tool_that_names_no_write() {
        // `Bash` is in the list since #7399, and stays here: this payload
        // carries a `file_path` and no `command`, so the shell half finds
        // nothing to resolve.
        let dir = main_checkout();
        for tool in ["Read", "Bash", "Grep", "Agent", "SendMessage"] {
            assert_eq!(
                evaluate_main_checkout_write(
                    tool,
                    Some(&write_input(&dir.path().join("src/lib.rs"))),
                    dir.path()
                ),
                None,
                "{tool} names no write here"
            );
        }
    }

    /// A `Bash` payload running `command`.
    fn bash_input(command: &str) -> serde_json::Value {
        serde_json::json!({"command": command})
    }

    // #7399: `git diff --output=<file>` writes exactly as `> <file>` does, so
    // the boundary must answer the same way for both.
    #[test]
    fn denies_a_git_output_write_in_a_main_checkout() {
        let dir = main_checkout();
        let target = dir.path().join("crates/x/src/lib.rs");
        let target = target.display().to_string();
        for command in [
            format!("git diff --output={target} HEAD~1 HEAD"),
            format!("git diff --output {target} HEAD"),
            format!("git log -1 --output={target}"),
            format!("git show HEAD --output={target}"),
            // `format-patch -o` and `archive -o` reach the same detector; they
            // name a directory and an archive, which `is_source_code_path`
            // does not classify as source, so the boundary leaves them to the
            // `SHELL_EDIT_REASON` deny #7405 already gives them.
            format!("git -C {} diff --output={target}", dir.path().display()),
        ] {
            let reason =
                evaluate_main_checkout_write("Bash", Some(&bash_input(&command)), dir.path())
                    .unwrap_or_else(|| {
                        panic!("`{command}` writes into a main checkout and must deny")
                    });
            assert!(reason.contains("ADR-0044"), "{reason}");
        }
    }

    // #7399: the redirect the boundary is made to match, proving both spellings
    // reach one deny with one message.
    #[test]
    fn denies_a_shell_redirect_into_a_main_checkout() {
        let dir = main_checkout();
        let target = dir.path().join("crates/x/src/lib.rs");
        let command = format!("echo 'fn main() {{}}' > {}", target.display());
        let reason = evaluate_main_checkout_write("Bash", Some(&bash_input(&command)), dir.path())
            .expect("a redirect into a main checkout's source must deny");
        assert!(reason.contains("ADR-0044"), "{reason}");
    }

    // #7399: the worktree is where the write is SUPPOSED to land.
    #[test]
    fn allows_a_git_output_write_inside_a_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".git"), "gitdir: /elsewhere").expect("write .git");
        let target = dir.path().join("crates/x/src/lib.rs");
        let command = format!("git diff --output={} HEAD", target.display());
        assert_eq!(
            evaluate_main_checkout_write("Bash", Some(&bash_input(&command)), dir.path()),
            None
        );
    }

    // #7399 must not cost a single read. `--no-index` compares two files and
    // writes none; the plain forms write nothing either.
    #[test]
    fn allows_a_git_read_in_a_main_checkout() {
        let dir = main_checkout();
        let a = dir.path().join("crates/x/src/lib.rs");
        let a = a.display().to_string();
        for command in [
            format!("git diff --no-index {a} {a}"),
            "git diff HEAD~1 HEAD".to_string(),
            "git diff --stat".to_string(),
            format!("git diff --output-indicator-new=+ -- {a}"),
            format!("git format-patch --stdout -1 HEAD -- {a}"),
            // The sed/awk trailing token names a file `sed -n` only READS, so
            // the boundary deliberately does not resolve it.
            format!("sed -n '1,5p' {a}"),
        ] {
            assert_eq!(
                evaluate_main_checkout_write("Bash", Some(&bash_input(&command)), dir.path()),
                None,
                "`{command}` writes nothing"
            );
        }
    }

    // #7399: documents keep the ADR-0049 carve-out through the shell too.
    #[test]
    fn allows_a_git_output_write_of_a_document() {
        let dir = main_checkout();
        let command = format!("git diff --output={}/notes.md HEAD", dir.path().display());
        assert_eq!(
            evaluate_main_checkout_write("Bash", Some(&bash_input(&command)), dir.path()),
            None
        );
    }

    #[test]
    fn allows_a_call_with_no_readable_target() {
        // Indeterminate: nothing was positively identified, so nothing is
        // denied. An input with no path names no file to classify.
        let dir = main_checkout();
        for input in [
            None,
            Some(serde_json::json!({})),
            Some(serde_json::json!({"file_path": ""})),
            Some(serde_json::json!({"file_path": 7})),
        ] {
            assert_eq!(
                evaluate_main_checkout_write("Write", input.as_ref(), dir.path()),
                None,
                "{input:?} names no target"
            );
        }
    }

    #[test]
    fn resolves_a_relative_target_against_the_hook_cwd() {
        // The hook payload carries relative paths routinely; resolving them
        // against the wrong base would make the rule miss every one of them.
        let dir = main_checkout();
        assert!(
            evaluate_main_checkout_write(
                "Edit",
                Some(&serde_json::json!({"file_path": "src/lib.rs"})),
                dir.path()
            )
            .is_some(),
            "a relative source path must resolve against the hook cwd"
        );
    }

    #[test]
    fn denies_an_absolute_write_from_outside_the_checkout() {
        // The shape a dispatched agent standing in its own worktree produces
        // when it reaches back into the shared tree by absolute path. The cwd
        // is innocent; the target is not.
        let checkout = main_checkout();
        let elsewhere = tempfile::tempdir().expect("tempdir");
        assert!(
            evaluate_main_checkout_write(
                "Write",
                Some(&write_input(&checkout.path().join("src/lib.rs"))),
                elsewhere.path()
            )
            .is_some(),
            "the target decides, not the caller's directory"
        );
    }

    #[test]
    fn deny_reason_names_the_file_and_both_remedies() {
        let reason = deny_reason("src/lib.rs");
        assert!(reason.contains("src/lib.rs"), "{reason}");
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
        assert!(reason.contains("Documents"), "{reason}");
    }
}
