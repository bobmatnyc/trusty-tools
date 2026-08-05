//! The agent-cost stop's Bash escape hatch: "does this command do nothing but
//! persist work?" (issue #4837; flag surface rebuilt in the #4850 review).
//!
//! Why: this is the one ALLOW-list in the guard, and it runs in the opposite
//! direction from every deny rule around it. A deny rule that misses a shape
//! lets a PM edit a file; this one, if it misses a shape, lets a *stopped* agent
//! keep running arbitrary programs — the stop becomes decorative. The first cut
//! got segment splitting right and the flag surface wrong: it resolved the git
//! subcommand through git's global options and then asked only "is the
//! subcommand allowlisted?", so `git -c diff.external='cargo test' diff`,
//! `git push --receive-pack='cargo test' /tmp/repo HEAD`,
//! `git push ext::sh -c 'cargo test' HEAD`, and `git diff <(cargo test)` all
//! answered yes while executing an arbitrary program.
//!
//! Every one of those is the same mistake — an unrecognised construct was
//! allowed through — so the rebuild is structural rather than a list of the four
//! strings. Four rules, three of them default-deny:
//!
//! 1. **No live shell metacharacter.** A [`SUBSTITUTION_METACHARS`] byte
//!    outside single quotes, a [`SYNTAX_METACHARS`] byte outside quotes
//!    entirely, or unbalanced quotes anywhere disqualify the whole command.
//!    That covers command substitution, subshells, process substitution, and
//!    redirection in both directions with one rule, instead of scanning for the
//!    particular spellings someone thought of.
//! 2. **The program must be `git` itself.** No env-assignment prefix
//!    (`GIT_SSH_COMMAND='cargo test' git push`), no `sudo`, no `env`.
//! 3. **Global options are default-denied**, allowing only the handful in
//!    [`SAFE_GIT_GLOBAL_OPTS_WITH_ARG`] / [`SAFE_GIT_GLOBAL_FLAGS`] that cannot
//!    name a program. `-c` is rejected outright rather than having its values
//!    allow-listed — an agent persisting work does not need it, and the set of
//!    config keys that reach an exec (`diff.external`, `core.gitProxy`,
//!    `credential.helper`, `protocol.ext.allow`, `alias.*`, `sshCommand`, …) is
//!    open-ended enough that enumerating the safe ones is the losing side of the
//!    bet.
//! 4. **Arguments may not name a program or a remote helper.** Any token
//!    containing `::` is a remote-helper transport (`ext::sh -c …` runs a shell
//!    by design), and any option in the [`EXEC_OPTION_DENY`] family — including
//!    every `--*-pack` — hands git a command line to run.
//!
//! Rule 4 is the one enumerated set, and deliberately so: the *cost* of a false
//! reject here is the exact failure this hatch exists to prevent. A stopped
//! agent that cannot push loses its work, so the post-subcommand surface stays
//! permissive for ordinary flags (`-m`, `--amend`, `--force-with-lease`, `-u`)
//! and names only the families that take a program or an output file.
//!
//! Residual, documented rather than hidden: git reads exec-capable settings from
//! the repository's own config and attributes (`diff.external` via the default
//! `--ext-diff`, `textconv`), so an agent that wrote `.git/config` *before*
//! hitting the ceiling can still reach an exec through a bare `git diff`. Rule 3
//! closes the argument-supplied route; closing the on-disk one would mean
//! reading and judging repo config from inside a `PreToolUse` hook, which is
//! neither fast nor side-effect-free. Same posture as this module's sibling
//! bypass notes on `evaluate_worktree_add_command`.
//!
//! What: [`command_is_persistence_only`] is the whole public surface.
//! Test: `command_is_persistence_only_*` in the sibling `tests` module.

use trusty_mpm::core::agent_cost::PERSISTENCE_GIT_SUBCOMMANDS;

use super::shell_lex::QuoteScan;

/// Metacharacters that introduce an expansion, and so disqualify a command
/// anywhere single quotes do not suppress them.
///
/// Why: `$(…)` and `` `…` `` run another command, and a double quote does NOT
/// stop either — `git commit -m "$(cargo build)"` is the case the first cut
/// caught by string-matching `$(`. Rejecting the bare `$` and backtick instead
/// covers every spelling built from them (`${…}`, `$'…'`, an arithmetic `$((…))`
/// whose parens a paren-matcher would have to balance) without recognising any
/// of them.
/// What: the byte set [`has_live_metacharacter`] rejects wherever
/// [`QuoteScan::allows_substitution`] is true, i.e. everywhere but inside single
/// quotes. `git commit -m 'costs $5'` therefore still works.
/// Test: `command_is_persistence_only_rejects_substitution_and_redirection`,
/// `command_is_persistence_only_allows_metacharacters_inside_quotes`.
const SUBSTITUTION_METACHARS: &[u8] = b"$`";

/// Metacharacters that are shell syntax only when unquoted, and disqualify a
/// command there.
///
/// Why: `<(…)`, `>(…)`, a `(…)` subshell, and plain `< f` / `> f` redirection
/// are the other ways a command runs or writes something you cannot see in the
/// argv. The first cut asked [`super::has_file_write_redirection`] about `>` and
/// knew nothing about `<`, so `git diff <(cargo test)` — neither a substitution
/// spelling it recognised nor a `>` — passed straight through. Treating the raw
/// characters as disqualifying closes the class: nothing has to be recognised,
/// so nothing can be unrecognised, and none of them is needed to stage, commit,
/// push, or report.
/// What: the byte set [`has_live_metacharacter`] rejects wherever
/// [`QuoteScan::is_unquoted`] is true. Inside quotes they are literal argument
/// text (`git commit -m 'handles (n>1)'`) and are allowed — the rule has to stay
/// quote-aware or it strands the work the hatch exists to save.
/// Test: `command_is_persistence_only_rejects_process_substitution`,
/// `command_is_persistence_only_allows_metacharacters_inside_quotes`.
const SYNTAX_METACHARS: &[u8] = b"()<>";

/// The only git global options allowed to precede a persistence subcommand,
/// value-taking form.
///
/// Why: an agent commits from a worktree, so `git -C <path> commit` has to keep
/// working — but that is the entire legitimate need, alongside the explicit
/// `--git-dir`/`--work-tree` spelling of the same thing. None of the three can
/// name a program. Everything else is default-denied, which is what makes `-c`
/// (config injection: `diff.external`, `core.gitProxy`, `credential.helper`,
/// `protocol.ext.allow`, `alias.*`), `--config-env`, and `--exec-path` (where
/// git looks for its subcommand binaries) rejected without any of them being
/// listed anywhere.
/// What: names matched before any `=`; the `=`-joined spelling carries its value
/// in the same token, the bare spelling consumes the next one.
/// Test: `command_is_persistence_only_sees_past_git_global_flags`,
/// `command_is_persistence_only_rejects_config_injection`.
const SAFE_GIT_GLOBAL_OPTS_WITH_ARG: &[&str] = &["-C", "--git-dir", "--work-tree"];

/// The only valueless git global options allowed before a persistence
/// subcommand.
///
/// Why: these three change nothing but output plumbing and locking, and agents
/// reach for `--no-pager` routinely. Kept as a closed list for the same reason
/// as [`SAFE_GIT_GLOBAL_OPTS_WITH_ARG`] — the default is deny.
const SAFE_GIT_GLOBAL_FLAGS: &[&str] =
    &["--no-pager", "--no-optional-locks", "--literal-pathspecs"];

/// Post-subcommand option families that hand git a program to run or a file to
/// write.
///
/// Why: git's global options are not the only place a command line can be
/// injected — `git push --receive-pack='cargo test' /tmp/repo HEAD` puts it
/// *after* the subcommand, where the first cut never looked because
/// `git_subcommand` had already answered "push" and stopped reading. The
/// families here are the exec-capable ones on the persistence surface:
/// `--exec`/`--exec-path` and every `--*-pack` name a program; `--ext-diff` and
/// `--textconv` turn on repository-configured external filters; `--output`
/// writes a file, which rule 1 already forbids via `>` and should not be
/// reachable by another spelling.
/// What: matched against the option name (the part before any `=`) by
/// [`argument_is_inert`], plus a `-pack` suffix rule so a future
/// `--<something>-pack` is denied without being listed.
/// Test: `command_is_persistence_only_rejects_exec_options`.
const EXEC_OPTION_DENY: &[&str] = &[
    "--exec",
    "--exec-path",
    "--ext-diff",
    "--textconv",
    "--output",
    "--output-directory",
    "--config-env",
];

/// Whether EVERY segment of `command` is an allowlisted persistence git call.
///
/// Why (#4837 review, BLOCK 1(b)): the agent-cost hard stop needs an escape
/// hatch that lets a stopped agent commit and push its work without letting it
/// keep working. That question — "what does this Bash command actually do?" —
/// is `pm_guard_bash`'s domain, and the parent module already owns the hard part
/// of the answer: composition splitting ([`super::split_shell_segments`]).
/// Re-deriving it in the cost guard would be a second implementation of a
/// safety-critical parser, so the classifier lives here and the *policy* (which
/// subcommands, which tools) stays in [`trusty_mpm::core::agent_cost`].
///
/// The subcommand resolution deliberately does NOT reuse
/// `shell_lex::git_subcommand`. That parser exists to find a subcommand for the
/// *deny* side (`git apply`), where skipping past an unrecognised global option
/// is the safe direction; here it is the unsafe one, and reusing it is exactly
/// how `-c diff.external=…` got through. Two callers, two opposite notions of
/// "safe on ambiguity" — see the module docs.
/// What: `true` only when the command is non-empty, carries no live shell
/// metacharacter ([`has_live_metacharacter`]), and every composition segment is
/// a bare `git` invocation ([`segment_is_persistence`]) whose subcommand is in
/// [`PERSISTENCE_GIT_SUBCOMMANDS`] and whose options are all inert. One
/// unrecognised construct anywhere fails the WHOLE command, so
/// `git commit -m x && cargo test` is not persistence.
/// Test: `command_is_persistence_only_accepts_commit_and_push`,
/// `command_is_persistence_only_sees_past_git_global_flags`,
/// `command_is_persistence_only_rejects_smuggled_work`,
/// `command_is_persistence_only_rejects_config_injection`,
/// `command_is_persistence_only_rejects_exec_options`,
/// `command_is_persistence_only_rejects_remote_helper_transport`,
/// `command_is_persistence_only_rejects_process_substitution`,
/// `command_is_persistence_only_rejects_substitution_and_redirection`.
pub(crate) fn command_is_persistence_only(command: &str) -> bool {
    if command.trim().is_empty() || has_live_metacharacter(command) {
        return false;
    }
    let mut saw_one = false;
    for segment in super::split_shell_segments(command) {
        if segment.trim().is_empty() {
            // A trailing separator leaves an empty tail; ignore it, but it
            // cannot be the only thing in the command (guarded above).
            continue;
        }
        saw_one = true;
        if !segment_is_persistence(segment) {
            return false;
        }
    }
    saw_one
}

/// Whether `command` contains a metacharacter that is live where it sits.
///
/// Why: the two byte sets have different quoting rules and collapsing them into
/// one would be wrong in both directions. A double quote does not suppress
/// substitution, so `git commit -m "$(cargo build)"` must be rejected even
/// though the `$` is quoted; it does suppress redirection and grouping, so
/// `git commit -m "spec -> code"` must be allowed. Only single quotes kill
/// everything. Getting this backwards either reopens the substitution bypass or
/// strands an agent over an arrow in its commit message.
/// What: `true` when quotes are unbalanced (the [`QuoteScan`] map is
/// untrustworthy, so nothing can be shown to be quoted), when a
/// [`SUBSTITUTION_METACHARS`] byte sits anywhere but inside single quotes, or
/// when a [`SYNTAX_METACHARS`] byte sits outside quotes entirely.
/// Test: `command_is_persistence_only_allows_metacharacters_inside_quotes`,
/// `command_is_persistence_only_rejects_process_substitution`,
/// `command_is_persistence_only_rejects_substitution_and_redirection`.
fn has_live_metacharacter(command: &str) -> bool {
    let scan = QuoteScan::new(command);
    if !scan.balanced {
        return true;
    }
    command.bytes().enumerate().any(|(i, b)| {
        (SUBSTITUTION_METACHARS.contains(&b) && scan.allows_substitution(i))
            || (SYNTAX_METACHARS.contains(&b) && scan.is_unquoted(i))
    })
}

/// Whether one composition segment is a bare, inert git persistence call.
///
/// Why: the per-segment half of [`command_is_persistence_only`], split out so
/// the argv walk reads as the three default-deny gates it is: the program must
/// be git, the globals must be recognised-safe, the subcommand must be
/// allowlisted, and the tail must be inert.
/// What: shlex-splits (unbalanced quotes → `None` → not persistence), requires
/// the program basename to be `git` with NO env-assignment/`sudo`/`env` prefix
/// (`GIT_SSH_COMMAND='cargo test' git push` and `sudo git commit` are both
/// exec-adjacent and neither is needed to persist), walks the global options
/// against [`SAFE_GIT_GLOBAL_OPTS_WITH_ARG`]/[`SAFE_GIT_GLOBAL_FLAGS`] denying
/// anything else, checks the subcommand against [`PERSISTENCE_GIT_SUBCOMMANDS`],
/// and requires every remaining token to satisfy [`argument_is_inert`].
/// Test: covered via `command_is_persistence_only_*` (this is its core).
fn segment_is_persistence(segment: &str) -> bool {
    let Some(argv) = shlex::split(segment) else {
        return false;
    };
    let Some(program) = argv.first() else {
        return false;
    };
    if program.rsplit('/').next().unwrap_or(program) != "git" {
        return false;
    }
    let mut i = 1;
    let sub = loop {
        let Some(tok) = argv.get(i) else {
            // Ran off the end: no subcommand, or a value-taking global option
            // with nothing after it.
            return false;
        };
        if !tok.starts_with('-') {
            break tok.as_str();
        }
        let name = tok.split('=').next().unwrap_or(tok);
        if SAFE_GIT_GLOBAL_OPTS_WITH_ARG.contains(&name) {
            i += if tok.contains('=') { 1 } else { 2 };
            continue;
        }
        if SAFE_GIT_GLOBAL_FLAGS.contains(&tok.as_str()) {
            i += 1;
            continue;
        }
        // Default-deny: an unrecognised global option may name a program.
        return false;
    };
    PERSISTENCE_GIT_SUBCOMMANDS.contains(&sub) && argv[i + 1..].iter().all(|a| argument_is_inert(a))
}

/// Whether one post-subcommand argv token can be handed to git without letting
/// it execute something.
///
/// Why: `git_subcommand` stopped reading once it had resolved `push`, so
/// everything after the subcommand was unexamined — which is where
/// `--receive-pack='cargo test'` and the `ext::` transport live. Both are
/// checked here, on every remaining token, so position cannot hide either.
/// What: `false` for any token containing `::` (git's remote-helper transport
/// syntax — `ext::sh -c '…'` runs a shell by design, and the form generalises
/// to any `<transport>::<address>` helper) and for any option whose name, before
/// an `=`, is in [`EXEC_OPTION_DENY`] or ends in `-pack`. Everything else is
/// inert: ordinary flags, refs, pathspecs, and messages cannot add an exec.
/// Test: `command_is_persistence_only_rejects_exec_options`,
/// `command_is_persistence_only_rejects_remote_helper_transport`.
fn argument_is_inert(arg: &str) -> bool {
    if arg.contains("::") {
        return false;
    }
    if !arg.starts_with('-') {
        return true;
    }
    let name = arg.split('=').next().unwrap_or(arg);
    !EXEC_OPTION_DENY.contains(&name) && !name.ends_with("-pack")
}
