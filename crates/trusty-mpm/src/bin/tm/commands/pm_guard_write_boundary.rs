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
//! [`is_main_checkout`] carries and documents. The `Bash` half sees only the
//! two shapes that NAME their target unambiguously — a redirect, and a git
//! write option — so everything else keeps only the `SHELL_EDIT_REASON`
//! treatment and gets no WHERE dimension:
//!
//! * `git apply` and `git am` write the files a patch names, and the patch
//!   names them, not the argv.
//! * `tee`, `cp`, `mv`, `install` and `dd` write a target that is an ordinary
//!   argument, indistinguishable here from the source they read.
//! * `sed -i` and the awk family put their target in the trailing position,
//!   which a read (`sed -n '1,5p' <file>`) occupies identically.
//! * An interpreter (`python -c`, `node -e`) carries its write inside a
//!   program string this guard does not parse.
//!
//! Each is a candidate for the same detector rather than a second rule; none
//! is closed here.
//!
//! **What an unexpanded expansion costs, exactly (#7838).** Three leading
//! forms ARE resolved, because this process holds their values rather than
//! guessing them ([`expand_leading_base`]): `~` and `$HOME`/`${HOME}` against
//! the hook's own HOME, which it shares with the shell it is judging, and
//! `$PWD`/`${PWD}` against the `cwd` the hook is handed. Any other variable or
//! command substitution is not resolved: nothing in this process holds the
//! value. The allowance is therefore scoped to the one thing such an expansion
//! actually hides, the BASE directory — [`base_directory_is_unresolvable`] —
//! leaving exactly three gaps:
//!
//! * A target whose FIRST segment is an unresolvable expansion (`$SP/v1.zsh`,
//!   `$FOO.rs`) is allowed even when it would have expanded into the checkout.
//!   This is the bypass #7838 chose over refusing every scratchpad write.
//! * A `~user/…` target names another account's home and is allowed unread;
//!   so is a `~`- or `$HOME`-rooted target when HOME is unset.
//! * An expansion in a deeper segment or in the filename is classified from
//!   the literal segments around it, so one that expands to `..` — or to a
//!   name carrying `/` — is judged against the wrong directory. That error
//!   runs toward DENY, never toward a silent allow, so it costs a refusal the
//!   operator can see and never the write ADR-0044 exists to stop.
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
/// `allows_a_git_read_in_a_main_checkout`,
/// `allows_a_target_whose_base_directory_is_an_unexpanded_expansion`,
/// `denies_an_expansion_confined_to_the_filename`,
/// `denies_a_leading_tilde_that_resolves_into_the_checkout`,
/// `denies_a_dollar_pwd_first_segment_that_resolves_into_the_checkout`,
/// `denies_a_dollar_home_first_segment_that_resolves_into_the_checkout`.
pub(crate) fn evaluate_main_checkout_write(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    cwd: &Path,
) -> Option<String> {
    evaluate_main_checkout_write_with(tool_name, tool_input, cwd, home_dir().as_deref())
}

/// [`evaluate_main_checkout_write`] with the home directory injected.
///
/// Why (#7838 review, CRITICAL): the tilde arm needs a home that resolves into
/// a fixture checkout to be tested at all, and mutating `$HOME` in-process
/// races every sibling test in the binary — the failure #7746/#7989 fixed by
/// adding seams rather than by locking the environment. So the ambient read
/// happens once, in the wrapper above, and the decision itself is pure.
/// What: identical to [`evaluate_main_checkout_write`] except that `home` is
/// supplied. `None` for `home` is the HOME-unset case, which makes a `~`- or
/// `$HOME`-rooted target indeterminate.
/// Test: `denies_a_leading_tilde_that_resolves_into_the_checkout`,
/// `denies_a_dollar_home_first_segment_that_resolves_into_the_checkout`,
/// `allows_a_leading_tilde_when_home_is_unknown`.
fn evaluate_main_checkout_write_with(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    cwd: &Path,
    home: Option<&str>,
) -> Option<String> {
    let target = write_target(tool_name, tool_input)?;
    // #7838: only the part of the path that decides the BASE directory has to
    // be literal; an expansion deeper in the path lands under a base this
    // guard already knows.
    let resolvable = expand_leading_base(&target, cwd, home)?;
    if base_directory_is_unresolvable(&resolvable) {
        return None;
    }
    if !is_source_code_path(&resolvable) {
        return None;
    }
    let resolved = resolve_write_target(&resolvable, cwd);
    // The message quotes the spelling the caller used, not the expansion.
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

/// This process's home directory, read once at the ambient entry point.
///
/// Why: the `~` and `$HOME` arms of [`expand_leading_base`] need it, and a
/// `PreToolUse` hook inherits HOME from the very shell whose command it is
/// judging — unlike an arbitrary variable, the two agree by construction.
/// What: `$HOME`, `None` when unset or blank.
/// Test: `denies_a_leading_tilde_that_resolves_into_the_checkout` exercises the
/// value through the injected seam.
fn home_dir() -> Option<String> {
    std::env::var("HOME").ok().filter(|home| !home.is_empty())
}

/// Substitute a first segment this process can compute, or refuse to guess.
///
/// Why (#7838 review, CRITICAL): three leading forms are NOT arbitrary
/// variables. A `PreToolUse` hook shares the invoking shell's HOME and is
/// handed that shell's working directory, so `~`, `$HOME` and `$PWD` each name
/// a directory this process holds exactly. Routing them into the generic
/// unknown-expansion arm handed a real in-checkout write an ALLOW on any
/// machine whose checkout sits under `$HOME` — the ordinary layout — and on
/// every `$PWD`-rooted write without exception. Only these spellings are
/// substituted; a value guessed for any other name would be a guess, which is
/// the thing this rule refuses to make.
/// What: `Some(<base><rest>)` for `~`, `~/…`, and a FIRST SEGMENT spelled
/// exactly `$PWD`, `${PWD}`, `$HOME` or `${HOME}` — `cwd` for the two `PWD`
/// spellings, `home` for `~` and the two `HOME` spellings. `Some(target)`
/// unchanged for every other target, which then meets
/// [`base_directory_is_unresolvable`] as before. `None` when HOME is needed and
/// unknown, or for the `~user/…` form, which names another account's home and
/// is not this process's to resolve; `None` is indeterminate and the caller
/// allows.
/// Test: `denies_a_leading_tilde_that_resolves_into_the_checkout`,
/// `denies_a_dollar_pwd_first_segment_that_resolves_into_the_checkout`,
/// `denies_a_dollar_home_first_segment_that_resolves_into_the_checkout`,
/// `allows_a_leading_tilde_when_home_is_unknown`.
fn expand_leading_base(target: &str, cwd: &Path, home: Option<&str>) -> Option<String> {
    if let Some(rest) = target.strip_prefix('~') {
        if !(rest.is_empty() || rest.starts_with('/')) {
            return None;
        }
        return Some(join_base(home?, rest));
    }
    let (first, rest) = match target.split_once('/') {
        Some((first, rest)) => (first, format!("/{rest}")),
        None => (target, String::new()),
    };
    match first {
        "$PWD" | "${PWD}" => Some(join_base(&cwd.to_string_lossy(), &rest)),
        "$HOME" | "${HOME}" => Some(join_base(home?, &rest)),
        _ => Some(target.to_string()),
    }
}

/// Glue a substituted base to the remainder of the path.
///
/// What: `base` with any trailing `/` dropped, then `rest`, which is either
/// empty or already starts with `/` — so `$PWD/x` and a `cwd` of `/` yield
/// `/x`, not `//x`.
/// Test: `denies_a_dollar_pwd_first_segment_that_resolves_into_the_checkout`.
fn join_base(base: &str, rest: &str) -> String {
    format!("{}{rest}", base.trim_end_matches('/'))
}

/// Is the BASE directory of `target` hidden behind an unexpanded expansion?
///
/// Why (#7838, narrowed by its review): the guard runs in its own process and
/// never sees the calling shell's variables, so `$SP/v1.zsh` reaches it
/// verbatim, `Path::is_absolute` says false, and [`resolve_write_target`]
/// joined it to the hook cwd — a scratchpad write denied as a source write in
/// the checkout it merely happened to be launched from. But only the LEADING
/// segment decides which directory the path is relative to. Testing the whole
/// string for `$` allowed `crates/x/src/$(true)lib.rs` and
/// `crates/$(echo x)/src/lib.rs`, both of which land in the checkout no matter
/// what the expansion yields, and both of which this rule exists to deny.
/// Expanding the variable from THIS process's environment is still refused: a
/// shell variable set in the command's own shell is absent here, and one that
/// shares a name with an inherited variable may not share its value.
/// What: true only when the first non-empty segment of the directory component
/// carries `$` or a backtick — the segment that fixes the base. A target with
/// no `/` at all is its own filename, and an expansion there can still expand
/// to contain separators, so that case is indeterminate too. Every deeper
/// segment is left to [`is_main_checkout`], whose ancestor walk answers from
/// the literal segments that remain.
/// Test: `allows_a_target_whose_base_directory_is_an_unexpanded_expansion`,
/// `denies_an_expansion_confined_to_the_filename`,
/// `denies_an_expansion_in_a_middle_segment`.
fn base_directory_is_unresolvable(target: &str) -> bool {
    let Some((directory, _file)) = target.rsplit_once('/') else {
        return names_an_expansion(target);
    };
    directory
        .split('/')
        .find(|segment| !segment.is_empty())
        .is_some_and(names_an_expansion)
}

/// Does one path segment carry a shell expansion?
///
/// What: `$` (a variable or a `$(…)` substitution) or a backtick. Fail-open — a
/// literal `$` in a real directory name costs one missed deny, never a wrong one.
/// Test: `a_literal_target_is_not_read_as_an_expansion`.
fn names_an_expansion(segment: &str) -> bool {
    segment.contains('$') || segment.contains('`')
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

    // #7399 review, HIGH: a `>` inside a here-document BODY is prose, and the
    // whole heredoc reaches the guard as one segment (#6946). Denying on it
    // would refuse a command that writes nothing, through a deny no budget and
    // no subagent marker can soften.
    #[test]
    fn allows_a_heredoc_body_redirect_in_a_main_checkout() {
        let dir = main_checkout();
        let target = dir.path().join("crates/x/src/lib.rs");
        let command = format!("cat <<'EOF'\nsee: git diff > {}\nEOF", target.display());
        assert_eq!(
            evaluate_main_checkout_write("Bash", Some(&bash_input(&command)), dir.path()),
            None,
            "a here-document body is data, not a write"
        );
        // The operator line is still live.
        let live = format!("python3 <<'PY' > {}\nprint(1)\nPY", target.display());
        assert!(
            evaluate_main_checkout_write("Bash", Some(&bash_input(&live)), dir.path()).is_some(),
            "a redirect on the operator line is still a write"
        );
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

    // #7838: the reported shape is `$SP/v1.zsh`, the session scratchpad written
    // through a variable. The FIRST segment is the expansion, so the base
    // directory is unknown and none of these is evidence of a write into the
    // checkout.
    #[test]
    fn allows_a_target_whose_base_directory_is_an_unexpanded_expansion() {
        let dir = main_checkout();
        for command in [
            "cat <<'EOF' > $SP/v1.zsh\necho hi\nEOF",
            "echo hi > ${SP}/v1.zsh",
            "echo hi > $(mktemp -d)/probe.rs",
            // No separator at all: the token can still expand to contain one.
            "echo hi > $SCRATCH.rs",
        ] {
            assert_eq!(
                evaluate_main_checkout_write("Bash", Some(&bash_input(command)), dir.path()),
                None,
                "`{command}` names no base directory this guard can resolve"
            );
        }
        // An edit tool carries the same unresolved spelling through
        // `file_path`, and must answer the same way.
        assert_eq!(
            evaluate_main_checkout_write(
                "Write",
                Some(&serde_json::json!({"file_path": "$SP/v1.zsh"})),
                dir.path()
            ),
            None,
        );
    }

    // #7838 review, CRITICAL: an expansion AFTER a literal directory hides
    // nothing about WHERE the write lands — these were DENY before #7838 and
    // the allowance must not have reached them.
    #[test]
    fn denies_an_expansion_confined_to_the_filename() {
        let dir = main_checkout();
        for command in [
            "echo hi > crates/x/src/$(true)lib.rs",
            "echo hi > crates/x/src/`true`lib.rs",
        ] {
            let reason =
                evaluate_main_checkout_write("Bash", Some(&bash_input(command)), dir.path())
                    .unwrap_or_else(|| {
                        panic!("`{command}` lands in a literal in-checkout directory")
                    });
            assert!(reason.contains("ADR-0044"), "{reason}");
        }
    }

    // #7838 review, CRITICAL: same for a middle segment. Whatever
    // `$(echo trusty-mpm)` yields, the path is still relative to the hook cwd,
    // so `is_main_checkout`'s ancestor walk answers from the literal segments.
    #[test]
    fn denies_an_expansion_in_a_middle_segment() {
        let dir = main_checkout();
        // Through an edit tool, which carries the path verbatim. The shell
        // route is the `${PKG}` spelling below: `shell_write_target` tokenizes
        // on whitespace, so a `$( … )` carrying a space never reaches here as
        // one target at all — a limit of that lexer, not of this rule.
        for input in [
            serde_json::json!({"file_path": "crates/$(echo trusty-mpm)/src/lib.rs"}),
            serde_json::json!({"file_path": "crates/${PKG}/src/lib.rs"}),
        ] {
            let reason = evaluate_main_checkout_write("Write", Some(&input), dir.path())
                .unwrap_or_else(|| panic!("{input} is still relative to the hook cwd"));
            assert!(reason.contains("ADR-0044"), "{reason}");
        }
        let command = "echo hi > crates/${PKG}/src/lib.rs";
        let reason = evaluate_main_checkout_write("Bash", Some(&bash_input(command)), dir.path())
            .expect("a relative path under the hook cwd is still in the checkout");
        assert!(reason.contains("ADR-0044"), "{reason}");
    }

    // #7838 review, CRITICAL: a tilde is not an arbitrary variable — the hook
    // shares the invoking shell's HOME. HOME is injected rather than set, so
    // this cannot race a sibling test (#7746, #7989).
    #[test]
    fn denies_a_leading_tilde_that_resolves_into_the_checkout() {
        let home = tempfile::tempdir().expect("tempdir");
        let checkout = home.path().join("proj");
        std::fs::create_dir_all(checkout.join(".git")).expect("mkdir .git");
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let home = home.path().to_string_lossy().into_owned();
        let reason = evaluate_main_checkout_write_with(
            "Write",
            Some(&serde_json::json!({"file_path": "~/proj/src/lib.rs"})),
            elsewhere.path(),
            Some(&home),
        )
        .expect("a tilde path resolving into a main checkout must deny");
        assert!(reason.contains("ADR-0044"), "{reason}");
        // The message quotes what the caller wrote, not the expansion.
        assert!(reason.contains("~/proj/src/lib.rs"), "{reason}");
    }

    // The fail-open half: with no HOME, and for another account's home, the
    // tilde names a directory this process cannot compute.
    #[test]
    fn allows_a_leading_tilde_when_home_is_unknown() {
        let dir = main_checkout();
        assert_eq!(
            evaluate_main_checkout_write_with(
                "Write",
                Some(&serde_json::json!({"file_path": "~/proj/src/lib.rs"})),
                dir.path(),
                None,
            ),
            None,
            "HOME unset leaves the tilde unresolvable"
        );
        let cwd = dir.path();
        assert_eq!(
            expand_leading_base("~someone/src/lib.rs", cwd, Some("/home/me")),
            None
        );
        assert_eq!(
            expand_leading_base("$HOME/src/lib.rs", cwd, None),
            None,
            "HOME unset leaves `$HOME` unresolvable too"
        );
        assert_eq!(
            expand_leading_base("/tmp/a~b.rs", cwd, None),
            Some("/tmp/a~b.rs".to_string()),
            "a tilde that is not leading is an ordinary character"
        );
        // Only the four exact spellings substitute; a name that merely starts
        // with one stays in the generic unresolvable arm.
        for target in ["$PWDX/src/lib.rs", "$HOMEDIR/src/lib.rs", "$SP/v1.zsh"] {
            assert_eq!(
                expand_leading_base(target, cwd, Some("/home/me")),
                Some(target.to_string()),
                "{target} is not one of the four resolvable spellings"
            );
        }
    }

    // #7838 review round 2, CRITICAL: `$PWD` is not an arbitrary variable —
    // `cwd` is a parameter of this very function. Both routes that name a
    // target must resolve it.
    #[test]
    fn denies_a_dollar_pwd_first_segment_that_resolves_into_the_checkout() {
        let dir = main_checkout();
        let inputs = [
            ("Bash", bash_input("echo hi > $PWD/crates/x/src/lib.rs")),
            (
                "Write",
                serde_json::json!({"file_path": "$PWD/crates/x/src/lib.rs"}),
            ),
            (
                "Edit",
                serde_json::json!({"file_path": "${PWD}/crates/x/src/lib.rs"}),
            ),
        ];
        for (tool, input) in inputs {
            let reason = evaluate_main_checkout_write_with(tool, Some(&input), dir.path(), None)
                .unwrap_or_else(|| panic!("{tool} {input} resolves to the hook cwd"));
            assert!(reason.contains("ADR-0044"), "{reason}");
        }
    }

    // #7838 review round 2, CRITICAL: `$HOME` reaches the same value the `~`
    // arm already uses, so the two spellings must answer alike.
    #[test]
    fn denies_a_dollar_home_first_segment_that_resolves_into_the_checkout() {
        let home = tempfile::tempdir().expect("tempdir");
        let checkout = home.path().join("proj");
        std::fs::create_dir_all(checkout.join(".git")).expect("mkdir .git");
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let home = home.path().to_string_lossy().into_owned();
        for spelling in ["$HOME", "${HOME}"] {
            let target = format!("{spelling}/proj/src/lib.rs");
            let reason = evaluate_main_checkout_write_with(
                "Write",
                Some(&serde_json::json!({ "file_path": target })),
                elsewhere.path(),
                Some(&home),
            )
            .unwrap_or_else(|| panic!("{target} resolves into a main checkout"));
            assert!(reason.contains("ADR-0044"), "{reason}");
            assert!(reason.contains(&target), "{reason}");
        }
    }

    // The other half of #7838: the allowance is scoped to the base directory,
    // so a literal relative path still denies exactly as it did before.
    #[test]
    fn a_literal_target_is_not_read_as_an_expansion() {
        assert!(!base_directory_is_unresolvable("src/lib.rs"));
        assert!(!base_directory_is_unresolvable("/tmp/a~b.rs"));
        assert!(!base_directory_is_unresolvable(
            "crates/x/src/$(true)lib.rs"
        ));
        assert!(!base_directory_is_unresolvable(
            "crates/$(echo x)/src/lib.rs"
        ));
        assert!(base_directory_is_unresolvable("$SP/v1.zsh"));
        assert!(base_directory_is_unresolvable("/$(echo tmp)/v1.zsh"));
        let dir = main_checkout();
        assert!(
            evaluate_main_checkout_write(
                "Write",
                Some(&serde_json::json!({"file_path": "src/lib.rs"})),
                dir.path()
            )
            .is_some(),
            "a literal relative source path must still deny"
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
