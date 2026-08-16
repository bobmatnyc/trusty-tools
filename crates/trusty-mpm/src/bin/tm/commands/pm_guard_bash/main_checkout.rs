//! Whole-tree-destructive git commands, denied in a project's main checkout
//! ([ADR-0037](../../../../../../../docs/adr/0037-pm-placement-precedence-main-checkout-by-default.md)).
//!
//! Why: ADR-0037's write-restriction amendment makes the main checkout
//! read-only except for documents and configuration, and records that the
//! enforcement "is mechanical and NOT YET BUILT". This module is that
//! mechanism for the destructive-git half. The incident it is built from: a
//! `local-ops` agent dispatched by a DIFFERENT session ran `git checkout <sha>
//! -- .`, `git reset HEAD`, then `git clean -fdx -- crates/ docs/` in the
//! shared main checkout — switching the branch under another live session,
//! discarding uncommitted work, and sweeping build caches across seven UI
//! packages and two crate `target/` directories. Its dispatch brief said that
//! checkout was off-limits. A prohibition in a brief is not enforcement, and a
//! brief is invisible to agents from other sessions.
//!
//! What: [`evaluate_main_checkout_destructive_command`] denies a `Bash` call
//! when BOTH halves hold — the command names a whole-tree-destructive git verb
//! ([`is_whole_tree_destructive`]) AND the directory that verb would act on
//! ([`git_verb_target_dir`]) is a main checkout
//! ([`trusty_mpm::core::project_aliases::is_main_checkout`]). Anything under
//! `.claude/worktrees/**` or `.worktrees/**` is a worktree, never a main
//! checkout, so delegated work stays fully writable — that is where it is
//! supposed to happen.
//!
//! **It pierces the subagent exemptions, deliberately.** Like the
//! `git worktree add` denylist (#3977) and the fan-out denial (#4784), this is
//! called from `pm_guard()` BEFORE Guard 1's and Guard 4's early returns —
//! see that call site's comment. The incident was an AGENT, dispatched by
//! another session, so a PM-only rule would be a no-op for exactly the case it
//! exists for. The owner's 2026-07-26 ruling on #3973 permits this only for a
//! SAFETY rule, and the scope here is drawn to match: irreversible destruction
//! of another session's uncommitted work, nothing wider. It is not a role rule
//! and not a style rule — a subagent keeps every other tool and every
//! non-destructive git verb, in the main checkout and everywhere else.
//!
//! **Fail-open / fail-closed, decided per branch.** The guard consults NO
//! daemon, so it has no unreachable-daemon arm to fail open through — the
//! classification is filesystem-lexical and answers the same way whether or
//! not anything else on the machine is running
//! (`pm_guard_denies_main_checkout_destructive_git_with_the_daemon_unreachable`
//! pins that). The registry was considered and rejected as the authority:
//! `DaemonState::projects` is an in-memory map populated only by an explicit
//! `POST /projects` (or the Telegram `/setproj` command), never from session
//! history, so gating on "registered" would make the guard silently inert on
//! any machine where nobody had registered the path by hand — and the
//! persisted `Project` record carries a `repo_url`, no local path, so it
//! cannot answer the question at all. The remaining indeterminate arms all
//! resolve to ALLOW, and each one is a case where nothing was positively
//! identified: a directory with no `.git` ancestor is not a checkout; a
//! segment `shlex` cannot split (unbalanced quotes) yields no argv to
//! classify; a git verb that is not in the destructive table is not this
//! rule's business. The guard denies only on positive evidence of both halves.
//!
//! Residual bypasses, stated rather than hidden — each is the same shape the
//! sibling `evaluate_worktree_add_command` documents, because both reuse the
//! same lexical path resolution: a `cd`/`-C` argument built from a shell
//! variable or a command substitution is not resolved; a symlink into a
//! checkout is not followed; a destructive verb hidden inside a `$(…)`
//! substitution is not scanned. And the Guard 2/3 operator escape hatches
//! (`TRUSTY_MPM_DISABLE_HOOKS`, `TRUSTY_MPM_PM_UNRESTRICTED`) lift this rule
//! along with every other, which remains tracked as #3981.
//!
//! **The HEAD-moving verbs decide with the daemon, not alone (ADR-0048
//! decision 10).** [`main_checkout_head_move`] classifies `git pull`, `merge`
//! and `rebase` the same lexical way, but it returns the verb and the directory
//! instead of a reason: the caller then asks the daemon's directory-keyed
//! `live_shared_tree_writers` who else is writing there and denies only when
//! the answer is non-empty. That second half is what makes the rule safe to
//! state by verb alone. A solo session updating its own checkout is ordinary
//! work and stays allowed; only a HEAD move under another live writer is
//! refused, and a daemon that cannot answer answers "nobody here", so this
//! branch fails open exactly like the #4480 guard it borrows the query from.
//!
//! **Residuals specific to the HEAD-move rule, stated rather than hidden
//! (#5769).** Four of them, each an ALLOW where a deny might be expected:
//!
//! * `--git-dir=` and `--work-tree=` are skipped when resolving the subcommand
//!   name ([`shell_lex::git_subcommand`]) but are never resolved into a target
//!   directory — only `cd` and `git -C` are. So
//!   `git --git-dir=/repo/.git --work-tree=/repo pull`, run from anywhere,
//!   resolves the running directory rather than `/repo` and is allowed when the
//!   running directory is not itself a shared checkout. Closing it means
//!   teaching [`git_verb_target_dir`] a third override, which the destructive
//!   and commit rules would inherit — a wider change than this rule.
//! * The deny quotes the daemon's delegation records, and a record can be wrong
//!   in the ALLOW direction as well as the deny one. A grant whose isolation
//!   POST failed (that path fails open) leaves a genuinely unisolated record for
//!   an agent that does have a worktree, and the deny then names it. The text
//!   attributes the claim for exactly this reason — see
//!   [`head_move_deny_reason`].
//! * The query is keyed by directory, and this rule asks two — the resolved
//!   target and its checkout root. A record stamped at some THIRD directory
//!   inside the same checkout (a hook that ran from another subdirectory) is
//!   still invisible.
//! * [`IN_PROGRESS_CONTROL_FLAGS`] is matched on the first tail token only, so a
//!   real in-progress control that git would accept in a later position — none
//!   exists today — would be denied rather than exempted. That is the fail
//!   direction this carve-out can afford; the reverse cost a whole `git merge`
//!   its deny.
//!
//! **The commit rule reads the index, and is the one branch here that spawns a
//! subprocess (ADR-0049).** Everything above is filesystem-lexical, and the
//! commit rule was too until the owner ruled that documents may be committed in
//! a main checkout. Which files a commit will land is a question only git can
//! answer, so [`evaluate_main_checkout_commit_command`] runs
//! [`trusty_mpm::core::staged_paths::staged_paths`] — but only after both
//! lexical halves already match, so ordinary Bash traffic never pays for it,
//! the same discipline the HEAD-move rule uses before its daemon call. A
//! documents-only staged set then takes the same live-writer query, because a
//! commit moves the shared HEAD exactly as far as a pull does. Every other
//! arm — a staged source file, an unreadable index, an empty index, and every
//! flag form that commits content the index does not hold — keeps the
//! unconditional deny ADR-0048 decision 4 shipped, so this rule can only turn a
//! deny into an allow and never the reverse.
//!
//! Test: `is_whole_tree_destructive_*`, `destructive_target_dir_*`,
//! `commit_target_dir_*`, `classify_staged_commit_*`,
//! `commit_flags_leave_the_index_authoritative_*`, `main_checkout_head_move_*`,
//! `starts_a_head_move_*` below;
//! `is_main_checkout_*` in `trusty_mpm::core::project_aliases`;
//! `pm_guard_denies_the_incident_commands_in_a_main_checkout` and siblings in
//! `tests/tm_hook_pm_guard.rs` run the stdin→decision→stdout path through the
//! real binary, including the subagent-marked payload.

use std::path::{Path, PathBuf};

use trusty_mpm::core::project_aliases::{is_main_checkout, main_checkout_root};
use trusty_mpm::core::staged_paths::staged_paths;

use super::{PathEnv, git_dash_c_override, resolve_target_path, split_shell_segments};
use crate::commands::hook_rewrite::first_command_token;
use crate::commands::pm_guard::is_source_code_path;
use crate::commands::pm_guard_bash::shell_lex;

/// Deny a whole-tree-destructive git command aimed at a main checkout.
///
/// Why: the one entry point `pm_guard` calls. It is kept to the wrapper shape
/// the sibling worktree guard uses so the process-environment read
/// ([`PathEnv::from_process`]) happens in exactly one place and the policy
/// underneath stays testable without it.
/// What: `Some(reason)` — naming the verb, the directory, and the remedy —
/// when [`git_verb_target_dir`] finds a destructive verb whose target
/// directory [`is_main_checkout`]; `None` (ALLOW) otherwise.
/// Test: the two halves are covered separately (see the module doc); the
/// composition runs end to end in `tests/tm_hook_pm_guard.rs`.
pub(crate) fn evaluate_main_checkout_destructive_command(
    command: &str,
    cwd: &Path,
) -> Option<String> {
    let (verb, target) = git_verb_target_dir(
        command,
        cwd,
        &PathEnv::from_process(),
        is_whole_tree_destructive,
    )?;
    is_main_checkout(&target).then(|| deny_reason(&verb, &target))
}

/// What a `git commit` aimed at a main checkout is allowed to do (ADR-0049).
///
/// Why: the rule stopped being decidable from the verb alone when the owner
/// ruled that documents may be committed there, so the caller needs two
/// answers, not one. A source commit is refused outright; a docs-only commit is
/// permitted unless the daemon reports another live writer sharing the HEAD,
/// which only the caller can ask.
/// Test: `evaluate_main_checkout_commit_*`.
pub(crate) enum CommitVerdict {
    /// Refuse, with the reason already built.
    Deny(String),
    /// Documents and configuration only. Permit unless another live writer
    /// holds this HEAD; `dirs` are the query keys, checkout root first then the
    /// command's own directory — the same order and the same reason as the
    /// HEAD-move rule (#5769: `tm hook` stamps a record from its own process
    /// directory, and the root is the spelling a record most often carries).
    DocsOnly { root: PathBuf, dirs: [PathBuf; 2] },
}

/// Classify a `git commit` aimed at a main checkout (ADR-0048, amended by
/// ADR-0049).
///
/// Why: ADR-0044 makes the main checkout read-only apart from documents and
/// configuration, and a commit is the step that makes a write permanent on a
/// branch other sessions are standing on. The reported incident is exactly
/// this: commit `f1da7bce` landed on `fix/1646-drive-query-v2-migration`, a
/// branch belonging to a different workstream, because three sessions shared
/// one checkout and one of them committed on whichever branch HEAD happened to
/// point at. Blocking the source write alone would not have stopped it — the
/// files were already there.
///
/// ADR-0049 then found the other half of that rule stranded work: documents are
/// WRITABLE in a main checkout and a commit was denied unconditionally, so a
/// doc could be written where it could never be landed. The gate now reads what
/// is STAGED rather than the verb — the verb is `git commit` either way, and
/// only the content tells a safe commit from an unsafe one.
///
/// What: `None` (ALLOW, this rule's business does not arise) unless a
/// `git commit` segment's effective directory belongs to a main checkout. When
/// it does, the command must first be a LONE commit
/// ([`command_is_a_lone_commit`]) — one index read describes one commit and
/// only when nothing between the read and the commit can restage — and
/// [`classify_staged_commit`] then decides between [`CommitVerdict::Deny`] and
/// [`CommitVerdict::DocsOnly`]. The staged set is read only once both lexical
/// halves match, so ordinary Bash traffic never pays for the subprocess.
///
/// **The direction this can move a verdict is one-way, and the property is
/// stated per COMMAND rather than per arm (#5788 review).** Before ADR-0049
/// every `git commit` reaching this function was denied. After it, a command is
/// allowed only when it is a lone commit whose staged set is positively
/// evidenced as documents and configuration; every other command still denies.
/// The first cut stated this per-arm — true of [`classify_staged_commit`] in
/// isolation, false of the composed rule, and the gap between the two was the
/// `git commit -m docs && git commit -a -m src` exploit. Read this way it holds
/// for the whole entry point, which is what keeps it off ordinary work without
/// a #5356-shaped false-deny risk.
///
/// Scope, stated because the near neighbours are tempting: `git checkout
/// <branch>` and `git switch <branch>` are NOT covered here even though
/// switching a branch under another session is part of the same incident.
/// Their safe and unsafe forms differ by argument rather than by verb and a
/// loose rule there costs a false deny on ordinary work — the failure #5356 was
/// filed for. `pull`, `merge` and `rebase` left that family in ADR-0048
/// decision 10 and are handled by [`main_checkout_head_move`], which needs no
/// argument analysis: none of the three has a form that leaves HEAD alone.
/// `git add` is not covered by any of them and deliberately so: it writes the
/// index and moves no ref, so it creates none of the shared-HEAD hazard these
/// rules exist for (ADR-0049 decision 4). It is still refused when CHAINED to a
/// commit, which is a statement about the index read above, not about `git add`.
/// Test: `commit_target_dir_*`, `evaluate_main_checkout_commit_*`,
/// `command_is_a_lone_commit_*`.
pub(crate) fn evaluate_main_checkout_commit_command(
    command: &str,
    cwd: &Path,
) -> Option<CommitVerdict> {
    let (_, target, tail) =
        git_verb_target_dir_with_tail(command, cwd, &PathEnv::from_process(), |verb, _| {
            verb == "commit"
        })?;
    // `main_checkout_root` rather than `is_main_checkout` for the reason #5769
    // gave the HEAD-move rule: `cd crates/foo && git commit` resolves a
    // subdirectory that shares the checkout's HEAD, and the writer query has to
    // be keyed on a directory a delegation record can actually carry.
    let root = main_checkout_root(&target)?;
    // #5788 review, CRITICAL 1: one index read authorises at most one commit,
    // and only when nothing between the read and that commit can change the
    // index. Asked before the staged set is even read, because a composition
    // this cannot vouch for is refused whatever is staged.
    if !command_is_a_lone_commit(command) {
        return Some(CommitVerdict::Deny(composed_commit_deny_reason(&root)));
    }
    Some(classify_staged_commit(
        &tail,
        staged_paths(&target),
        &target,
        root,
    ))
}

/// Whether `command` is a single `git commit` and nothing that could restage.
///
/// Why (#5788 review, CRITICAL 1): the staged set is read once, at hook time,
/// and describes the index at that instant. A composition breaks that in two
/// ways, both demonstrated live against the first cut of this rule.
/// `git commit -m docs && git add -A && git commit -a -m src` was ALLOWED — the
/// walker returned on the FIRST commit segment, so the docs-only staged set
/// licensed a source commit two segments later. And `git add -A &&
/// git commit -m docs` has one commit segment but restages before it runs, so
/// the read describes an index the commit never sees. Both were denied before
/// ADR-0049 and must stay denied.
///
/// The sibling destructive rule scans every segment and so has neither problem;
/// this one cannot fix it the same way, because scanning further segments still
/// would not tell it what the index holds by the time they run. The only
/// defensible answer is to refuse the composition.
///
/// What: `true` when every non-empty segment is either a `cd` — which moves no
/// files and touches no index — or THE one `git commit`. Any second commit, any
/// other command, and any segment `shlex` cannot split all return `false`,
/// which the caller turns into the pre-ADR-0049 deny. The remedy is one extra
/// tool call, and [`composed_commit_deny_reason`] names it.
/// Test: `command_is_a_lone_commit_accepts_a_commit_and_cd`,
/// `command_is_a_lone_commit_rejects_a_second_commit`,
/// `command_is_a_lone_commit_rejects_anything_that_can_restage`.
fn command_is_a_lone_commit(command: &str) -> bool {
    let mut commits = 0;
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if first_command_token(trimmed) == Some("cd") {
            continue;
        }
        if shell_lex::git_subcommand(trimmed).as_deref() != Some("commit") {
            return false;
        }
        commits += 1;
    }
    commits == 1
}

/// The verdict for a commit already known to be aimed at a main checkout.
///
/// Why: split from [`evaluate_main_checkout_commit_command`] so the whole
/// policy is a pure function of the argv tail and the staged set. Every arm
/// below is a failure arm except one, and a test that had to build a real
/// repository to reach each of them would exercise git rather than the policy.
/// What: [`CommitVerdict::DocsOnly`] only when BOTH halves are positively
/// evidenced — the flags leave the index authoritative
/// ([`commit_flags_leave_the_index_authoritative`]) AND the staged set is
/// non-empty and contains no source file. Everything else is a
/// [`CommitVerdict::Deny`], which is the pre-ADR-0049 behaviour:
///
/// * a staged set containing source, mixed or not, names the offending files;
/// * `None` — git could not be asked — is UNKNOWN, not empty;
/// * an empty staged set is not evidence of a docs commit, and `git commit`
///   with nothing staged is either an error or one of the forms below;
/// * a flag that commits content the index does not hold (`-a`, `--amend`,
///   `--include`, `--only`, a bare pathspec) makes the staged set a lie about
///   what will land, so the staged set cannot license it.
///
/// Test: `classify_staged_commit_permits_documents_and_configuration`,
/// `classify_staged_commit_denies_source_and_names_it`,
/// `classify_staged_commit_denies_a_mixed_set_and_names_only_the_source`,
/// `classify_staged_commit_denies_an_unreadable_or_empty_index`,
/// `classify_staged_commit_denies_the_forms_the_index_does_not_describe`.
fn classify_staged_commit(
    tail: &[String],
    staged: Option<Vec<String>>,
    target: &Path,
    root: PathBuf,
) -> CommitVerdict {
    if !commit_flags_leave_the_index_authoritative(tail) {
        return CommitVerdict::Deny(commit_deny_reason(&root));
    }
    let Some(staged) = staged.filter(|s| !s.is_empty()) else {
        return CommitVerdict::Deny(commit_deny_reason(&root));
    };
    let source: Vec<&str> = staged
        .iter()
        .map(String::as_str)
        .filter(|p| is_source_code_path(p))
        .collect();
    if source.is_empty() {
        CommitVerdict::DocsOnly {
            dirs: [root.clone(), target.to_path_buf()],
            root,
        }
    } else {
        CommitVerdict::Deny(staged_source_deny_reason(&root, &source))
    }
}

/// `git commit` flags under which the index IS what the commit will contain.
///
/// Why: the docs-only carve-out rests entirely on the staged set describing the
/// commit, and four flag families break that. `-a`/`--all` stages every tracked
/// modification at commit time; `-i`/`--include` adds paths to the index first;
/// `-o`/`--only` commits the named paths from the working tree instead of the
/// index; `--amend` reuses the previous commit's tree, whose contents this
/// process never looked at. A bare pathspec implies `--only`. Under any of
/// them the staged set answers a different question than the one being asked.
/// What: an ALLOWLIST, walked left to right, so an unrecognised token — a
/// pathspec, a short cluster, a flag added by a future git — is `false`. That
/// direction is safe by construction here: `false` returns the pre-ADR-0049
/// deny, which is what the caller did for every commit before this rule
/// existed. A denylist would have the opposite bias and would license a commit
/// on a flag nobody had heard of.
///
/// `-S`/`--gpg-sign` sit in the no-value list because git accepts their
/// optional key id only glued (`-S<keyid>`, `--gpg-sign=<keyid>`), never as a
/// following token.
/// Test: `commit_flags_leave_the_index_authoritative_accepts_message_forms`,
/// `commit_flags_leave_the_index_authoritative_rejects_content_flags`,
/// `commit_flags_leave_the_index_authoritative_rejects_a_pathspec`.
fn commit_flags_leave_the_index_authoritative(tail: &[String]) -> bool {
    const NO_VALUE: &[&str] = &[
        "-s",
        "--signoff",
        "--no-signoff",
        "-n",
        "--no-verify",
        "--verify",
        "-q",
        "--quiet",
        "-v",
        "--verbose",
        "-e",
        "--edit",
        "--no-edit",
        "--allow-empty",
        "--allow-empty-message",
        "--status",
        "--no-status",
        "--no-post-rewrite",
        "-S",
        "--gpg-sign",
        "--no-gpg-sign",
    ];
    const WITH_VALUE: &[&str] = &[
        "-m",
        "--message",
        "-F",
        "--file",
        "--author",
        "--date",
        "--cleanup",
        "--trailer",
    ];

    let mut expecting_value = false;
    for token in tail {
        if expecting_value {
            expecting_value = false;
            continue;
        }
        // `--flag=value` and `-S<keyid>` carry their value glued on.
        let name = match token.split_once('=') {
            Some((head, _)) if head.starts_with("--") => head,
            _ if token.starts_with("-S") => "-S",
            _ => token.as_str(),
        };
        if NO_VALUE.contains(&name) {
            continue;
        }
        if WITH_VALUE.contains(&name) {
            expecting_value = name == token.as_str();
            continue;
        }
        return false;
    }
    true
}

/// The verb and directory of a HEAD-moving git command aimed at a main
/// checkout (ADR-0048 decision 10).
///
/// Why: `pull`, `merge` and `rebase` move HEAD and write the working tree of
/// the directory they run in. In a worktree that HEAD belongs to the one
/// session that owns it, so the move races nothing. In a main checkout the HEAD
/// is shared, and moving it changes the ground another session's uncommitted
/// work is sitting on — with no error at any step, the same silence the commit
/// rule above exists for.
/// What: `Some((verb, directory, checkout_root))` when a segment
/// [`starts_a_head_move`] and the directory it would act on belongs to a main
/// checkout. It returns those rather than a reason because the deny is not
/// decided here: the caller asks the daemon who else is live and denies only on
/// a positive answer. See [`head_move_deny_reason`].
///
/// **Why two directories and not one (#5769).** `directory` is what the command
/// resolves to through `cd` and `git -C`; `checkout_root` is the main checkout
/// that directory sits in. They differ whenever the command runs from a
/// subdirectory, and the delegation records the caller queries are stamped from
/// `tm hook`'s own process directory, which may be either. Both name one HEAD,
/// so the caller asks both — and the test that the directory belongs to a
/// checkout is now [`main_checkout_root`], not [`is_main_checkout`], which
/// closes a second hole in the same place: `cd crates/foo && git pull` resolved
/// a subdirectory, `is_main_checkout` on it was still true, and the query then
/// keyed a directory no record could match.
/// Test: `main_checkout_head_move_*`, and end to end in
/// `tests/tm_hook_pm_guard.rs`.
pub(crate) fn main_checkout_head_move(
    command: &str,
    cwd: &Path,
) -> Option<(String, PathBuf, PathBuf)> {
    let (verb, target) =
        git_verb_target_dir(command, cwd, &PathEnv::from_process(), starts_a_head_move)?;
    let root = main_checkout_root(&target)?;
    Some((verb, target, root))
}

/// Flags that operate on an operation already in progress rather than start a
/// new one.
///
/// Why: see [`starts_a_head_move`] — a deny on these would strand a session
/// mid-rebase with no way out, which is the retried-differently-and-worse
/// failure ADR-0048 decision 6 exists to prevent.
const IN_PROGRESS_CONTROL_FLAGS: &[&str] = &[
    "--abort",
    "--quit",
    "--continue",
    "--skip",
    "--edit-todo",
    "--show-current-patch",
];

/// Whether a git subcommand, given its argv tail, would START moving HEAD.
///
/// Why: this is the second false-positive boundary in this module, and the
/// reason ADR-0048 originally left these three verbs uncovered is that a loose
/// rule here costs false denies on ordinary work (#5356). What makes a
/// verb-only rule safe for exactly these three is that none of them has a form
/// that leaves HEAD and the working tree alone: `git pull` is a fetch plus a
/// merge or fast-forward, and `merge`/`rebase` rewrite the current branch.
/// There is no pathspec-vs-ref ambiguity to resolve, which is what keeps
/// `checkout` and `switch` out of this rule and in the doc comment above.
/// Neighbouring verbs are separate subcommand names (`merge-base`,
/// `merge-tree`, `rebase--helper`) and never match.
///
/// The one carve-out is [`IN_PROGRESS_CONTROL_FLAGS`]. Those resolve a state
/// that already exists — the operation started before this call — and denying
/// them would leave the shared checkout parked mid-rebase, which is worse for
/// every other session in it than letting the operation finish or unwind. The
/// deny that matters is the one that stops the move from starting.
///
/// The carve-out is matched POSITIONALLY, on the first tail token only (#5769).
/// Scanning the whole tail exempted any command carrying one of those strings
/// anywhere in it, so `git merge -m "--continue" origin/main` — a real merge,
/// with the flag as a commit message — passed straight through. Git itself
/// accepts these only as the first argument, so the narrower match is also the
/// accurate one.
/// Test: `starts_a_head_move_covers_the_three_verbs`,
/// `starts_a_head_move_allows_in_progress_control`,
/// `starts_a_head_move_matches_in_progress_control_positionally`,
/// `starts_a_head_move_ignores_everything_else`.
fn starts_a_head_move(subcommand: &str, tail: &[String]) -> bool {
    matches!(subcommand, "pull" | "merge" | "rebase")
        && !tail
            .first()
            .is_some_and(|t| IN_PROGRESS_CONTROL_FLAGS.contains(&t.as_str()))
}

/// Build the deny message for a blocked HEAD move.
///
/// Why: as [`deny_reason`] and [`commit_deny_reason`] — ADR-0048 decision 6
/// requires every deny to name what to do instead. This one has two remedies to
/// offer rather than one, and the cheap one comes first: most calls that reach
/// here want updated refs, and `git fetch` gives exactly that with no HEAD move
/// (ADR-0048 decision 9), so a reader who only needed `origin/main` refreshed is
/// unblocked without provisioning anything.
/// What: names the verb, the directory, the sibling agents the daemon reports,
/// `git fetch` as the ref-updating remedy, a worktree as the merge-or-rebase
/// remedy, and the forms this rule never blocks.
///
/// **It attributes, rather than asserts (#5769).** The text used to state as
/// fact that another agent "is already writing there without a worktree of its
/// own". This process cannot know that — it knows only what the daemon's
/// delegation records say, and a record can outlive its agent (nothing closes
/// one whose `SubagentStop` never arrives, for `RUNNING_STALE_AFTER_SECS`) or
/// misdescribe it (a granted dispatch whose isolation POST failed is recorded
/// unisolated, fail-open). Saying whose claim it is keeps the deny honest and
/// tells the reader where to look when it is wrong.
/// Test: `head_move_deny_reason_names_the_verb_the_path_and_both_remedies`,
/// `head_move_deny_reason_attributes_the_claim_to_the_daemon`.
pub(crate) fn head_move_deny_reason(verb: &str, target: &Path, live: &[String]) -> String {
    let mut names: Vec<&str> = live.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    format!(
        "HEAD-moving git command denied in a shared main checkout (ADR-0048): `git {verb}` moves \
         HEAD and writes the working tree of {}, and the daemon's delegation records name {} as \
         running there with no worktree of its own — possibly dispatched by a different session \
         standing in the same directory. Moving HEAD under a live writer changes the branch its \
         uncommitted work sits on, and git reports no error when it happens. If you \
         believe that record is stale — the agent finished without its stop signal reaching the \
         daemon — report it to the PM rather than retrying this command. If you need the remote \
         refs updated, run \
         `git fetch` instead: it writes only `refs/remotes/origin/*` and never touches HEAD or \
         the working tree, so it is never blocked here — then branch and diff against \
         `origin/main` rather than local `main`, which only a pull moves. If you need the merge \
         or rebase itself, do it in a worktree: ask the PM to re-dispatch you with \
         `isolation: \"worktree\"`, or `git worktree add .claude/worktrees/<name>`. Read-only git \
         (`status`, `log`, `diff`), `git fetch`, `git rebase --abort`/`--continue`, \
         `git merge --abort`, and everything under `.claude/worktrees/**` are never blocked by \
         this rule.",
        target.display(),
        names.join(", ")
    )
}

/// Build the deny message for a blocked commit whose content this rule could
/// not read as documents.
///
/// Why: as [`deny_reason`] — a bare refusal is retried differently and worse.
/// This one has to be clear that the work is not lost and does not need
/// redoing, only moved, because the reflex on a blocked commit is to reach for
/// `git stash` or a second `-m` attempt. Since ADR-0049 it also has to name the
/// docs-only carve-out, because the reader who lands here after editing one
/// `.md` file has usually hit the `-a`/`--amend`/nothing-staged arm and the fix
/// is one `git add`, not a worktree.
/// Test: `commit_deny_reason_names_the_path_and_the_remedy`.
fn commit_deny_reason(target: &Path) -> String {
    format!(
        "Commit denied in a main checkout (ADR-0044): {} is a project's main checkout, which is \
         read-only apart from documents and configuration, and other sessions are standing on \
         this same git HEAD. A commit here lands on whichever branch HEAD currently points at — \
         the reported failure is a commit landing on another workstream's branch and the branch \
         it belonged to left empty, with no error at any step. A DOCUMENTS-ONLY commit is \
         permitted here (ADR-0049), but only when the staged set says so: stage the documents \
         explicitly (`git add -- <paths>`) and commit with a plain `git commit -m …`. This call \
         was refused because the staged set does not describe what it would commit — nothing is \
         staged, the index could not be read, or the command carries `-a`, `--amend`, \
         `--include`, `--only`, or a pathspec, each of which commits content the index does not \
         hold. For source changes, commit from a worktree instead: ask the PM to re-dispatch you \
         with `isolation: \"worktree\"`, or move the work with \
         `git worktree add .claude/worktrees/<name>` and commit there. Nothing is lost — the \
         changes are still in the tree. Read-only git (`status`, `log`, `diff`), `git add`, and \
         everything under `.claude/worktrees/**` are never blocked by this rule.",
        target.display()
    )
}

/// Build the deny message for a commit this rule cannot read the index for,
/// because the command composes it with something else.
///
/// Why (#5788 review, CRITICAL 1): the reader has staged documents and is being
/// refused anyway, so the text has to name the composition as the cause — not
/// the content — or the retry is to unstage something that was never the
/// problem. The remedy is one extra tool call and the message says exactly
/// which one.
/// Test: `composed_commit_deny_reason_names_the_composition_and_the_remedy`.
fn composed_commit_deny_reason(target: &Path) -> String {
    format!(
        "Composed commit denied in a main checkout (ADR-0049): {} is a project's main checkout, \
         where a documents-only commit is permitted — but only when the guard can see what the \
         commit will contain. It reads the index once, before this command runs, and that reading \
         describes exactly one commit with nothing in between: a second `git commit`, a \
         `git add`, or any other command in the same call can change the index after the reading \
         and before the commit, so the reading would be describing a commit that never happens. \
         Split the call: stage in one Bash call (`git add -- <paths>`), then run \
         `git commit -m …` as its own call with nothing chained to it. `cd` is the one exception \
         and may be chained, since it touches no index. Nothing is lost — the changes are still \
         in the tree, and `git add` is never blocked by this rule.",
        target.display()
    )
}

/// Build the deny message for a commit whose staged set contains source.
///
/// Why: ADR-0049 decision 2 — a mixed staged set fails safe, and a refusal that
/// does not say WHICH file made it unsafe leaves the reader unstaging by
/// guesswork. Naming them turns the remedy into a mechanical
/// `git restore --staged` of a listed path.
/// What: names the checkout, every staged source path, and the two ways
/// forward — unstage the source and commit the documents here, or move the
/// whole change to a worktree.
/// Test: `staged_source_deny_reason_names_every_source_file`.
fn staged_source_deny_reason(target: &Path, source: &[&str]) -> String {
    let mut names: Vec<&str> = source.to_vec();
    names.sort_unstable();
    names.dedup();
    format!(
        "Source commit denied in a main checkout (ADR-0044, amended by ADR-0049): {} is a \
         project's main checkout, which is read-only apart from documents and configuration, and \
         other sessions are standing on this same git HEAD. Documents may be committed here; \
         these staged paths are source and may not be: {}. A mixed staged set is refused as a \
         whole rather than split, because a commit is one object and half of it cannot be sent \
         elsewhere. Two ways forward. To land the documents from here, unstage the source \
         (`git restore --staged -- <paths above>`) and commit again. To land the source, do it \
         in a worktree: ask the PM to re-dispatch you with `isolation: \"worktree\"`, or \
         `git worktree add .claude/worktrees/<name>` and commit there. Nothing is lost — the \
         changes are still in the tree, and `git add` is never blocked by this rule.",
        target.display(),
        names.join(", ")
    )
}

/// Build the deny message for a documents-only commit refused because another
/// live writer shares the HEAD.
///
/// Why: ADR-0049 decision 3 gives a docs commit the same concurrency test
/// ADR-0048 decision 10 gives `pull`/`merge`/`rebase`, because the hazard is
/// the same one — a commit MOVES HEAD, and the branch it moves is whichever one
/// the other session's uncommitted work is sitting on. The reader has to be
/// told that the content was fine and the timing was not, or the obvious retry
/// is to unstage the document that was never the problem.
/// What: names the checkout, the writers the daemon reports, and the remedy.
/// It attributes the claim to the daemon's records for the reason
/// [`head_move_deny_reason`] gives: a record can outlive its agent.
/// Test: `docs_commit_deny_reason_names_the_writer_and_attributes_the_claim`.
pub(crate) fn docs_commit_deny_reason(target: &Path, live: &[String]) -> String {
    let mut names: Vec<&str> = live.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    format!(
        "Documents-only commit denied in a SHARED main checkout (ADR-0049): the staged set is \
         documents and configuration, which may be committed in {} — but a commit moves HEAD, \
         and the daemon's delegation records name {} as running there with no worktree of its \
         own, possibly dispatched by a different session standing in the same directory. Moving \
         HEAD under a live writer changes the branch its uncommitted work sits on, and git \
         reports no error when it happens. The content is not the problem here and unstaging it \
         will not help. Wait for that agent to finish and commit then, or commit from a worktree: \
         ask the PM to re-dispatch you with `isolation: \"worktree\"`, or \
         `git worktree add .claude/worktrees/<name>`. If you believe that record is stale — the \
         agent finished without its stop signal reaching the daemon — report it to the PM rather \
         than retrying this command. Nothing is lost; the staged changes are still in the tree.",
        target.display(),
        names.join(", ")
    )
}

/// Build the deny message.
///
/// Why: a bare refusal leaves the model guessing and it retries the identical
/// call. The text names what was blocked, WHERE (the reader's next question is
/// always "which tree?"), why the directory is special, and the one remedy
/// that always works — do the work in a worktree. Built per call rather than
/// kept as a constant because naming the actual directory is most of its value.
/// Test: `deny_reason_names_the_verb_the_path_and_the_remedy`.
fn deny_reason(verb: &str, target: &Path) -> String {
    format!(
        "Destructive git command denied in a main checkout (ADR-0037): `git {verb}` would \
         overwrite or delete uncommitted work in {}, which is a project's main checkout rather \
         than a worktree. Another session may be standing in that directory, and what this \
         removes is not in git — it cannot be recovered. ADR-0037 makes the main checkout \
         read-only apart from documents and configuration. Do this work in a worktree instead: \
         `git worktree add .claude/worktrees/<name>`, or ask the PM to re-dispatch you with \
         `isolation: \"worktree\"`. Read-only git (`status`, `log`, `diff`), branch creation \
         (`checkout -b`), and a plain `git reset` are never blocked by this rule.",
        target.display()
    )
}

/// The directory the first git segment matching `matches` would act on, with
/// that segment's verb.
///
/// Why: split from the filesystem classification so the whole text-and-path
/// half — which is where a false positive would come from — is a pure function
/// with no filesystem or environment of its own. The classifier is a parameter
/// rather than hardcoded so the destructive-verb rule and the commit rule share
/// one walker: `cd` tracking, `git -C` resolution, and segment splitting are
/// the parts a second copy would drift on, and they are identical for both.
/// What: walks the composition segments (reusing [`split_shell_segments`], so
/// `true && git reset --hard` is classified on its second segment), tracks the
/// effective working directory across `cd` segments and a leading `git -C`,
/// and returns the first segment whose `(verb, argv-tail)` satisfies `matches`.
/// `None` when no segment qualifies — including a segment `shlex` cannot split,
/// which yields no argv to classify.
/// Test: `destructive_target_dir_*`, `commit_target_dir_*`.
fn git_verb_target_dir(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
    matches: impl Fn(&str, &[String]) -> bool,
) -> Option<(String, PathBuf)> {
    git_verb_target_dir_with_tail(command, cwd, env, matches).map(|(verb, dir, _)| (verb, dir))
}

/// [`git_verb_target_dir`], also handing back the matched segment's argv tail.
///
/// Why: the commit rule decides on the flags as well as the directory
/// (ADR-0049 — `--amend` and `-a` commit content the index does not describe),
/// and the tail was already computed here to run `matches`. Returning it beats
/// a second lexer in the commit rule, which is the copy that would drift on
/// `cd` tracking and `git -C` resolution.
/// Test: as [`git_verb_target_dir`], plus
/// `commit_target_dir_hands_back_the_argv_tail`.
fn git_verb_target_dir_with_tail(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
    matches: impl Fn(&str, &[String]) -> bool,
) -> Option<(String, PathBuf, Vec<String>)> {
    let mut effective_cwd = cwd.to_path_buf();
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if first_command_token(trimmed) == Some("cd") {
            if let Some(argv) = shlex::split(trimmed)
                && let Some(dest) = argv.get(1)
            {
                effective_cwd = resolve_target_path(dest, &effective_cwd, env);
            }
            continue;
        }
        let Some(subcommand) = shell_lex::git_subcommand(trimmed) else {
            continue;
        };
        let Some(argv) = shlex::split(trimmed) else {
            continue;
        };
        // The subcommand token's position, so the tail can be classified.
        // `git_subcommand` already skipped the global flags to resolve the
        // name, and a global flag VALUE that happens to equal the subcommand
        // name (`git -C reset reset --hard`) would find the earlier token —
        // the same "realistic invocations use at most one `-C`" simplification
        // `git_dash_c_override` documents.
        let Some(idx) = argv.iter().position(|t| *t == subcommand) else {
            continue;
        };
        let tail = &argv[idx + 1..];
        if !matches(&subcommand, tail) {
            continue;
        }
        let base = match git_dash_c_override(&argv, idx) {
            Some(dash_c) => resolve_target_path(dash_c, &effective_cwd, env),
            None => effective_cwd.clone(),
        };
        return Some((subcommand, base, tail.to_vec()));
    }
    None
}

/// Whether a git subcommand, given its argv tail, overwrites or deletes work
/// in the working tree.
///
/// Why: this table IS the false-positive boundary, and #5356 (a `pm_guard`
/// deny landing on a turn of purely read-only `git status`/`git log` calls) is
/// the live reminder of what a loose one costs. So each verb is narrowed to
/// the forms that actually destroy, and everything else — every other
/// subcommand, and every non-destructive form of these five — falls through to
/// ALLOW.
/// What, per verb:
/// - `reset`: only the modes that touch the working tree (`--hard`,
///   `--merge`, `--keep`). A bare `git reset` and its default `--mixed`
///   unstage without destroying anything, and stay allowed.
/// - `clean`: any force flag (`--force`, or a short cluster containing `f`;
///   `x`/`X` too, which are inert without force but cost nothing to include)
///   — UNLESS the command is a dry run (`-n`/`--dry-run`), which only prints.
/// - `checkout`: the pathspec-restoring forms (`-- <pathspec>`, a bare `.`)
///   and `-f`/`--force`, which discards the whole tree. `checkout -b`, a
///   plain branch switch, and a detaching `checkout <sha>` are untouched.
/// - `restore`: the modern equivalent of `checkout -- <pathspec>`, and the
///   easy one to miss. Its DEFAULT target is the working tree, so the rule
///   inverts: destructive unless `--staged`/`-S` is present without
///   `--worktree`/`-W` (index-only, which destroys nothing).
/// - `switch`: `--discard-changes`, plus `-f`/`--force` which implies it.
///   Included so the `checkout`/`switch` pair cannot be used to reach the
///   same destruction through the newer spelling.
///
/// Test: `is_whole_tree_destructive_denies_*`, `is_whole_tree_destructive_allows_*`.
fn is_whole_tree_destructive(subcommand: &str, tail: &[String]) -> bool {
    let has = |names: &[&str]| tail.iter().any(|t| names.contains(&t.as_str()));
    let cluster_has = |chars: &[char]| {
        tail.iter().any(|t| {
            t.starts_with('-')
                && !t.starts_with("--")
                && t.chars().skip(1).any(|c| chars.contains(&c))
        })
    };
    match subcommand {
        "reset" => has(&["--hard", "--merge", "--keep"]),
        "clean" => {
            let dry_run = has(&["--dry-run"]) || cluster_has(&['n']);
            let forced = has(&["--force"]) || cluster_has(&['f', 'x', 'X']);
            forced && !dry_run
        }
        "checkout" => {
            has(&["-f", "--force"])
                || tail.iter().any(|t| t == "." || t == "./")
                || pathspec_separator_with_paths(tail)
        }
        "restore" => {
            let staged = has(&["--staged", "-S"]);
            let worktree = has(&["--worktree", "-W"]);
            worktree || !staged
        }
        "switch" => has(&["--discard-changes", "-f", "--force"]),
        _ => false,
    }
}

/// Whether the tail carries a `--` end-of-options separator with at least one
/// pathspec after it — the `git checkout <ref> -- <pathspec>` shape that
/// restores files over uncommitted work.
fn pathspec_separator_with_paths(tail: &[String]) -> bool {
    tail.iter()
        .position(|t| t == "--")
        .is_some_and(|i| i + 1 < tail.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tail(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    /// The destructive-verb specialisation of [`git_verb_target_dir`], so the
    /// pre-existing cases below keep asserting on the shape they were written
    /// against rather than restating the classifier at every call.
    fn destructive_target_dir(
        command: &str,
        cwd: &Path,
        env: &PathEnv,
    ) -> Option<(String, PathBuf)> {
        git_verb_target_dir(command, cwd, env, is_whole_tree_destructive)
    }

    /// A directory that answers `is_main_checkout`: a `.git` DIRECTORY, which
    /// is how git marks a main checkout and never a linked worktree.
    fn main_checkout_dir() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".git")).expect("mkdir .git");
        dir
    }

    #[test]
    fn is_whole_tree_destructive_denies_the_incident_shapes() {
        // The two commands from the 2026-08-10 incident that this rule owns.
        // The third, `git reset HEAD`, is deliberately NOT here — see
        // `is_whole_tree_destructive_allows_the_near_misses`.
        assert!(is_whole_tree_destructive(
            "checkout",
            &tail(&["a1b2c3d", "--", "."])
        ));
        assert!(is_whole_tree_destructive(
            "clean",
            &tail(&["-fdx", "--", "crates/", "docs/"])
        ));
    }

    #[test]
    fn is_whole_tree_destructive_denies_every_listed_form() {
        for (verb, args) in [
            ("checkout", vec!["--", "src/lib.rs"]),
            ("checkout", vec!["HEAD~1", "--", "crates/"]),
            ("checkout", vec!["."]),
            ("checkout", vec!["./"]),
            ("checkout", vec!["-f", "main"]),
            ("checkout", vec!["--force", "main"]),
            ("restore", vec!["src/lib.rs"]),
            ("restore", vec!["--source=HEAD~1", "--", "src/"]),
            ("restore", vec!["--staged", "--worktree", "src/"]),
            ("restore", vec!["-S", "-W", "src/"]),
            ("reset", vec!["--hard"]),
            ("reset", vec!["--hard", "origin/main"]),
            ("reset", vec!["--merge", "HEAD"]),
            ("reset", vec!["--keep", "HEAD~2"]),
            ("clean", vec!["-f"]),
            ("clean", vec!["-fd"]),
            ("clean", vec!["-fdx"]),
            ("clean", vec!["-x"]),
            ("clean", vec!["--force", "-d"]),
            ("switch", vec!["--discard-changes", "main"]),
            ("switch", vec!["-f", "main"]),
        ] {
            assert!(
                is_whole_tree_destructive(verb, &tail(&args)),
                "expected `git {verb} {}` to be destructive",
                args.join(" ")
            );
        }
    }

    #[test]
    fn is_whole_tree_destructive_allows_the_near_misses() {
        // The false-positive boundary, and the reason it is tested: #5356 is
        // an open P2 filed because pm_guard denied a turn of purely read-only
        // git calls. `git reset HEAD` is here on the owner's explicit rule —
        // the default `--mixed` unstages and destroys nothing — even though it
        // was one of the three commands the incident agent ran.
        for (verb, args) in [
            ("reset", vec!["HEAD"]),
            ("reset", vec![]),
            ("reset", vec!["--soft", "HEAD~1"]),
            ("reset", vec!["--mixed", "HEAD"]),
            ("checkout", vec!["-b", "feature/x"]),
            ("checkout", vec!["-B", "feature/x", "origin/main"]),
            ("checkout", vec!["main"]),
            ("checkout", vec!["a1b2c3d"]),
            ("restore", vec!["--staged", "src/lib.rs"]),
            ("restore", vec!["-S", "src/lib.rs"]),
            ("clean", vec!["-n"]),
            ("clean", vec!["-nfd"]),
            ("clean", vec!["--dry-run", "-fdx"]),
            ("clean", vec!["-d"]),
            ("switch", vec!["main"]),
            ("switch", vec!["-c", "feature/x"]),
            ("status", vec!["--short"]),
            ("log", vec!["--oneline", "-20"]),
            ("diff", vec!["--stat", "HEAD~1"]),
            ("stash", vec!["list"]),
            ("add", vec!["-A"]),
            ("commit", vec!["-m", "wip"]),
        ] {
            assert!(
                !is_whole_tree_destructive(verb, &tail(&args)),
                "`git {verb} {}` must NOT be treated as destructive",
                args.join(" ")
            );
        }
    }

    #[test]
    fn destructive_target_dir_defaults_to_the_hook_cwd() {
        let env = PathEnv::from_process();
        let (verb, dir) =
            destructive_target_dir("git reset --hard origin/main", Path::new("/repo"), &env)
                .expect("a destructive verb must resolve a target");
        assert_eq!(verb, "reset");
        assert_eq!(dir, PathBuf::from("/repo"));
    }

    #[test]
    fn destructive_target_dir_follows_cd_and_dash_c() {
        // Both bypasses the sibling worktree guard already closes: a `cd` in an
        // earlier segment, and git's own `-C` working-directory override.
        let env = PathEnv::from_process();
        let (_, via_cd) = destructive_target_dir(
            "cd /elsewhere/main && git clean -fdx",
            Path::new("/repo"),
            &env,
        )
        .expect("cd must move the target");
        assert_eq!(via_cd, PathBuf::from("/elsewhere/main"));

        let (_, via_dash_c) = destructive_target_dir(
            "git -C /elsewhere/main reset --hard",
            Path::new("/repo"),
            &env,
        )
        .expect("-C must move the target");
        assert_eq!(via_dash_c, PathBuf::from("/elsewhere/main"));

        // Relative targets resolve against the running directory, and `..`
        // collapses lexically.
        let (_, relative) =
            destructive_target_dir("git -C ../other reset --hard", Path::new("/a/b"), &env)
                .expect("relative -C must resolve");
        assert_eq!(relative, PathBuf::from("/a/other"));
    }

    #[test]
    fn destructive_target_dir_finds_a_verb_hidden_in_a_later_segment() {
        // A benign leading verb must not hide the destructive one behind it.
        let env = PathEnv::from_process();
        let (verb, _) = destructive_target_dir(
            "git status --short; git checkout -- crates/",
            Path::new("/repo"),
            &env,
        )
        .expect("the second segment must be classified");
        assert_eq!(verb, "checkout");
    }

    #[test]
    fn destructive_target_dir_is_none_for_ordinary_work() {
        // The overwhelmingly common traffic: nothing here may reach the
        // filesystem classification at all.
        let env = PathEnv::from_process();
        for command in [
            "git status",
            "git log --oneline -20",
            "git diff HEAD~1",
            "ls -la",
            "cargo test -p trusty-mpm",
            "git checkout -b feature/x",
            "git reset HEAD",
            "git commit -m 'x'",
            "",
            // Unbalanced quotes: shlex yields no argv, so nothing is
            // positively identified and the rule stays out of it.
            "git reset --hard 'unterminated",
        ] {
            assert!(
                destructive_target_dir(command, Path::new("/repo"), &env).is_none(),
                "`{command}` must not resolve a destructive target"
            );
        }
    }

    #[test]
    fn commit_target_dir_finds_the_commit_and_follows_cd_and_dash_c() {
        let env = PathEnv::from_process();
        let is_commit = |verb: &str, _: &[String]| verb == "commit";

        let (verb, dir) =
            git_verb_target_dir("git commit -m 'wip'", Path::new("/repo"), &env, is_commit)
                .expect("a commit must resolve a target");
        assert_eq!(verb, "commit");
        assert_eq!(dir, PathBuf::from("/repo"));

        // The composition shape `git add -A && git commit -m …` is the ordinary
        // one, so the commit must be found in a later segment.
        let (_, chained) = git_verb_target_dir(
            "git add -A && git commit -m 'wip'",
            Path::new("/repo"),
            &env,
            is_commit,
        )
        .expect("the second segment must be classified");
        assert_eq!(chained, PathBuf::from("/repo"));

        // Both directory overrides the destructive rule already closes.
        let (_, via_cd) = git_verb_target_dir(
            "cd /elsewhere/main && git commit -m x",
            Path::new("/repo"),
            &env,
            is_commit,
        )
        .expect("cd must move the target");
        assert_eq!(via_cd, PathBuf::from("/elsewhere/main"));

        let (_, via_dash_c) = git_verb_target_dir(
            "git -C /elsewhere/main commit -m x",
            Path::new("/repo"),
            &env,
            is_commit,
        )
        .expect("-C must move the target");
        assert_eq!(via_dash_c, PathBuf::from("/elsewhere/main"));
    }

    #[test]
    fn commit_target_dir_is_none_for_everything_else() {
        let env = PathEnv::from_process();
        let is_commit = |verb: &str, _: &[String]| verb == "commit";
        for command in [
            "git status",
            "git log --oneline -5",
            "git add -A",
            "git push origin HEAD",
            "cargo test -p trusty-mpm",
            "",
        ] {
            assert!(
                git_verb_target_dir(command, Path::new("/repo"), &env, is_commit).is_none(),
                "`{command}` is not a commit"
            );
        }
    }

    #[test]
    fn evaluate_main_checkout_commit_denies_in_a_checkout_and_allows_in_a_worktree() {
        // `main_checkout_dir` fabricates `.git` as a directory, so `git diff
        // --cached` cannot run there and the staged set is UNKNOWN — the
        // deny arm of ADR-0049 decision 5, which is also the pre-ADR-0049
        // behaviour for every commit.
        let checkout = main_checkout_dir();
        let CommitVerdict::Deny(reason) =
            evaluate_main_checkout_commit_command("git commit -m 'wip'", checkout.path())
                .expect("a commit in a main checkout must be classified")
        else {
            panic!("an unreadable staged set must deny");
        };
        assert!(reason.contains("ADR-0044"), "{reason}");

        // A linked worktree carries a `.git` FILE. Committing there is the
        // whole remedy the deny offers, so it must work.
        let worktree = tempfile::tempdir().expect("tempdir");
        std::fs::write(worktree.path().join(".git"), "gitdir: /elsewhere").expect("write .git");
        assert!(
            evaluate_main_checkout_commit_command("git commit -m 'wip'", worktree.path()).is_none()
        );

        // Not a repository at all: nothing to protect.
        let plain = tempfile::tempdir().expect("tempdir");
        assert!(
            evaluate_main_checkout_commit_command("git commit -m 'wip'", plain.path()).is_none()
        );
    }

    #[test]
    fn commit_deny_reason_names_the_path_and_the_remedy() {
        let reason = commit_deny_reason(Path::new("/repo/main"));
        assert!(reason.contains("/repo/main"), "{reason}");
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
        assert!(reason.contains("Nothing is lost"), "{reason}");
        // ADR-0049: the reader who lands here after editing one `.md` needs to
        // know the carve-out exists and what it wants.
        assert!(reason.contains("ADR-0049"), "{reason}");
        assert!(reason.contains("git add"), "{reason}");
    }

    // ── ADR-0049: the commit gate reads the staged set ──────────────────────

    /// `classify_staged_commit` with the shapes the caller supplies, so each
    /// case reads as the argv tail plus the staged set and nothing else.
    fn classify(tail: &[&str], staged: Option<&[&str]>) -> CommitVerdict {
        let tail: Vec<String> = tail.iter().map(|s| (*s).to_string()).collect();
        let staged = staged.map(|s| s.iter().map(|p| (*p).to_string()).collect());
        classify_staged_commit(
            &tail,
            staged,
            Path::new("/repo/crates"),
            PathBuf::from("/repo"),
        )
    }

    fn is_docs_only(verdict: &CommitVerdict) -> bool {
        matches!(verdict, CommitVerdict::DocsOnly { .. })
    }

    fn deny_text(verdict: &CommitVerdict) -> &str {
        match verdict {
            CommitVerdict::Deny(reason) => reason,
            CommitVerdict::DocsOnly { .. } => panic!("expected a deny"),
        }
    }

    #[test]
    fn classify_staged_commit_permits_documents_and_configuration() {
        // ADR-0049 decision 1 and 2: the same non-source classification the
        // write boundary uses, so a file that may be WRITTEN here may be
        // COMMITTED here.
        for staged in [
            &["docs/adr/0049-x.md"][..],
            &["CLAUDE.md", "Cargo.toml"][..],
            &[".claude/settings.json", "TASK.md", "Makefile"][..],
            &["crates/trusty-mpm/changelog.d/5782-x.md"][..],
        ] {
            let verdict = classify(&["-m", "docs: x"], Some(staged));
            assert!(is_docs_only(&verdict), "{staged:?} is documents-only");
        }
        // The DocsOnly arm hands back both query keys, checkout ROOT first —
        // the order the HEAD-move rule uses, and the spelling a delegation
        // record most often carries (#5769).
        let CommitVerdict::DocsOnly { root, dirs } = classify(&[], Some(&["README.md"])) else {
            panic!("expected DocsOnly");
        };
        assert_eq!(root, PathBuf::from("/repo"));
        assert_eq!(
            dirs,
            [PathBuf::from("/repo"), PathBuf::from("/repo/crates")]
        );
    }

    #[test]
    fn classify_staged_commit_denies_source_and_names_it() {
        let verdict = classify(&["-m", "feat: x"], Some(&["crates/a/src/lib.rs"]));
        let reason = deny_text(&verdict);
        assert!(reason.contains("crates/a/src/lib.rs"), "{reason}");
        assert!(reason.contains("ADR-0049"), "{reason}");
        assert!(reason.contains("git restore --staged"), "{reason}");
    }

    #[test]
    fn classify_staged_commit_denies_a_mixed_set_and_names_only_the_source() {
        // ADR-0049 decision 6: fail safe on a mixed set, and name what made it
        // unsafe. Naming the documents too would send the reader unstaging the
        // files that were fine.
        let verdict = classify(
            &["-m", "wip"],
            Some(&["docs/x.md", "src/a.rs", "Cargo.toml", "src/b.py"]),
        );
        let reason = deny_text(&verdict);
        assert!(
            reason.contains("src/a.rs") && reason.contains("src/b.py"),
            "{reason}"
        );
        assert!(!reason.contains("docs/x.md"), "{reason}");
        assert!(!reason.contains("Cargo.toml"), "{reason}");
    }

    #[test]
    fn classify_staged_commit_denies_an_unreadable_or_empty_index() {
        // ADR-0045's distinction, on a gate: `None` is UNKNOWN and
        // `Some(vec![])` is empty, and neither is evidence of a docs commit.
        for staged in [None, Some(&[][..])] {
            let verdict = classify(&["-m", "wip"], staged);
            assert!(deny_text(&verdict).contains("ADR-0044"), "{staged:?}");
        }
    }

    #[test]
    fn classify_staged_commit_denies_the_forms_the_index_does_not_describe() {
        // ADR-0049 decision 5. Each of these commits content the staged set
        // does not hold, so a staged set of pure documents cannot license it.
        for tail in [
            &["-a", "-m", "wip"][..],
            &["-am", "wip"][..],
            &["--all"][..],
            &["--amend", "--no-edit"][..],
            &["-i", "--", "src/lib.rs"][..],
            &["--include", "src/lib.rs"][..],
            &["-o", "docs/x.md"][..],
            &["--only", "docs/x.md"][..],
            // A bare pathspec implies `--only`.
            &["docs/x.md"][..],
            &["-m", "wip", "--", "docs/x.md"][..],
        ] {
            let verdict = classify(tail, Some(&["docs/x.md"]));
            assert!(
                !is_docs_only(&verdict),
                "`git commit {}` must not be licensed by the index",
                tail.join(" ")
            );
        }
    }

    #[test]
    fn commit_flags_leave_the_index_authoritative_accepts_message_forms() {
        // The forms an agent actually types. A false `false` here costs the
        // carve-out; it never costs a new deny, because `false` is the
        // pre-ADR-0049 behaviour.
        for tail in [
            &[][..],
            &["-m", "docs: x"][..],
            &["--message", "docs: x"][..],
            &["--message=docs: x"][..],
            &["-m", "docs: x", "-s"][..],
            &["-m", "x", "--no-verify"][..],
            &["-q", "-m", "x"][..],
            &["--author", "A <a@b>", "-m", "x"][..],
            &["--date=2026-08-16", "-m", "x"][..],
            &["-S", "-m", "x"][..],
            &["-Sdeadbeef", "-m", "x"][..],
            &["--gpg-sign=deadbeef", "-m", "x"][..],
            &["-F", "/tmp/msg", "--cleanup=strip"][..],
            &["--trailer", "Closes: #1", "-m", "x"][..],
            &["--allow-empty", "-m", "x"][..],
            // A message that happens to contain a content flag is a VALUE,
            // consumed by the `-m` before it.
            &["-m", "--amend"][..],
        ] {
            let tail: Vec<String> = tail.iter().map(|s| (*s).to_string()).collect();
            assert!(
                commit_flags_leave_the_index_authoritative(&tail),
                "`git commit {}` leaves the index authoritative",
                tail.join(" ")
            );
        }
    }

    #[test]
    fn commit_flags_leave_the_index_authoritative_rejects_content_flags() {
        for tail in [
            &["-a"][..],
            &["--all"][..],
            &["--amend"][..],
            &["-i"][..],
            &["--include"][..],
            &["-o"][..],
            &["--only"][..],
            // Unrecognised: a short cluster, and a flag this list has never
            // heard of. The allowlist denies both rather than guessing.
            &["-sn"][..],
            &["--some-future-flag"][..],
        ] {
            let tail: Vec<String> = tail.iter().map(|s| (*s).to_string()).collect();
            assert!(
                !commit_flags_leave_the_index_authoritative(&tail),
                "`git commit {}` must not be licensed by the index",
                tail.join(" ")
            );
        }
    }

    #[test]
    fn commit_flags_leave_the_index_authoritative_rejects_a_pathspec() {
        for tail in [
            &["docs/x.md"][..],
            &["-m", "x", "docs/x.md"][..],
            &["--"][..],
        ] {
            let tail: Vec<String> = tail.iter().map(|s| (*s).to_string()).collect();
            assert!(
                !commit_flags_leave_the_index_authoritative(&tail),
                "a pathspec is `--only` by implication: {}",
                tail.join(" ")
            );
        }
    }

    #[test]
    fn command_is_a_lone_commit_accepts_a_commit_and_cd() {
        // `cd` moves no files and touches no index, so it cannot invalidate the
        // reading. Everything a documents commit legitimately needs is here.
        for command in [
            "git commit -m 'docs: x'",
            "cd /repo && git commit -m 'docs: x'",
            "cd /repo && cd docs && git commit -m x",
            "git -C /repo commit -m x",
        ] {
            assert!(
                command_is_a_lone_commit(command),
                "`{command}` is a lone commit"
            );
        }
    }

    #[test]
    fn command_is_a_lone_commit_rejects_a_second_commit() {
        // #5788 review, CRITICAL 1 — the demonstrated exploit. The walker
        // returns on the first commit segment, so without this the docs-only
        // reading licensed the later source commit.
        for command in [
            "git commit -m docs && git add -A && git commit -a -m src",
            "git commit -m docs ; git commit --amend --no-edit",
            "git commit -m a || git commit -m b",
        ] {
            assert!(
                !command_is_a_lone_commit(command),
                "`{command}` carries more than one commit"
            );
        }
    }

    #[test]
    fn command_is_a_lone_commit_rejects_anything_that_can_restage() {
        // One commit segment is not enough — anything running before it can
        // change the index after the reading. `git add -A && git commit` is the
        // shape that matters, and it was denied before ADR-0049 too.
        for command in [
            "git add -A && git commit -m docs",
            "echo x > src/lib.rs && git commit -m docs",
            "git status && git commit -m docs",
            "git commit -m docs && cargo test",
            // No commit at all: this rule has nothing to license.
            "git status",
        ] {
            assert!(
                !command_is_a_lone_commit(command),
                "`{command}` is not a lone commit"
            );
        }
    }

    #[test]
    fn composed_commit_deny_reason_names_the_composition_and_the_remedy() {
        let reason = composed_commit_deny_reason(Path::new("/repo"));
        assert!(reason.contains("/repo"), "{reason}");
        assert!(reason.contains("ADR-0049"), "{reason}");
        // The cause is the composition, not the content — say so, or the retry
        // is to unstage the document that was fine.
        assert!(reason.contains("Split the call"), "{reason}");
        assert!(reason.contains("git add -- <paths>"), "{reason}");
    }

    #[test]
    fn commit_target_dir_hands_back_the_argv_tail() {
        let env = PathEnv::from_process();
        let (verb, dir, tail) = git_verb_target_dir_with_tail(
            "cd /repo && git -C sub commit --amend -m wip",
            Path::new("/start"),
            &env,
            |verb, _| verb == "commit",
        )
        .expect("a commit must resolve");
        assert_eq!(verb, "commit");
        assert_eq!(dir, PathBuf::from("/repo/sub"));
        assert_eq!(tail, vec!["--amend", "-m", "wip"]);
    }

    #[test]
    fn staged_source_deny_reason_names_every_source_file() {
        let reason = staged_source_deny_reason(Path::new("/repo"), &["src/b.rs", "src/a.rs"]);
        assert!(
            reason.contains("src/a.rs") && reason.contains("src/b.rs"),
            "{reason}"
        );
        // Sorted, so two runs over the same set read identically.
        assert!(
            reason.find("src/a.rs") < reason.find("src/b.rs"),
            "{reason}"
        );
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
    }

    #[test]
    fn docs_commit_deny_reason_names_the_writer_and_attributes_the_claim() {
        let reason = docs_commit_deny_reason(Path::new("/repo"), &["rust-engineer".to_string()]);
        assert!(reason.contains("/repo"), "{reason}");
        assert!(reason.contains("rust-engineer"), "{reason}");
        assert!(reason.contains("ADR-0049"), "{reason}");
        // The claim is the daemon's, not this process's — a record can outlive
        // its agent. Same wording discipline as `head_move_deny_reason`.
        assert!(reason.contains("delegation records"), "{reason}");
        // Unstaging is the wrong retry here and the text has to say so.
        assert!(reason.contains("unstaging it will not help"), "{reason}");
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
    }

    #[test]
    fn starts_a_head_move_covers_the_three_verbs() {
        // ADR-0048 decision 10. Every form of these three moves HEAD, which is
        // what lets the rule be stated by verb with no argument analysis.
        for (verb, args) in [
            ("pull", vec![]),
            ("pull", vec!["--rebase"]),
            ("pull", vec!["origin", "main"]),
            ("pull", vec!["--ff-only"]),
            ("merge", vec!["origin/main"]),
            ("merge", vec!["--no-ff", "feature/x"]),
            ("merge", vec!["--squash", "feature/x"]),
            ("rebase", vec!["origin/main"]),
            ("rebase", vec!["-i", "HEAD~3"]),
            ("rebase", vec!["--onto", "main", "feature/x"]),
        ] {
            assert!(
                starts_a_head_move(verb, &tail(&args)),
                "expected `git {verb} {}` to start a HEAD move",
                args.join(" ")
            );
        }
    }

    #[test]
    fn starts_a_head_move_allows_in_progress_control() {
        // The carve-out: these resolve an operation that already started.
        // Denying them would park the shared checkout mid-rebase with no way
        // out, and the deny would carry no remedy that works from there.
        for (verb, args) in [
            ("rebase", vec!["--abort"]),
            ("rebase", vec!["--continue"]),
            ("rebase", vec!["--skip"]),
            ("rebase", vec!["--quit"]),
            ("rebase", vec!["--edit-todo"]),
            ("rebase", vec!["--show-current-patch"]),
            ("merge", vec!["--abort"]),
            ("merge", vec!["--quit"]),
            ("merge", vec!["--continue"]),
        ] {
            assert!(
                !starts_a_head_move(verb, &tail(&args)),
                "`git {verb} {}` resolves an in-progress operation and must be allowed",
                args.join(" ")
            );
        }
    }

    #[test]
    fn starts_a_head_move_ignores_everything_else() {
        // The false-positive boundary #5356 is the reminder for. `fetch` is the
        // remedy the deny offers and must never be classified (ADR-0048
        // decision 9); `checkout`/`switch` are deliberately out of this rule;
        // `merge-base` and `merge-tree` are separate subcommand names that read
        // without writing.
        for (verb, args) in [
            ("fetch", vec!["origin"]),
            ("fetch", vec!["--all", "--prune"]),
            ("status", vec!["--short"]),
            ("log", vec!["--oneline", "-20"]),
            ("diff", vec!["--stat"]),
            ("merge-base", vec!["main", "HEAD"]),
            ("merge-tree", vec!["main", "HEAD"]),
            ("checkout", vec!["main"]),
            ("switch", vec!["main"]),
            ("pull-request", vec![]),
            ("commit", vec!["-m", "wip"]),
        ] {
            assert!(
                !starts_a_head_move(verb, &tail(&args)),
                "`git {verb} {}` must not be treated as a HEAD move",
                args.join(" ")
            );
        }
    }

    #[test]
    fn starts_a_head_move_matches_in_progress_control_positionally() {
        // #5769: the carve-out used to scan the whole tail, so any command
        // carrying one of those strings anywhere — a commit message is the
        // realistic shape — exempted itself from the rule.
        for args in [
            vec!["-m", "--continue", "origin/main"],
            vec!["origin/main", "-m", "--abort"],
        ] {
            assert!(
                starts_a_head_move("merge", &tail(&args)),
                "`git merge {}` is a real merge and must still classify",
                args.join(" ")
            );
        }
        // The genuine form — the flag first — stays exempt.
        assert!(!starts_a_head_move("rebase", &tail(&["--continue"])));
    }

    #[test]
    fn main_checkout_head_move_resolves_a_subdirectory_to_the_checkout_root() {
        // #5769: a delegation record is stamped from `tm hook`'s own process
        // directory, so a command aimed at a subdirectory has to report the
        // checkout root as well or the query keys a directory no record matches.
        let checkout = main_checkout_dir();
        let sub = checkout.path().join("crates/foo");
        std::fs::create_dir_all(&sub).expect("mkdir sub");
        let (verb, target, root) = main_checkout_head_move(
            &format!("cd {} && git pull", sub.display()),
            checkout.path(),
        )
        .expect("a pull from a subdirectory must resolve");
        assert_eq!(verb, "pull");
        assert_eq!(target, sub);
        assert_eq!(root, checkout.path());
    }

    #[test]
    fn main_checkout_head_move_finds_the_checkout_and_skips_a_worktree() {
        let checkout = main_checkout_dir();
        let (verb, target, root) = main_checkout_head_move("git pull --rebase", checkout.path())
            .expect("a pull in a main checkout must resolve");
        assert_eq!(verb, "pull");
        assert_eq!(target, checkout.path());
        assert_eq!(root, checkout.path());

        // A linked worktree carries a `.git` FILE. Its HEAD belongs to the one
        // session that owns it, so a pull there races nothing and must resolve
        // nothing — this is the ordinary-work case the directory test protects.
        let worktree = tempfile::tempdir().expect("tempdir");
        std::fs::write(worktree.path().join(".git"), "gitdir: /elsewhere").expect("write .git");
        assert_eq!(main_checkout_head_move("git pull", worktree.path()), None);

        // Not a repository at all: nothing to protect.
        let plain = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            main_checkout_head_move("git merge main", plain.path()),
            None
        );
    }

    #[test]
    fn main_checkout_head_move_follows_cd_and_dash_c_and_later_segments() {
        let checkout = main_checkout_dir();
        let path = checkout.path().display();
        let outside = tempfile::tempdir().expect("tempdir");

        // `-C` and `cd` both aim the verb at a checkout the hook is not
        // standing in — the two overrides the sibling rules already close.
        let (_, via_dash_c, _) =
            main_checkout_head_move(&format!("git -C {path} rebase origin/main"), outside.path())
                .expect("-C must move the target");
        assert_eq!(via_dash_c, checkout.path());

        let (_, via_cd, _) =
            main_checkout_head_move(&format!("cd {path} && git pull"), outside.path())
                .expect("cd must move the target");
        assert_eq!(via_cd, checkout.path());

        // A benign leading verb must not hide the HEAD move behind it — this is
        // the ordinary `git fetch && git pull` shape.
        let (verb, _, _) = main_checkout_head_move("git fetch origin && git pull", checkout.path())
            .expect("the second segment must be classified");
        assert_eq!(verb, "pull");
    }

    #[test]
    fn main_checkout_head_move_is_none_for_ordinary_work_in_a_checkout() {
        // Even inside a main checkout, everything that is not one of the three
        // verbs resolves nothing and never reaches the daemon query.
        let checkout = main_checkout_dir();
        for command in [
            "git fetch origin",
            "git status --porcelain",
            "git log --oneline -5",
            "git worktree list",
            "cargo test -p trusty-mpm",
            "git rebase --abort",
            "",
        ] {
            assert_eq!(
                main_checkout_head_move(command, checkout.path()),
                None,
                "`{command}` must not resolve a HEAD move"
            );
        }
    }

    #[test]
    fn head_move_deny_reason_names_the_verb_the_path_and_both_remedies() {
        let reason = head_move_deny_reason(
            "pull",
            Path::new("/repo/main"),
            &["rust-engineer".to_string(), "rust-engineer".to_string()],
        );
        assert!(reason.contains("ADR-0048"), "{reason}");
        assert!(reason.contains("git pull"), "{reason}");
        assert!(reason.contains("/repo/main"), "{reason}");
        // The sibling is named once, not repeated per delegation.
        assert_eq!(reason.matches("rust-engineer").count(), 1, "{reason}");
        // Both remedies: the cheap one (fetch) and the general one (worktree).
        assert!(reason.contains("git fetch"), "{reason}");
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
    }

    #[test]
    fn head_move_deny_reason_attributes_the_claim_to_the_daemon() {
        // #5769: this process knows what the daemon's records SAY, never what
        // another agent is doing. A record can outlive its agent, and a grant
        // whose isolation POST failed is recorded unisolated — so stating the
        // claim as fact made the deny assert something it could not check.
        let reason = head_move_deny_reason("pull", Path::new("/repo/main"), &["qa".to_string()]);
        assert!(
            reason.contains("the daemon's delegation records name"),
            "the claim must be attributed to its source: {reason}"
        );
        assert!(
            !reason.contains("is already writing there"),
            "the deny must not assert another agent's behaviour as fact: {reason}"
        );
    }

    #[test]
    fn deny_reason_names_the_verb_the_path_and_the_remedy() {
        let reason = deny_reason("clean", Path::new("/repo/main"));
        assert!(reason.contains("ADR-0037"), "{reason}");
        assert!(reason.contains("git clean"), "{reason}");
        assert!(reason.contains("/repo/main"), "{reason}");
        assert!(reason.contains(".claude/worktrees"), "{reason}");
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
    }
}
