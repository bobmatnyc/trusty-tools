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
//! Residual bypasses, stated rather than hidden: the path is resolved
//! lexically, so a symlink into a checkout is not followed — the same limit
//! [`is_main_checkout`] carries and documents. A write performed through `Bash`
//! rather than an edit tool is classified by `pm_guard_bash` instead, which
//! reaches the same deny through `SHELL_EDIT_REASON` for the PM but does not
//! yet carry the main-checkout dimension for dispatched agents; that is stated
//! in ADR-0048's Consequences rather than closed here.
//!
//! Test: `denies_*`, `allows_*` below; `pm_guard_denies_a_source_write_in_a_main_checkout`
//! and siblings in `tests/tm_hook_pm_guard.rs` run the real binary, including
//! the subagent-marked payload.

use std::path::{Path, PathBuf};

use trusty_mpm::core::project_aliases::is_main_checkout;

use super::pm_guard::{EDIT_TOOLS, edit_tool_target_path, is_source_code_path};

/// Deny a source-file write whose target lives in a project's main checkout.
///
/// Why: the one entry point `pm_guard` calls, ordered cheapest test first so
/// the overwhelming majority of tool calls — everything that is not an edit —
/// costs one slice comparison and nothing else.
/// What: `Some(reason)` when `tool_name` is an [`EDIT_TOOLS`] member, its
/// target path [`is_source_code_path`], and the directory that path resolves
/// into [`is_main_checkout`]. `None` (ALLOW) in every other case.
/// Test: `denies_a_source_write_in_a_main_checkout`,
/// `allows_documents_and_configuration`, `allows_a_write_inside_a_worktree`.
pub(crate) fn evaluate_main_checkout_write(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    cwd: &Path,
) -> Option<String> {
    if !EDIT_TOOLS.contains(&tool_name) {
        return None;
    }
    let target = edit_tool_target_path(tool_input)?;
    if !is_source_code_path(target) {
        return None;
    }
    let resolved = resolve_write_target(target, cwd);
    is_main_checkout(&resolved).then(|| deny_reason(target))
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
    fn allows_every_non_edit_tool() {
        let dir = main_checkout();
        for tool in ["Read", "Bash", "Grep", "Agent", "SendMessage"] {
            assert_eq!(
                evaluate_main_checkout_write(
                    tool,
                    Some(&write_input(&dir.path().join("src/lib.rs"))),
                    dir.path()
                ),
                None,
                "{tool} is not an edit tool"
            );
        }
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
