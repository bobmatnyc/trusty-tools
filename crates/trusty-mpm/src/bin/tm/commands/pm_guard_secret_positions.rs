//! The position rules of the #7266 secret-file read guard: which words of a
//! command sit in a WORD LIST, a git REF position, or a TEXT PAYLOAD, and when
//! such a word may set the directory-prefix proxy aside (#7498, #7557).
//!
//! Why: #7557 — two consecutive critic rounds on #7498 found an exemption
//! judging a path by SPELLING rather than STRUCTURE, and it held one more: a
//! glob component (`feat/.?/secrets/prod-credentials`) that bash up to 5.1
//! expands onto `..`, and a fragment the path-byte cut lifted out of a token
//! that lives elsewhere (`../x=feat/db-credentials`). Both presented a
//! branch prefix as their first component while naming a path outside it.
//! The rules also pushed `pm_guard_secret_read.rs` to its 500-SLOC cap, so they
//! moved here the way #7414 carved the word-cutting layer out.
//! What: [`word_position_start`] finds where a segment's word list or ref names
//! begin, [`exempt_in_word_position`] decides whether one word there may skip
//! the proxy, and [`text_payload_indices`] names the prose tokens. Every
//! exemption routes through [`is_a_well_formed_ref_name`], which applies
//! `git check-ref-format`'s structural rules, so a word no ref could spell
//! keeps its deny.
//! Test: `allows_a_for_loop_word_list_of_branch_names`,
//! `denies_a_secret_file_in_a_for_loop_word_list`,
//! `allows_a_git_ref_name_carrying_a_word_family`,
//! `denies_a_secret_file_in_a_git_ref_position`,
//! `denies_a_traversal_hidden_from_the_prefix_check_7557`,
//! `a_well_formed_ref_name_follows_check_ref_format_7557`, and the rest of
//! `pm_guard_secret_read`'s `tests` submodule.

use crate::commands::hook_rewrite::strip_wrapper_prefix;
use crate::commands::pm_guard_bash::{git_argv_at_subcommand, matches_only_name_substring_family};
use crate::commands::pm_guard_secret_read::{
    NESTED_COMMAND_MARKERS, command_basename, has_a_named_extension,
};
use crate::commands::pm_guard_secret_words::{normalize_bracket_classes, scan_spellings};

/// Which kind of word-carrying position a segment opened (#7557).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WordPosition {
    /// The words after `for <var> in` / `select <var> in`.
    WordList,
    /// A git ref name, where a `src:dst` refspec is structure, not a path.
    RefName,
}

/// Where a segment's word list or ref names begin, and which it is (#7557).
///
/// What: [`for_word_list_start`] first, then [`ref_name_start`] — the order
/// `secret_words_in_segment` always asked them in.
/// Test: `allows_a_for_loop_word_list_of_branch_names`,
/// `allows_a_git_ref_name_carrying_a_word_family`.
pub(crate) fn word_position_start(segment: &str, argv: &[String]) -> Option<(usize, WordPosition)> {
    for_word_list_start(segment, argv)
        .map(|at| (at, WordPosition::WordList))
        .or_else(|| ref_name_start(segment).map(|at| (at, WordPosition::RefName)))
}

/// Whether `word`, cut from `token` in a word-list or ref position, may set
/// the directory-prefix proxy aside (#7557).
///
/// Why: the word is a FRAGMENT — `secret_files_named_in` cuts every token at
/// each byte a path cannot contain — so judging the fragment judged a
/// spelling. `for f in ../x=feat/db-credentials` put `feat/db-credentials` in
/// front of the prefix check while the loop variable holds a path outside
/// `feat/`.
/// What: the word must be the WHOLE of one shell reading of the token (a ref
/// position also splits a `src:dst` refspec at `:`, which no ref name may
/// carry), and [`reads_as_a_branch_name`] must hold. Anything else keeps the
/// deny, so a word this cannot place fails CLOSED.
/// Test: `denies_a_traversal_hidden_from_the_prefix_check_7557`,
/// `allows_a_git_ref_name_carrying_a_word_family`.
pub(crate) fn exempt_in_word_position(token: &str, word: &str, position: WordPosition) -> bool {
    let parts: Vec<&str> = match position {
        WordPosition::WordList => vec![token],
        // #7557: `git push origin HEAD:feat/x-secrets` names two refs.
        WordPosition::RefName => token.split(':').collect(),
    };
    let is_whole = parts
        .iter()
        .any(|part| scan_spellings(part).iter().any(|reading| reading == word));
    // #7557: a ref position takes ref PATTERNS (`git branch --list 'docs/x*'`)
    // and never prints a file, so its glob bytes stand for one literal byte;
    // a `.`-led component such as `.?` or `.*` is still refused.
    let shape = match position {
        WordPosition::WordList => word.to_string(),
        WordPosition::RefName => word.replace(['*', '?', '[', ']'], "x"),
    };
    is_whole && reads_as_a_branch_name(&shape)
}

/// Whether `word` is spelled the way `git check-ref-format` requires a ref name
/// to be (#7557).
///
/// Why: the branch-prefix allowlist says where a word LIVES only when nothing
/// in the word can move it. `git check-ref-format` already names every byte
/// and component shape that can: a component starting with `.` (`.`, `..`,
/// and the glob `.?`/`.*` that bash up to 5.1 expands onto `..`), a `..`
/// anywhere, a glob or escape byte, `~` and `^`. A word no ref could carry is
/// therefore not a ref, and treating it as a path is the fail-CLOSED answer.
/// What: `false` for an empty component, a component starting with `.` or
/// ending in `.lock`, a `..` anywhere, a trailing `.` or `/`, `@{`, a lone
/// `@`, and any of `~ ^ : ? * [ ] \ { }`, whitespace or a control byte. The
/// braces are git-legal but reach here only when brace expansion left them
/// unresolved, so they are refused rather than guessed at.
/// Test: `a_well_formed_ref_name_follows_check_ref_format_7557`.
pub(crate) fn is_a_well_formed_ref_name(word: &str) -> bool {
    const FORBIDDEN: &[char] = &['~', '^', ':', '?', '*', '[', ']', '\\', '{', '}'];
    if word.is_empty()
        || word == "@"
        || word.contains("..")
        || word.contains("@{")
        || word.ends_with('.')
        || word
            .chars()
            .any(|c| FORBIDDEN.contains(&c) || c.is_whitespace() || c.is_control())
    {
        return false;
    }
    word.split('/')
        .all(|c| !c.is_empty() && !c.starts_with('.') && !c.ends_with(".lock"))
}

/// Shell keywords whose `<var> in <words>` header is a WORD LIST (#7498).
///
/// Why: `for` and `select` are the two compound commands that bind a variable
/// to a list of literal words. Nothing in that list is handed to a program as
/// a path — the body decides what the variable is used for, and it reaches the
/// body as `$var`.
/// Test: `allows_a_for_loop_word_list_of_branch_names`.
const WORD_LIST_KEYWORDS: &[&str] = &["for", "select"];

/// Where a segment's `for`/`select` WORD LIST begins, if it is one (#7498).
///
/// Why: `split_shell_segments` cuts at `;`, so `for b in … ; do … ; done`
/// reaches this rule as a segment whose first token is the keyword `for`. The
/// scan then read every word after it as argv, and a git BRANCH name that
/// carries `secrets`/`credentials`/`token` refused the whole loop — live on tm
/// 1.5.33, `for b in feat/x fix/y docs/secrets-integration-spec; do …` was
/// refused for "naming" a branch this guard never opened (#7498).
/// What: the index just past `for <var> in`, when the segment lexes to that
/// header and runs no nested command. A nested command (`for f in $(ls …)`)
/// withdraws it, because the words are then whatever that command prints rather
/// than the literal list written here. `None` for every other segment, so no
/// ordinary argv reaches the narrowed shape test. The keyword is identified by
/// POSITION, exactly as [`pattern_argument_index`](super::pm_guard_secret_read::pattern_argument_index) and [`inline_program_indices`](super::pm_guard_secret_read::inline_program_indices)
/// identify theirs — this adds no verb to any list.
///
/// Round 13 critic MEDIUM: a header NESTED in an outer compound command keeps
/// the introducing keyword in front of it, because `split_shell_segments` cuts
/// at `;` and not at `do`. `for a in 1; do for b in <branch>; do …` and
/// `if true; then for b in <branch>; do …` therefore arrive as `do for b in …`
/// and `then for b in …`, which position 0 alone does not recognise. Those
/// [`LIST_INTRODUCERS`] are skipped first, so a nested header is read exactly
/// like a top-level one.
/// Test: `allows_a_for_loop_word_list_of_branch_names`,
/// `denies_a_secret_file_in_a_for_loop_word_list`.
pub(crate) fn for_word_list_start(segment: &str, argv: &[String]) -> Option<usize> {
    if NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m)) {
        return None;
    }
    let at = argv
        .iter()
        .position(|t| !LIST_INTRODUCERS.contains(&t.as_str()))?;
    if !WORD_LIST_KEYWORDS.contains(&argv.get(at)?.as_str()) || argv.get(at + 2)? != "in" {
        return None;
    }
    (argv.len() > at + 3).then_some(at + 3)
}

/// Shell keywords that only INTRODUCE a command list, carrying no operand.
///
/// Why: see [`for_word_list_start`] — `split_shell_segments` cuts at `;`, so a
/// nested `for` header reaches the scan behind the `do` or `then` of the
/// command that contains it.
/// Test: `allows_a_for_loop_word_list_of_branch_names`.
pub(crate) const LIST_INTRODUCERS: &[&str] = &["do", "then", "else", "elif", "{"];

/// The first path component of a conventional git BRANCH or REF name.
///
/// Why: round 13's first cut withdrew the directory-prefix proxy from every
/// word-family name in a word list, and its critic measured the bypass that
/// opened: `~/.aws/credentials`, `/var/run/secrets/kubernetes.io/serviceaccount/token`,
/// `/etc/secrets`, `vault/token` and `config/credentials` are the canonical
/// EXTENSIONLESS credential files, and each one ALLOWED through a `for` header
/// while the same operand written directly still denied — a bypass keyed on a
/// shell keyword, which is the rounds 1-to-4 failure mode in a new spelling.
/// The withdrawal therefore needs a positive reason to believe the word is a
/// ref, not merely the absence of file shape.
/// What: an ALLOWLIST, so an unrecognised first component keeps the deny and
/// the gate fails CLOSED. These seven are the conventional-commit branch
/// prefixes plus `refs`, the git ref namespace; none of them is an absolute
/// path or a `~` expansion, both of which leave a first component this list
/// cannot contain.
///
/// This is an accepted TRADE, not a claim that no credential file lives under
/// these seven — one can: `docs/api-secrets` and `release/gpg-secrets` deny in
/// argv and allow in a word list. The `DOCUMENTED_RESIDUALS` rows pin that gap
/// so its width is asserted rather than asserted-about (#7498 round 13
/// critic). What the trade buys is every branch-name loop, which is everyday
/// git work; what it costs is an extensionless file under a branch-shaped
/// directory, reachable only through the loop variable.
/// Test: `allows_a_for_loop_word_list_of_branch_names`,
/// `denies_a_secret_file_in_a_for_loop_word_list`,
/// `the_documented_residuals_still_allow`.
pub(crate) const BRANCH_NAME_PREFIXES: &[&str] =
    &["feat", "fix", "docs", "hotfix", "release", "chore", "refs"];

/// Whether `word`, inside a `for`/`select` word list, reads as a git BRANCH or
/// REF name rather than a path (#7498).
///
/// Why: [`names_a_secret_file`](super::pm_guard_secret_read::names_a_secret_file)'s last arm is the proxy the three English-word
/// families use for "this is a file rather than a word" — a `/` in the word.
/// That proxy misreads a branch: `docs/secrets-integration-spec` refused a
/// whole loop while the guard opened nothing. This predicate is the narrowest
/// reason to set the proxy aside, and it withdraws nothing that carries file
/// shape of its own.
/// What: four clauses, all required. No leading `.`, no extension and matched
/// only by `pm_guard_bash::matches_only_name_substring_family` are the three
/// tests [`names_a_secret_file`](super::pm_guard_secret_read::names_a_secret_file) takes BEFORE that last arm, inverted — so
/// `.env`, `secrets.txt`, `credentials.json`, `*.pem` and `id_rsa` are
/// untouched. The fourth is the positive evidence the critic's bypass corpus
/// showed was missing: the basename is not itself a bare family core
/// (`credentials`, `secrets`, `token`), the word TRAVERSES no `.` or `..`
/// component, and its FIRST component is a [`BRANCH_NAME_PREFIXES`] entry.
///
/// The traversal clause is round 13's second critic round. The allowlist reads
/// the word's SPELLING, so `feat/../secrets/prod-credentials` presented `feat`
/// as its first component while naming a path outside every prefix — it
/// ALLOWED on `acfb7c70a` while `cat feat/../secrets/prod-credentials` denied,
/// and the same loop without `feat/../` is a `WORD_LIST_BYPASS_CORPUS` row.
/// `docs/../secrets/prod-credentials`,
/// `feat/../../../../etc/db-credentials`, `refs/../../../var/run/my-secrets`
/// and `feat/../../.aws/aws-credentials` are the same escape. The prefix must
/// therefore say where the word LIVES, not merely how it starts. Rejecting
/// both dot components costs no legitimate loop: `git check-ref-format`
/// rejects `.` and `..` in a ref name, so no branch is spelled with either.
/// #7557 widens that clause to the whole of [`is_a_well_formed_ref_name`],
/// because `feat/.?/…` and `docs/.*/…` reach `..` through a glob.
/// Test: `reads_as_a_branch_name_needs_a_branch_prefix`,
/// `denies_a_secret_file_in_a_for_loop_word_list`,
/// `denies_a_traversal_hidden_from_the_prefix_check_7557`.
pub(crate) fn reads_as_a_branch_name(word: &str) -> bool {
    let base = normalize_bracket_classes(&command_basename(word));
    // #7533: the same three tests `names_a_secret_file` takes, inverted — so
    // the empty-extension reading of a trailing `.` is set aside here too.
    if base.starts_with('.')
        || has_a_named_extension(&base)
        || !matches_only_name_substring_family(&base)
    {
        return false;
    }
    // #7498 round 13 critic: `config/credentials` and `/etc/secrets` name the
    // canonical extensionless credential files, so a bare family core is never
    // a branch and an unlisted first component is never trusted.
    if ["credentials", "secrets", "token"].contains(&base.to_ascii_lowercase().as_str()) {
        return false;
    }
    let first_is_a_prefix = word
        .split('/')
        .next()
        .is_some_and(|first| BRANCH_NAME_PREFIXES.contains(&first));
    // #7498, #7557: a `..` after the prefix escapes it, and so does any
    // component a glob expands onto `..` — the ref-format check refuses both,
    // so the prefix says where the word LIVES rather than how it is spelled.
    first_is_a_prefix && is_a_well_formed_ref_name(word)
}

/// git subcommands whose positional arguments name a REF, never a path (#7498
/// round 3).
///
/// Why: `git branch <name>` and `git push <remote> <refspec>` take no path
/// operand at all — every positional is a branch, a tag, a remote or a
/// refspec. A branch called `docs/secrets-integration-spec` is then a "secret
/// file" on the strength of its `/`, which is the same proxy round 13 withdrew
/// from a `for` word list, reached through a different position.
/// What: compared against `pm_guard_bash::git_subcommand`'s answer, so
/// `git -C <path> branch …` resolves the same way. `git tag`, `git merge` and
/// every other ref-taking subcommand are deliberately ABSENT: an unlisted
/// subcommand keeps the pre-fix answer, which is the fail-CLOSED side.
/// Test: `allows_a_git_ref_name_carrying_a_word_family`,
/// `denies_a_secret_file_in_a_git_ref_position`.
const REF_NAMING_GIT_SUBCOMMANDS: &[&str] = &["branch", "push"];

/// `checkout`/`switch` flags whose next token is a NEW ref name (#7498 round 3).
///
/// Why: `git checkout` and `git switch` DO take paths — `git checkout main --
/// src/x` restores a file — so their positionals cannot be read as refs the way
/// [`REF_NAMING_GIT_SUBCOMMANDS`]'s are. The new-branch flag is what makes the
/// token after it a ref: `-b`/`-B` for `checkout`, `-c`/`-C` for `switch`.
/// Test: `allows_a_git_ref_name_carrying_a_word_family`,
/// `denies_a_secret_file_in_a_git_ref_position`.
pub(crate) const NEW_REF_FLAGS: &[&str] = &["-b", "-B", "-c", "-C"];

/// Where a segment's git REF names begin, if it names refs at all (#7498
/// round 3).
///
/// Why: live on tm 1.5.33, `git checkout -b feat/7526-secrets-manager-agent`,
/// `git switch -c feat/7527-tm-secrets-skill`, `git branch
/// feat/7527-tm-secrets-skill` and `git branch -m feat/7527-tm-secrets-skill`
/// were all refused for naming a secret-bearing file. A git ref argument is
/// never a file's bytes, and every child of an epic whose subject is secrets
/// wants that word in its branch name.
/// What: `Some(index)` — the first token that names a ref — for a
/// [`REF_NAMING_GIT_SUBCOMMANDS`] call, for a `checkout`/`switch` call at
/// the token after its [`NEW_REF_FLAGS`] spelling — or, with no such flag and
/// no `-p`/`--patch`, at its first operand (#8110) — and for `worktree add` at
/// its first operand (#7533). A nested command withdraws
/// it, exactly as it withdraws [`for_word_list_start`], because the words are
/// then whatever that command prints. `None` for every other segment, so no
/// ordinary argv reaches the narrowed shape test. The position is what is
/// identified, never the verb — this adds nothing to [`SAFE_HANDLING_VERBS`](super::pm_guard_secret_read::SAFE_HANDLING_VERBS) or
/// [`SAFE_GIT_SUBCOMMANDS`](super::pm_guard_secret_read::SAFE_GIT_SUBCOMMANDS), and the narrowing it enables is the single
/// predicate the word-list rule already uses ([`reads_as_a_branch_name`],
/// [`BRANCH_NAME_PREFIXES`] allowlist included), so `git checkout -b .env`,
/// `git push origin id_rsa` and `git checkout -b config/credentials` still
/// deny.
///
/// The `--` separator bounds BOTH halves of that answer, which the first cut
/// got only half right (#7498 round 3 critic MEDIUM). git reads every token
/// after `--` as a PATHSPEC, one spelled `-b` included, so scanning for the
/// new-branch flag across the whole tail let a flag BEHIND the separator open a
/// ref window over the real pathspec: `git checkout main -- -b docs/api-secrets`
/// ALLOWED while `git checkout main -- docs/api-secrets` denied. The search and
/// the result are now both confined to the tokens BEFORE the first `--`, so no
/// token at or after it can open a window — which also subsumes the earlier
/// separate `--` test.
///
/// The subcommand's index comes from `pm_guard_bash::git_argv_at_subcommand`,
/// which returns the argv and the index together. Re-deriving that index by
/// string equality is a second argv parse — the defect that helper's own doc
/// says it exists to prevent — and it mis-indexes a segment whose global option
/// value repeats the subcommand name (`git -C branch branch x`).
/// Test: `allows_a_git_ref_name_carrying_a_word_family`,
/// `denies_a_secret_file_in_a_git_ref_position`.
pub(crate) fn ref_name_start(segment: &str) -> Option<usize> {
    if NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m)) {
        return None;
    }
    // #7498 critic: one parser answers both "which subcommand" and "at which
    // index", so the two can never disagree.
    let (argv, at) = git_argv_at_subcommand(segment)?;
    let sub = argv.get(at)?.as_str();
    // #7498 critic: `--` ends the options; everything after it is a pathspec.
    let end_of_options = argv
        .iter()
        .enumerate()
        .skip(at + 1)
        .find(|(_, token)| *token == "--")
        .map_or(argv.len(), |(index, _)| index);
    let from = if REF_NAMING_GIT_SUBCOMMANDS.contains(&sub) {
        at + 1
    } else if sub == "worktree" && argv.get(at + 1).is_some_and(|t| t == "add") {
        // #7533: every operand of `git worktree add` is a new tree's PATH or a
        // commit-ish, and neither prints a file's bytes. `list`, `remove` and
        // `prune` are absent, so they keep the pre-fix answer.
        at + 2
    } else if matches!(sub, "checkout" | "switch") {
        // #7498: the new-branch flag is what makes the next token a ref, and
        // only a flag before the separator is a flag at all.
        let window = argv.get(at + 1..end_of_options)?;
        match window
            .iter()
            .position(|t| NEW_REF_FLAGS.contains(&t.as_str()))
        {
            Some(flag) => at + 2 + flag,
            // #8110: `switch` takes only refs, and `checkout` restores a path
            // without printing it unless `-p`/`--patch` shows the hunks. The
            // window is read only when no `--` exists, since every word from
            // `from` on is read in the ref position.
            None if end_of_options == argv.len() && !window.iter().any(|t| shows_a_patch(t)) => {
                at + 1
            }
            None => return None,
        }
    } else {
        return None;
    };
    (from < end_of_options).then_some(from)
}

/// Whether a `checkout` option token asks for patch mode, which prints hunks
/// (#8110): a short cluster carrying `p`, or any long option starting `--p`,
/// since git accepts an abbreviation such as `--pa` for `--patch`.
fn shows_a_patch(token: &str) -> bool {
    token.starts_with("--p")
        || (token.starts_with('-') && !token.starts_with("--") && token.contains('p'))
}

/// Flags whose next token is a human-readable TEXT PAYLOAD, never a path
/// (#7498 round 3).
///
/// Why: live on tm 1.5.33, a `gh issue comment 7517 --body …` whose prose named
/// a `.env` file was refused for naming `.env` in a `gh` command. The body is a
/// value written for a person to read; rewording it to "dotenv" let the
/// identical command through, which is the signature of a rule reading prose as
/// argv.
/// What: LONG spellings only, matched by exact token equality, plus the joined
/// `--body=…` form. Exact equality is what keeps the FILE flags out — a
/// `--body-file`, `--file` or `-F` token is not `--body`, so
/// `gh issue comment 1 --body-file .env` and `git commit -F .env` still deny.
/// `-m` is deliberately ABSENT even though `git commit -m` is the commonest
/// spelling of a message: a bare `-m` is overloaded across programs, and
/// `sort -m .env` MERGES and prints the files after it, so exempting the token
/// after every `-m` would be a bypass rather than a false-positive fix. That
/// keeps round 5's deliberate cost for `git commit -m "add .env"` in place.
///
/// The skip is VERB-AGNOSTIC by design, so `somecmd --body .env` allows for any
/// program. Two assumptions carry that, stated here so the next round need not
/// re-derive them (#7498 round 3 critic LOW):
///
/// 1. No program takes a FILE to read behind one of these four exact
///    spellings. A file variant is spelled differently — `--body-file`,
///    `--file`, `-F`, `--notes-file` — and exact equality keeps every one of
///    them screened. Keying on the verb instead would be the rounds-1-to-4
///    failure mode, so the flag list, not a program list, is what must stay
///    short.
/// 2. GNU `getopt_long` accepts any UNAMBIGUOUS abbreviation of a long option,
///    and this rule does not. That asymmetry is safe in one direction and not
///    the other. An abbreviation an agent writes (`--bod .env`) is not one of
///    these spellings, so it is still screened — over-refusal, the correct
///    side. The uncovered case is a program that defines ONLY a longer
///    file-reading option of which one of these four is a prefix (a
///    `--message-file` with no `--message`), where the program would read the
///    file while this rule reads the token as prose. No such spelling is known
///    in the tools an agent here drives; a reported one is a flag to REMOVE
///    from this list, never a program to exempt.
///
/// Test: `allows_a_filename_named_in_a_text_payload`,
/// `denies_a_file_flag_beside_a_text_payload`.
pub(crate) const TEXT_PAYLOAD_FLAGS: &[&str] = &["--body", "--title", "--message", "--note"];

/// Which tokens of `argv` are a human-readable TEXT PAYLOAD (#7498 round 3).
///
/// Why: see [`TEXT_PAYLOAD_FLAGS`]. Unlike [`ref_name_start`], a payload is
/// skipped OUTRIGHT rather than narrowed to one arm, because prose names a file
/// with its real spelling — `--body "the agent reads .env"` carries the dotfile
/// itself. That makes the flag list the whole safety argument, so it is exact
/// spellings of long options and nothing else.
/// What: the index of every token that is the joined `<flag>=…` form, and of
/// every token whose PREDECESSOR is an exact [`TEXT_PAYLOAD_FLAGS`] spelling.
/// Empty when the segment runs a nested command, so
/// `gh issue comment 1 --body "$(cat .env)"` still denies. Empty is also the
/// answer for every segment carrying none of the flags, which leaves the scan
/// exactly as it was. #9001: a [`lists_gh_issues`] call's `--search` value
/// is a payload too.
/// Test: `allows_a_filename_named_in_a_text_payload`,
/// `denies_a_file_flag_beside_a_text_payload`,
/// `a_gh_search_string_names_no_file_9001`.
pub(crate) fn text_payload_indices(segment: &str, argv: &[String]) -> Vec<usize> {
    if NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m)) {
        return Vec::new();
    }
    // #9001: `gh issue|pr list --search` takes a GitHub query, never a path.
    let search: &[&str] = if lists_gh_issues(argv) {
        &["--search"]
    } else {
        &[]
    };
    let is_flag = |token: &str| TEXT_PAYLOAD_FLAGS.contains(&token) || search.contains(&token);
    argv.iter()
        .enumerate()
        .filter(|(index, token)| {
            TEXT_PAYLOAD_FLAGS.iter().chain(search).any(|flag| {
                token
                    .strip_prefix(*flag)
                    .is_some_and(|rest| rest.starts_with('='))
            }) || index
                .checked_sub(1)
                .and_then(|prev| argv.get(prev))
                .is_some_and(|prev| is_flag(prev))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Whether `argv` runs `gh issue list` or `gh pr list` (#9001).
///
/// What: the program word past `strip_wrapper_prefix` is `gh` by basename,
/// followed directly by `issue`/`pr` and `list`. A gh extension or any other
/// subcommand keeps the argv scan.
/// Test: `a_gh_search_string_names_no_file_9001`.
pub(crate) fn lists_gh_issues(argv: &[String]) -> bool {
    strip_wrapper_prefix(argv).is_some_and(|at| {
        argv.get(at).is_some_and(|p| command_basename(p) == "gh")
            && argv.get(at + 1).is_some_and(|s| s == "issue" || s == "pr")
            && argv.get(at + 2).is_some_and(|s| s == "list")
    })
}
