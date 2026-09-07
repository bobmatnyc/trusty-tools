//! Repo-label and assignee value types plus the `gh`-backed primitives that the
//! extended [`super::system::TicketSystem`] uses for the `tm issue` verbs.
//!
//! Why: #1246 extends the ticket backend with label seeding (list/create) and
//! issue-level label/assignee mutation. Keeping the value types ([`RepoLabel`],
//! [`AssigneeTarget`]) and the `gh`-invocation free functions here — rather than
//! inflating `system.rs` past the 500-SLOC production cap — lets the
//! `GhTicketSystem` trait impl stay a thin delegation while all the `gh` wiring
//! (and its `FakeRunner` tests) live in one focused module.
//! What: the [`RepoLabel`] (name/color/description) and [`AssigneeTarget`]
//! (self/login/none) types, and the `gh_*` helpers (`gh_list_repo_labels`,
//! `gh_create_label`, `gh_add_label`, `gh_remove_label`, `gh_swap_labels`,
//! `gh_set_assignee`) that drive `gh` through the injected [`CommandRunner`].
//! Test: the `gh_*` unit tests in this file drive a `FakeRunner` (no live `gh`).

use super::runner::CommandRunner;

/// A repository label as it exists (or should exist) on GitHub.
///
/// Why: seeding (`tm issue seed-labels`) is create-missing against the repo's
/// label set, so both the "what the model wants" and "what the repo has" sides
/// need the same name/color/description shape for a diff. #6914 made that shape
/// the library's [`trusty_mpm::core::policy_labels::PolicyLabel`] rather than a
/// second, identical struct here — session launch and `tm issue` now diff and
/// create the same type.
/// Test: round-tripped through `gh_list_repo_labels` parsing in
/// `gh_list_repo_labels_parses`.
pub(crate) use trusty_mpm::core::policy_labels::PolicyLabel as RepoLabel;

/// The effective assignee mutation a transition should apply.
///
/// Why: per-state assignee rules resolve to one of three generic GitHub
/// operations; modelling them as an enum keeps `set_assignee` total and its
/// `gh` mapping (§5.4 of the RFC) explicit and testable.
/// What: `SelfUser` assigns the authenticated `gh` user; `Login(l)` assigns a
/// named login; `None` clears all current assignees (read-then-remove).
/// Test: `gh_set_assignee_self`, `gh_set_assignee_login`,
/// `gh_set_assignee_none_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AssigneeTarget {
    /// Assign the authenticated `gh` user (`gh api user` → `--add-assignee`).
    SelfUser,
    /// Assign a specific GitHub login.
    Login(String),
    /// Clear all assignees (read current set, then `--remove-assignee` each).
    None,
}

/// `gh label list --limit <n> --json name,color,description` → existing repo
/// labels.
///
/// Why: idempotent seeding needs the current label set so only missing labels
/// are created (and present ones are left untouched). #6914: a partial read is
/// worse than no read — `gh label list` returns 30 labels by default and never
/// says it truncated, so on any larger repo (trusty-tools carries 89) the seed
/// judged every policy label missing and died on the first `gh label create`
/// of a label that already existed.
/// What: runs the argv from
/// [`trusty_mpm::core::policy_labels::list_labels_argv`] through `runner` and
/// parses the JSON array into [`RepoLabel`]s. A page that comes back exactly
/// `LABEL_LIST_LIMIT` long is reported as an error rather than passed on as a
/// complete set; `gh` failure and bad JSON surface actionable errors too.
/// Test: `gh_list_repo_labels_parses`, `gh_list_repo_labels_errors`,
/// `gh_list_repo_labels_reads_past_the_default_page`,
/// `gh_list_repo_labels_rejects_a_truncated_page`.
pub(crate) fn gh_list_repo_labels<R: CommandRunner>(runner: &R) -> anyhow::Result<Vec<RepoLabel>> {
    // #6914: the probe asked for gh's default 30-label page and read a partial
    // label set as if it were the whole repo.
    let limit = trusty_mpm::core::policy_labels::LABEL_LIST_LIMIT;
    let argv = trusty_mpm::core::policy_labels::list_labels_argv(None, limit);
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out = runner.run("gh", &args)?;
    let json = out.ok_or_stderr("gh label list")?;
    if json.is_empty() {
        return Ok(Vec::new());
    }
    let labels: Vec<RepoLabel> = serde_json::from_str(&json)
        .map_err(|e| anyhow::anyhow!("failed to parse `gh label list` JSON: {e}"))?;
    if labels.len() >= limit {
        anyhow::bail!(
            "`gh label list` returned a full {limit}-label page, so the repo's label set may be \
             truncated; seeding against a partial set would try to re-create labels that already \
             exist (see #6914)"
        );
    }
    Ok(labels)
}

/// `gh label create <name> --color <hex> --description <desc>`.
///
/// Why: the create-missing half of seeding; the caller decides *whether* to
/// create (post-diff), this just performs one creation.
/// What: builds the command line with
/// [`trusty_mpm::core::policy_labels::create_label_argv`] — the crate's single
/// `gh label create` spelling since #6914 — and runs it through `runner`.
/// Never forces: seeding must leave a label the repo already carries untouched,
/// colour and description included.
/// Test: `gh_create_label_invokes_create`,
/// `gh_create_label_omits_empty_description`.
pub(crate) fn gh_create_label<R: CommandRunner>(
    runner: &R,
    label: &RepoLabel,
) -> anyhow::Result<()> {
    // #6914: one argv builder for both the launch path and `tm issue`.
    let argv = trusty_mpm::core::policy_labels::create_label_argv(label, None, false);
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    runner.run("gh", &args)?.ok_or_stderr("gh label create")?;
    Ok(())
}

/// `gh issue edit <n> --add-label <label>`.
///
/// Why: a transition-fallback primitive (the single-call swap is preferred;
/// `add_label` is the degraded two-call path and a building block for other ops).
/// What: adds one label to an issue via `gh issue edit`.
/// Test: `gh_add_label_invokes_edit`.
pub(crate) fn gh_add_label<R: CommandRunner>(
    runner: &R,
    issue: u64,
    label: &str,
) -> anyhow::Result<()> {
    let n = issue.to_string();
    runner
        .run("gh", &["issue", "edit", &n, "--add-label", label])?
        .ok_or_stderr("gh issue edit")?;
    Ok(())
}

/// `gh issue edit <n> --remove-label <label>`.
///
/// Why: the remove half of the two-call fallback and the `repair` primitive for
/// dropping a stale state label.
/// What: removes one label from an issue via `gh issue edit`.
/// Test: `gh_remove_label_invokes_edit`.
pub(crate) fn gh_remove_label<R: CommandRunner>(
    runner: &R,
    issue: u64,
    label: &str,
) -> anyhow::Result<()> {
    let n = issue.to_string();
    runner
        .run("gh", &["issue", "edit", &n, "--remove-label", label])?
        .ok_or_stderr("gh issue edit")?;
    Ok(())
}

/// Single-call atomic label swap (the transition default, RFC §5.2).
///
/// Why: `gh issue edit --add-label NEW --remove-label OLD` applies both in one
/// invocation, so the issue is never observed carrying both labels or neither —
/// the tightest atomicity `gh` offers and the form aligned with the visibility
/// north star. Collapsing to one recorded call also simplifies the test.
/// What: runs the combined add/remove `gh issue edit` in one call.
/// Test: `gh_swap_labels_single_call`.
pub(crate) fn gh_swap_labels<R: CommandRunner>(
    runner: &R,
    issue: u64,
    add: &str,
    remove: &str,
) -> anyhow::Result<()> {
    let n = issue.to_string();
    runner
        .run(
            "gh",
            &[
                "issue",
                "edit",
                &n,
                "--add-label",
                add,
                "--remove-label",
                remove,
            ],
        )?
        .ok_or_stderr("gh issue edit")?;
    Ok(())
}

/// Apply an [`AssigneeTarget`] to an issue (RFC §5.4 exact `gh` semantics).
///
/// Why: per-state assignee rules must map to concrete, observable GitHub
/// mutations. The subtlety is `None` (clear-all): `gh issue edit
/// --remove-assignee` needs an explicit login, so clearing first reads the
/// current assignee set and removes each by login.
/// What: `SelfUser` → `gh api user --jq .login` then `--add-assignee <login>`;
/// `Login` → `--add-assignee <login>`; `None` → for each of `current_assignees`,
/// `--remove-assignee <login>` (a no-op `gh`-call-free when the set is empty).
///
/// TOCTOU NOTE: the `None` (clear-all) rule is read-then-remove and is therefore
/// NOT atomic — an assignee added between the caller's read of `current_assignees`
/// and these `--remove-assignee` calls will survive. This is inherent to `gh`'s
/// lack of a "clear all assignees" primitive; the factory default never exercises
/// this path (all per-state rules are `unchanged`), so the window is unreachable
/// for the only current consumer.
/// Test: `gh_set_assignee_self`, `gh_set_assignee_login`,
/// `gh_set_assignee_none_removes_each`, `gh_set_assignee_none_empty_is_noop`.
pub(crate) fn gh_set_assignee<R: CommandRunner>(
    runner: &R,
    issue: u64,
    who: &AssigneeTarget,
    current_assignees: &[String],
) -> anyhow::Result<()> {
    let n = issue.to_string();
    match who {
        AssigneeTarget::SelfUser => {
            let login = runner
                .run("gh", &["api", "user", "--jq", ".login"])?
                .ok_or_stderr("gh api user")?;
            if login.is_empty() {
                anyhow::bail!("could not resolve the authenticated `gh` user for self-assignment");
            }
            runner
                .run("gh", &["issue", "edit", &n, "--add-assignee", &login])?
                .ok_or_stderr("gh issue edit")?;
        }
        AssigneeTarget::Login(login) => {
            runner
                .run("gh", &["issue", "edit", &n, "--add-assignee", login])?
                .ok_or_stderr("gh issue edit")?;
        }
        AssigneeTarget::None => {
            for login in current_assignees {
                runner
                    .run("gh", &["issue", "edit", &n, "--remove-assignee", login])?
                    .ok_or_stderr("gh issue edit")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::runner::{CommandOutput, CommandRunner};
    use super::*;
    use std::cell::RefCell;

    /// Scripted [`CommandRunner`] recording `(program, args)` per call.
    struct FakeRunner {
        outputs: RefCell<Vec<CommandOutput>>,
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<CommandOutput>) -> Self {
            Self {
                outputs: RefCell::new(outputs),
                calls: RefCell::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.borrow().clone()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
            self.calls.borrow_mut().push((
                program.to_string(),
                args.iter().map(|a| a.to_string()).collect(),
            ));
            let mut outs = self.outputs.borrow_mut();
            if outs.is_empty() {
                anyhow::bail!("FakeRunner exhausted")
            }
            Ok(outs.remove(0))
        }
    }

    fn ok_out(stdout: &str) -> CommandOutput {
        CommandOutput {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn fail_out(stderr: &str) -> CommandOutput {
        CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn gh_list_repo_labels_parses() {
        let json = r#"[{"name":"unicorn:queued","color":"BFD4F2","description":"Queued"}]"#;
        let r = FakeRunner::new(vec![ok_out(json)]);
        let labels = gh_list_repo_labels(&r).expect("parse");
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].name, "unicorn:queued");
        assert_eq!(labels[0].color, "BFD4F2");
        assert_eq!(labels[0].description, "Queued");
        assert_eq!(
            r.calls()[0].1,
            trusty_mpm::core::policy_labels::list_labels_argv(
                None,
                trusty_mpm::core::policy_labels::LABEL_LIST_LIMIT
            )
        );
    }

    #[test]
    fn gh_list_repo_labels_empty_is_empty() {
        let r = FakeRunner::new(vec![ok_out("")]);
        assert!(gh_list_repo_labels(&r).expect("empty ok").is_empty());
    }

    /// Why: #6914 — without an explicit `--limit`, `gh label list` returns its
    /// default 30 labels and never says it truncated, so the seed read a
    /// partial label set as the whole repo.
    #[test]
    fn gh_list_repo_labels_reads_past_the_default_page() {
        let r = FakeRunner::new(vec![ok_out("[]")]);
        gh_list_repo_labels(&r).expect("list");
        let args = &r.calls()[0].1;
        let limit: usize = args
            .iter()
            .position(|a| a == "--limit")
            .and_then(|i| args.get(i + 1))
            .expect("`gh label list` must carry an explicit --limit")
            .parse()
            .expect("--limit is a number");
        assert!(limit > 30, "--limit {limit} must exceed gh's 30-label page");
    }

    /// Why: a page returned exactly as long as the one requested may be
    /// truncated, and seeding against a truncated set is the #6914 failure.
    #[test]
    fn gh_list_repo_labels_rejects_a_truncated_page() {
        let limit = trusty_mpm::core::policy_labels::LABEL_LIST_LIMIT;
        let json: String = format!(
            "[{}]",
            (0..limit)
                .map(|i| format!(r#"{{"name":"l{i}","color":"CCCCCC","description":""}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let r = FakeRunner::new(vec![ok_out(&json)]);
        let err = gh_list_repo_labels(&r).expect_err("a full page is not a complete set");
        assert!(
            err.to_string().contains("truncated"),
            "error must name the truncation; got {err}"
        );
    }

    #[test]
    fn gh_list_repo_labels_errors() {
        let r = FakeRunner::new(vec![fail_out("not a repo")]);
        assert!(gh_list_repo_labels(&r).is_err());
    }

    #[test]
    fn gh_create_label_invokes_create() {
        let r = FakeRunner::new(vec![ok_out("")]);
        let label = RepoLabel::new("unicorn:done", "0075CA", "Done");
        gh_create_label(&r, &label).expect("create");
        assert_eq!(
            r.calls()[0].1,
            vec![
                "label",
                "create",
                "unicorn:done",
                "--color",
                "0075CA",
                "--description",
                "Done"
            ]
        );
    }

    #[test]
    fn gh_create_label_omits_empty_description() {
        let r = FakeRunner::new(vec![ok_out("")]);
        let label = RepoLabel::new("T4", "0075CA", "");
        gh_create_label(&r, &label).expect("create");
        let args = &r.calls()[0].1;
        assert!(!args.iter().any(|a| a == "--description"));
        assert!(args.iter().any(|a| a == "--color"));
    }

    #[test]
    fn gh_add_label_invokes_edit() {
        let r = FakeRunner::new(vec![ok_out("")]);
        gh_add_label(&r, 42, "unicorn:approved").expect("add");
        assert_eq!(
            r.calls()[0].1,
            vec!["issue", "edit", "42", "--add-label", "unicorn:approved"]
        );
    }

    #[test]
    fn gh_remove_label_invokes_edit() {
        let r = FakeRunner::new(vec![ok_out("")]);
        gh_remove_label(&r, 42, "unicorn:queued").expect("remove");
        assert_eq!(
            r.calls()[0].1,
            vec!["issue", "edit", "42", "--remove-label", "unicorn:queued"]
        );
    }

    #[test]
    fn gh_swap_labels_single_call() {
        let r = FakeRunner::new(vec![ok_out("")]);
        gh_swap_labels(&r, 7, "unicorn:approved", "unicorn:queued").expect("swap");
        let calls = r.calls();
        assert_eq!(calls.len(), 1, "swap must be a single gh call");
        assert_eq!(
            calls[0].1,
            vec![
                "issue",
                "edit",
                "7",
                "--add-label",
                "unicorn:approved",
                "--remove-label",
                "unicorn:queued"
            ]
        );
    }

    #[test]
    fn gh_set_assignee_self() {
        // First call resolves the login, second adds it.
        let r = FakeRunner::new(vec![ok_out("octocat"), ok_out("")]);
        gh_set_assignee(&r, 7, &AssigneeTarget::SelfUser, &[]).expect("self");
        let calls = r.calls();
        assert_eq!(calls[0].1, vec!["api", "user", "--jq", ".login"]);
        assert_eq!(
            calls[1].1,
            vec!["issue", "edit", "7", "--add-assignee", "octocat"]
        );
    }

    #[test]
    fn gh_set_assignee_login() {
        let r = FakeRunner::new(vec![ok_out("")]);
        gh_set_assignee(&r, 7, &AssigneeTarget::Login("alice".to_string()), &[]).expect("login");
        assert_eq!(
            r.calls()[0].1,
            vec!["issue", "edit", "7", "--add-assignee", "alice"]
        );
    }

    #[test]
    fn gh_set_assignee_none_removes_each() {
        let r = FakeRunner::new(vec![ok_out(""), ok_out("")]);
        let current = vec!["alice".to_string(), "bob".to_string()];
        gh_set_assignee(&r, 7, &AssigneeTarget::None, &current).expect("none");
        let calls = r.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].1,
            vec!["issue", "edit", "7", "--remove-assignee", "alice"]
        );
        assert_eq!(
            calls[1].1,
            vec!["issue", "edit", "7", "--remove-assignee", "bob"]
        );
    }

    #[test]
    fn gh_set_assignee_none_empty_is_noop() {
        let r = FakeRunner::new(vec![]);
        gh_set_assignee(&r, 7, &AssigneeTarget::None, &[]).expect("noop");
        assert!(
            r.calls().is_empty(),
            "empty assignee set must make zero gh calls"
        );
    }
}
