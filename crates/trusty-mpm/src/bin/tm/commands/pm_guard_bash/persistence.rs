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
//! 2. **The program token must be the literal string `git`.** No
//!    env-assignment prefix (`GIT_SSH_COMMAND='cargo test' git push`), no
//!    `sudo`, no `env`, and no path — see [`segment_is_persistence`] for why a
//!    basename check was not enough.
//! 3. **Global options are default-denied**, allowing only the handful in
//!    [`SAFE_GIT_GLOBAL_OPTS_WITH_ARG`] / [`SAFE_GIT_GLOBAL_FLAGS`] that cannot
//!    name a program. `-c` is rejected outright rather than having its values
//!    allow-listed — an agent persisting work does not need it, and the set of
//!    config keys that reach an exec (`diff.external`, `core.gitProxy`,
//!    `credential.helper`, `protocol.ext.allow`, `alias.*`, `sshCommand`, …) is
//!    open-ended enough that enumerating the safe ones is the losing side of the
//!    bet.
//! 4. **Post-subcommand long options are default-denied too**, allowing only
//!    [`SAFE_LONG_OPTS`]; and any token containing `::` is rejected as a
//!    remote-helper transport (`ext::sh -c …` runs a shell by design).
//!
//! Rule 4 used to be the one *enumerated-danger* set — a deny list of
//! `--exec`/`--*-pack`/`--ext-diff`/`--output` names — on the reasoning that a
//! false reject strands an agent's work. The #4850 second review broke it with a
//! construct the list could not express: git's `parse-options` accepts any
//! unambiguous **prefix** of a long option, so `--exe` *is* `--exec` and
//! `--rece` *is* `--receive-pack`. Both passed the exact-match deny and both
//! executed an arbitrary program. A suffix rule cannot survive that either —
//! abbreviation strips the `-pack` off entirely.
//!
//! Adding `--exe` and `--rece` would have been the third round of the same
//! mistake, so rule 4 inverted to match rule 3. The abbreviation question then
//! stops needing an answer: an abbreviation of a dangerous option is not in
//! [`SAFE_LONG_OPTS`] and is denied, and an abbreviation of a *safe* option is
//! also denied — a false reject, in the harmless direction. Whatever the next
//! unrecognised construct turns out to be, the answer here is already "denied".
//!
//! The false-reject cost is smaller than the original reasoning assumed. The
//! hatch is not a general git CLI; it exists so a stopped agent can save work,
//! and `git add`/`git commit -m`/`git push` in their plain spellings always
//! work. An exotic flag being rejected costs a retry with the plain spelling,
//! not the work. Short options and positionals stay permissive — a single-dash
//! token is not abbreviatable, so there is no unrecognised-prefix class there,
//! and none of the short options on these five subcommands names a program.
//!
//! Residual, documented rather than hidden: git executes programs named by the
//! on-disk state of whatever repository it is pointed at, and `-C` /
//! `--git-dir` / `--work-tree` are allowed globals — so the repository is
//! *attacker-choosable within the same command*: `git -C /tmp/evil diff` runs
//! the `diff.external` configured in `/tmp/evil/.git/config`. Both the agent's
//! own repo and any path it wrote before hitting the ceiling are in reach.
//! The routes are config settings (`diff.external` via the default
//! `--ext-diff`, `textconv`, `core.editor`, and `core.fsmonitor`, which fires
//! on something as plain as `git -C /tmp/evil status`) and **hooks** — a
//! planted `.git/hooks/pre-commit` runs on `git commit -am hooked` while this
//! classifier ALLOWs the command, which the #4850 review demonstrated and which
//! is the cheapest of these to plant. Treat the list as instances of the class,
//! not its boundary. Rules 3 and 4 close the argument-supplied route; closing
//! the on-disk one would mean reading and judging repo config, hooks, and
//! attributes from inside a `PreToolUse` hook, which is neither fast nor
//! side-effect-free. `PATH` is the same class — a bare `git` resolves through it
//! and no argv-level check can see it. Same posture as this module's sibling
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

/// The only long options allowed after a persistence subcommand.
///
/// Why: the deny list this replaces named `--exec`, `--exec-path`, `--ext-diff`,
/// `--textconv`, `--output`, `--config-env` and any `--*-pack`, and matched them
/// exactly. Git does not: `parse-options` resolves any unambiguous prefix, so
/// `git push --exe=cargo /tmp/r HEAD` and `--rece=cargo` both ran `cargo` while
/// the classifier called the command persistence. Listing the abbreviations
/// would be endless — every proper prefix of every exec-capable name, present
/// and future — so the surface is default-deny instead, the same shape rule 3
/// already had. Nothing dangerous has to be recognised, which is the only
/// version of this rule that survives a construct nobody has thought of yet.
///
/// Entries are chosen for one question: can this option name a program, or a
/// file for git to write? None of these can. `--gpg-sign` takes a key id,
/// `--push-option` a string forwarded to the remote, `--repo` a repository
/// (an `ext::…` value is caught by the `::` rule first), `--file`/`--fixup`
/// read rather than exec.
///
/// What: matched EXACTLY against the option name (the part before any `=`) by
/// [`argument_is_inert`]. Exact match is deliberate — `--set-up` is a legal git
/// abbreviation of `--set-upstream` and is rejected here, a false reject in the
/// harmless direction, because the alternative is re-opening the prefix hole.
/// `--` is in the list because the pathspec separator starts with `-` and is not
/// an option at all.
/// Test: `command_is_persistence_only_rejects_exec_options`,
/// `command_is_persistence_only_rejects_abbreviated_exec_options`,
/// `command_is_persistence_only_accepts_the_flags_agents_actually_use`.
const SAFE_LONG_OPTS: &[&str] = &[
    // Not an option: the pathspec separator.
    "--",
    // Output plumbing, shared across the five subcommands.
    "--quiet",
    "--verbose",
    "--porcelain",
    "--dry-run",
    "--no-verify",
    "--progress",
    "--no-progress",
    "--color",
    "--no-color",
    // git add
    "--all",
    "--no-all",
    "--update",
    "--force",
    "--intent-to-add",
    "--renormalize",
    "--sparse",
    "--ignore-removal",
    // git commit
    "--message",
    "--file",
    "--amend",
    "--no-edit",
    "--allow-empty",
    "--allow-empty-message",
    "--signoff",
    "--no-signoff",
    "--reset-author",
    "--author",
    "--date",
    "--gpg-sign",
    "--no-gpg-sign",
    "--fixup",
    "--squash",
    "--only",
    "--include",
    "--no-status",
    "--cleanup",
    // git push
    "--set-upstream",
    "--force-with-lease",
    "--force-if-includes",
    "--tags",
    "--follow-tags",
    "--delete",
    "--atomic",
    "--no-atomic",
    "--prune",
    "--push-option",
    "--repo",
    "--thin",
    "--no-thin",
    "--signed",
    "--no-signed",
    // git status
    "--short",
    "--long",
    "--branch",
    "--no-branch",
    "--show-stash",
    "--untracked-files",
    "--ignored",
    "--ignore-submodules",
    "--ahead-behind",
    "--no-ahead-behind",
    "--renames",
    "--no-renames",
    "--column",
    "--no-column",
    // git diff
    "--stat",
    "--shortstat",
    "--numstat",
    "--dirstat",
    "--summary",
    "--name-only",
    "--name-status",
    "--cached",
    "--staged",
    "--patch",
    "--no-patch",
    "--raw",
    "--unified",
    "--check",
    "--exit-code",
    "--find-renames",
    "--find-copies",
    "--diff-filter",
    "--merge-base",
    "--relative",
    "--no-prefix",
    "--src-prefix",
    "--dst-prefix",
    "--text",
    "--binary",
    "--full-index",
    "--minimal",
    "--histogram",
    "--patience",
    "--anchored",
    "--word-diff",
    "--indent-heuristic",
    "--ignore-all-space",
    "--ignore-space-change",
    "--ignore-space-at-eol",
    "--ignore-blank-lines",
    "--ignore-cr-at-eol",
    // The explicit OFF switches for the two exec-capable diff filters. Their
    // ON spellings (`--ext-diff`, `--textconv`) are absent, hence denied.
    "--no-ext-diff",
    "--no-textconv",
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
/// `command_is_persistence_only_rejects_abbreviated_exec_options`,
/// `command_is_persistence_only_accepts_the_flags_agents_actually_use`,
/// `command_is_persistence_only_requires_a_bare_git_program`,
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
/// the argv walk reads as the four gates it is: the program must be git, the
/// globals must be recognised-safe, the subcommand must be allowlisted, and the
/// tail must be inert.
///
/// The program test compares the WHOLE token, not its basename. The #4850
/// second review executed a planted script named `git`: `program.rsplit('/')`
/// answered "git" for `./git`, `/tmp/evil/git`, and `../../tmp/evil/git`, so a
/// basename check verified the file's *name* while the module doc claimed it
/// verified the *program*. Resolving the path instead would mean stat-ing from
/// inside a `PreToolUse` hook and still could not tell a planted `git` on `PATH`
/// from the real one, so the check now matches what it can actually prove: the
/// literal token `git`. `PATH` resolution stays a documented residual (see the
/// module docs); an in-command `PATH=…` override is already rejected, because
/// then the program token is the assignment, not `git`.
/// What: shlex-splits (unbalanced quotes → `None` → not persistence), requires
/// the program token to be exactly `git` — no path, and no
/// env-assignment/`sudo`/`env` prefix (`GIT_SSH_COMMAND='cargo test' git push`
/// and `sudo git commit` are both exec-adjacent and neither is needed to
/// persist) — walks the global options against
/// [`SAFE_GIT_GLOBAL_OPTS_WITH_ARG`]/[`SAFE_GIT_GLOBAL_FLAGS`] denying anything
/// else, checks the subcommand against [`PERSISTENCE_GIT_SUBCOMMANDS`], and
/// requires every remaining token to satisfy [`argument_is_inert`].
/// Test: `command_is_persistence_only_requires_a_bare_git_program`, plus the
/// rest of `command_is_persistence_only_*` (this is their core).
fn segment_is_persistence(segment: &str) -> bool {
    let Some(argv) = shlex::split(segment) else {
        return false;
    };
    let Some(program) = argv.first() else {
        return false;
    };
    if program != "git" {
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
/// `--receive-pack='cargo test'` and the `ext::` transport live. The first fix
/// examined the tokens but judged long options against a list of dangerous
/// *names*, matched exactly, which git's prefix resolution walks straight
/// through (`--exe` → `--exec`, `--rece` → `--receive-pack`). Long options are
/// therefore default-denied now; see [`SAFE_LONG_OPTS`].
/// What: `false` for any token containing `::` (git's remote-helper transport
/// syntax — `ext::sh -c '…'` runs a shell by design, and the form generalises
/// to any `<transport>::<address>` helper), and for any `--` token whose name,
/// before an `=`, is not in [`SAFE_LONG_OPTS`]. Everything else is inert:
/// positionals (refs, pathspecs, remotes, messages) and short options, which
/// cannot be abbreviated and none of which names a program on this surface.
/// Test: `command_is_persistence_only_rejects_exec_options`,
/// `command_is_persistence_only_rejects_abbreviated_exec_options`,
/// `command_is_persistence_only_rejects_remote_helper_transport`,
/// `command_is_persistence_only_accepts_the_flags_agents_actually_use`.
fn argument_is_inert(arg: &str) -> bool {
    if arg.contains("::") {
        return false;
    }
    if !arg.starts_with("--") {
        return true;
    }
    let name = arg.split('=').next().unwrap_or(arg);
    SAFE_LONG_OPTS.contains(&name)
}
