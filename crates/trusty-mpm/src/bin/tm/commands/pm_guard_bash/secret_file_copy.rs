//! `tm hook --pm-guard` — secret-bearing file copy into a worktree (#7122).
//!
//! Why: issue #7122 — a `local-ops` agent `cp`'d a live, gitignored
//! `terraform.tfvars` into its session worktree to run a plan, then a
//! directory-wide `terraform fmt -check -diff` printed the file's contents,
//! credentials included, straight into the transcript. Prompt-level guidance
//! ("never copy a credential file into a worktree") is exactly the kind of
//! agent-discipline-only rule this guard tree exists to stop depending on
//! (see [`super::destructive_delete`]'s and [`super::worktree_remove`]'s doc
//! comments for the same argument). This module is the mechanical backstop:
//! a `cp`/`mv` whose SOURCE looks secret-shaped and whose DESTINATION resolves
//! into a harness worktree — or still carries a shell variable this guard
//! cannot resolve, so it cannot PROVE the destination is safe — is refused
//! outright, for the PM and any subagent alike — there is no legitimate
//! reason to shell-copy a credential file into a worktree, only to reference
//! it by absolute path (`-var-file`, `-state`, `--env-file`, …) or to use the
//! sanctioned, gitignore-verified channel
//! ([`trusty_mpm::daemon::managed_routes::inproject::untracked_sync::sync_untracked_files`],
//! which the daemon runs directly and never through this Bash guard at all).
//!
//! This is deliberately a DIFFERENT list from
//! [`trusty_mpm::core::trusty_tools_config::DEFAULT_UNTRACKED_SYNC_PATTERNS`]
//! even though both are "glob patterns naming files a worktree might need".
//! That list is an ALLOWLIST for a sanctioned, narrow, operator-declared sync
//! that verifies `git check-ignore` before writing a byte (#4733) — its
//! purpose is to make `.env*` land in every worktree by design.
//! [`SECRET_BEARING_FILE_PATTERNS`] below is a DENYLIST for an unsanctioned
//! raw shell copy an agent typed by hand, where nothing verifies anything —
//! so it names `.env*` too, deliberately overlapping in shape while denying
//! the opposite action. Merging the two into one list would either let the
//! sanctioned sync see `*.tfvars` and refuse to sync a `.env` an operator
//! legitimately declared, or let this guard stop denying `.env*` — neither is
//! what either caller wants. Only the matching FUNCTION
//! ([`trusty_mpm::daemon::managed_routes::inproject::untracked_sync::glob_match`],
//! promoted to `pub` for this reuse) is shared between them.
//!
//! What: [`evaluate_secret_file_copy_command`] scans every composition
//! segment (via [`super::split_shell_segments`]) for a `cp`/`mv` verb token —
//! scanning every token rather than only the first, matching
//! [`super::destructive_delete`]'s wrapper-resistance rationale — resolves
//! its last positional argument (after `--`/flag stripping) as the
//! destination and every other positional argument as a source, and denies
//! when any source's basename is secret-shaped
//! ([`is_secret_bearing_source`], which expands a brace alternation before
//! matching against [`SECRET_BEARING_FILE_PATTERNS`]) AND the destination
//! either resolves under a harness worktree ([`is_worktree_path`]) or still
//! carries an expansion [`resolve_target_path`] could not perform
//! ([`unresolved_target`]) — the guard cannot prove that destination
//! is NOT a worktree, and it must not allow what it cannot verify. Matching
//! is case-insensitive: a credential file named `Secrets.json` or
//! `AWS_Credentials` is exactly as real as a lowercase one, and filename
//! casing carries no security meaning worth missing a match over.
//!
//! A SECOND rule, added in issue #7266's fix round, answers a different
//! question about the same command and is deliberately not scoped to worktree
//! destinations at all. [`laundered_source_rename`] denies a secret-shaped
//! source reproduced under ANY destination basename the READ guard does not
//! also refuse — `cp terraform.tfvars secrets.rs`, `cp .env ./notes.txt`,
//! `cat .env > notes.md` — wherever it lands. Round 2 scoped this to the 35
//! extensions [`is_secret_read_target`]'s carve-out treats as transparent, and
//! `cp .env ./notes.txt` walked straight through it (#7266 round 3, critic
//! HIGH 3); the destination test now asks whether the read guard would refuse
//! the NEW name, which is the property that actually matters. Copying from a
//! main checkout, `$HOME` or `/tmp` (all outside [`is_worktree_path`]'s scope)
//! and then reading the copy was the one-move bypass this half closes: the read
//! guard cannot tell a laundered name from a real one, so the stop has to be
//! here.
//!
//! Four shapes are deliberately left alone, because none of them renames
//! anything: a destination that is itself unreadable (`cp .env .env.bak`), a
//! directory destination (`cp .env /tmp/`), and a source written as a bare WORD
//! rather than a path (`npm install token-bucket express`, `grep -rn
//! credentials src/ > out.md`) — see [`is_written_as_a_path`]. A destination
//! carrying an UNRESOLVABLE brace group (`cp .env 'note{s.txt'`) joins them for
//! the same reason: [`is_secret_read_target`] fails closed on it, so the read
//! guard refuses to print that name and nothing was laundered.
//!
//! Residual bypasses, stated rather than hidden (mirrors
//! [`super::destructive_delete`]'s own list):
//! - `TRUSTY_MPM_DISABLE_HOOKS` and `TRUSTY_MPM_PM_UNRESTRICTED=1` short-
//!   circuit this rule exactly like every other ABSOLUTE guard in this tree —
//!   that is pre-existing design (`pm_guard.rs:~341-347`), not a gap specific
//!   to this module. A `.claude/settings.json` write that sets either var is
//!   itself unblocked by this guard, so the PM or an exempt subagent can
//!   self-exempt via that path; tracked separately as issue #3981.
//! - `tar` and `scp` are different verbs, not covered by either rule.
//!   `rsync`, `install`, `ln` and shell redirection reach the RENAME rule
//!   ([`RENAME_VERBS`], [`redirection_target`]) but not the
//!   worktree-destination rule above, so `rsync secret.pem worktree/` keeps
//!   its own extension and stays allowed — closing that would need
//!   [`COPY_VERBS`] widened, which makes `npm install <pkg> <pkg>` in a
//!   worktree deny on a package name matching `token*`.
//! - Both halves of the rename rule read a source token only when
//!   [`is_written_as_a_path`] answers for it, so a bare secret-shaped WORD used
//!   as a pattern or a package name (`grep -rn credentials src/ > out.md`,
//!   `npm install token-bucket express`) is not treated as a file. A scoped npm
//!   package (`@scope/token-bucket`) is a bare word by that test too, which is
//!   why the test reads the basename's `.` rather than any `/` in the token.
//! - An unparseable segment (unbalanced quotes) is skipped rather than
//!   failing closed, unlike the sibling destructive-delete rule: every
//!   `rm`-denylisted target is catastrophic with no legitimate use, but
//!   `cp`/`mv` of a secret-shaped name has plenty of legitimate NON-worktree
//!   destinations, so blanket-denying every unparseable `cp`/`mv` would
//!   refuse far more ordinary work than it protects.
//! - Indirection through a shell variable or command substitution for the
//!   SOURCE argument is not resolved (the DESTINATION case is — see above).
//! - A directory copy (`cp -r secrets/ dest/`) is not inspected recursively —
//!   only the literal source token's basename is checked against the
//!   denylist.
//! - [`expand_brace_alternatives`] resolves only a single-level,
//!   comma-separated alternation (`secret.{tfvars,bak}`); a NESTED group
//!   (`{a,{b,c}}`) or an unbalanced `{`/`}` is not expanded — it fails closed
//!   (denied outright by [`is_secret_bearing_source`]) rather than allowing
//!   an unexamined source through, so this is a residual over-match, not a
//!   bypass. A Bash sequence expansion (`{1..3}`) is likewise not expanded as
//!   a sequence, but since its literal text still matches this guard's
//!   `*.<ext>` suffix patterns unchanged, it costs no coverage either way.
//!
//! Test: `denies_tfvars_copy_into_a_worktree`, `denies_dotenv_copy_into_a_worktree`,
//! `denies_pem_copy_into_a_worktree`, `denies_credentials_named_source`,
//! `denies_mv_of_a_secret_into_a_worktree`, `denies_case_insensitive_match`,
//! `allows_secret_copy_to_a_non_worktree_destination`,
//! `allows_ordinary_copy_into_a_worktree`,
//! `allows_secret_copy_hidden_in_an_unparseable_segment`,
//! `denies_secret_copy_behind_a_tracked_cd`,
//! `denies_brace_expanded_source_copy_into_a_worktree`,
//! `allows_brace_expanded_source_with_no_secret_alternative`,
//! `denies_source_with_an_unresolved_brace_group`,
//! `denies_secret_copy_to_a_destination_with_unresolved_variable`,
//! `denies_every_pattern_in_the_secret_bearing_list`,
//! `allows_non_secret_named_sources`,
//! `denies_a_secret_renamed_to_a_source_extension_outside_a_worktree`,
//! `denies_a_secret_renamed_to_a_source_extension_by_any_copy_verb`,
//! `denies_a_secret_redirected_into_a_markup_name`,
//! `allows_a_secret_copy_that_keeps_its_own_extension`,
//! `allows_a_grep_pattern_word_redirected_into_a_markup_name`,
//! `allows_renaming_an_ordinary_source_file_the_read_guard_already_prints`.
//! The rename rule
//! is proved WIRED end to end by
//! `pm_guard_denies_a_secret_copied_to_a_source_extension_name` in
//! `tests/tm_hook_pm_guard.rs`.

use std::path::Path;

use trusty_mpm::core::project_aliases::is_worktree_path;
use trusty_mpm::daemon::managed_routes::inproject::untracked_sync::glob_match;

use super::{PathEnv, resolve_target_path, split_shell_segments, unresolved_target};
use crate::commands::hook_rewrite::first_command_token;
// #7266 fix round: the READ guard's own target predicate, so "a name the read
// guard refuses" has ONE definition that both rules read.
use crate::commands::pm_guard_secret_read::is_secret_read_target;

/// Deny reason for a `cp`/`mv` of a secret-shaped source into a worktree (#7122).
pub(crate) const SECRET_FILE_COPY_REASON: &str = "`cp`/`mv` must not copy a secret-shaped file \
     (`*.tfvars`, `*.tfvars.json`, `*.tfstate`, `.env*`, `*.pem`, `*.key`, `.netrc`, `*.p12`, \
     `*.pfx`, `*.jks`, `*.kdbx`, `token*`, `*.ovpn`, an SSH private key `id_rsa`/`id_dsa`/\
     `id_ecdsa`/`id_ed25519` (any suffix), or a name containing `credentials`/`secrets`) into a \
     session worktree (issue #7122) — a subsequent directory-wide command (`terraform fmt`, \
     `cat`, a formatter) can print its contents, credentials included, into the transcript. \
     Reference the file by its absolute path instead (Terraform's `-var-file`/`-state`, a tool's \
     `--env-file`), or declare it in the operator's `untracked_sync` allowlist so the daemon \
     copies it through the gitignore-verified channel.";

/// The two verbs the worktree-destination rule scans every segment's tokens for.
const COPY_VERBS: &[&str] = &["cp", "mv"];

/// Verbs that can place a secret-shaped file's bytes under a NEW basename.
///
/// Why: the worktree-destination rule above answers only WHERE a secret lands.
/// The rename rule below answers what it lands AS, which is a wider question
/// with a wider verb class — `install`, `rsync` and `ln` each reproduce a file
/// under a name the caller chooses.
/// What: a separate list from [`COPY_VERBS`] on purpose. Widening that one
/// would make `npm install <pkg> <pkg>` inside a worktree deny whenever a
/// package name happens to match `token*`/`*secrets*`; the rename rule cannot
/// misfire that way, because since #7266 round 3 it fires only when the SOURCE
/// is written as a path ([`is_written_as_a_path`]), which a package name is not.
/// Test: `denies_a_secret_renamed_to_a_source_extension_by_any_copy_verb`,
/// `allows_an_npm_install_whose_package_name_is_secret_shaped`.
const RENAME_VERBS: &[&str] = &["cp", "mv", "install", "rsync", "ln"];

/// Filename glob patterns this guard treats as secret-bearing (issue #7122).
///
/// Why: the incident's own closure conditions name the terraform/`.env`/key
/// core of this list; the #7122 fix round widened it to the rest of the
/// common credential-file families a `local-ops`/`security` agent's working
/// directory can plausibly carry (`.netrc`, PKCS#12/JKS/KDBX key stores, VPN
/// profiles, bearer-token dumps) — the same reasoning as the original set,
/// just a longer enumeration of the same risk. See the module doc for why
/// this is a deliberately separate list from
/// [`trusty_mpm::core::trusty_tools_config::DEFAULT_UNTRACKED_SYNC_PATTERNS`]
/// rather than a reuse of it.
/// What: matched case-insensitively against a source argument's BASENAME via
/// [`glob_match`], after [`is_secret_bearing_source`] expands any brace
/// alternation the token carries. The four `id_*` entries are each scoped to
/// one SSH key family (`id_rsa*`, `id_dsa*`, `id_ecdsa*`, `id_ed25519*`)
/// rather than a bare `id_*`: the earlier bare form also matched ordinary
/// source files that merely share the `id_` prefix (`id_generator.rs`),
/// which is over-matching with no security benefit — unlike the intentional
/// `id_rsa.pub` over-match this list keeps (a public key, not a secret, but
/// costing nothing more than a redundant absolute-path reference).
const SECRET_BEARING_FILE_PATTERNS: &[&str] = &[
    "*.tfvars",
    "*.tfvars.json",
    "*.tfstate",
    "*.tfstate.backup",
    ".env",
    ".env.local",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa*",
    "id_dsa*",
    "id_ecdsa*",
    "id_ed25519*",
    "*credentials*",
    "*secrets*",
    ".netrc",
    "*.p12",
    "*.pfx",
    "*.jks",
    "*.kdbx",
    "token*",
    "*.ovpn",
];

/// The denylist entries that match on a NAME SUBSTRING rather than on a file
/// extension or a key-file prefix.
///
/// Why: `credentials`, `secrets` and `token` are ordinary English words, and
/// since #7266 round 5 the read rule refuses a secret-shaped name under EVERY
/// verb — so a bare word in a commit message, an `echo`, or a `grep` pattern
/// would deny if it were read as a filename. These three entries are the only
/// ones whose literal core is a word rather than a file spelling, so the read
/// rule asks for them by name.
/// What: a subset of [`SECRET_BEARING_FILE_PATTERNS`], pinned to it by
/// `name_substring_patterns_are_denylist_entries`.
/// Test: `name_substring_patterns_are_denylist_entries`,
/// `matches_only_name_substring_family_separates_word_families_from_file_families`.
const NAME_SUBSTRING_PATTERNS: &[&str] = &["*credentials*", "*secrets*", "token*"];

/// Final extensions that mark a file as a committed PLACEHOLDER rather than a
/// secret (#7479).
///
/// Why: `.env.*` classes every suffix as secret-bearing, so a tracked
/// `.env.example` — a file that exists to be read, copied and edited — could
/// not be reached by any verb, and a review-required change to one was handed
/// back to the operator. These three extensions are a naming CONVENTION for
/// "this holds key names and no key values", the same kind of convention
/// `is_ssh_public_key_name`'s `.pub` already reads.
/// What: the extension is honoured only as the FINAL one, so
/// `.env.example.bak` is still a `.env.*` secret.
/// Deliberate trade: a file that really does hold a credential and is named
/// `*.example` reads freely. A guard keyed on names cannot tell those apart,
/// and refusing every placeholder to catch a misnamed secret is the
/// over-blocking #7266 round 4 already had to reverse.
/// Test: `placeholder_suffixes_are_readable`, `real_dotenv_files_still_deny`,
/// `a_placeholder_suffix_is_honoured_only_as_the_final_extension`.
const PLACEHOLDER_SUFFIXES: &[&str] = &["example", "sample", "template"];

/// Whether `name`'s final extension marks it as a placeholder (#7479).
///
/// Why: the one place the placeholder convention is decided, so the read rule
/// and the `cp`/`mv` rule can never disagree about `.env.example`.
/// What: `true` when the lowercased final extension is a
/// [`PLACEHOLDER_SUFFIXES`] entry.
/// Test: see [`PLACEHOLDER_SUFFIXES`].
fn is_placeholder_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| PLACEHOLDER_SUFFIXES.contains(&e.as_str()))
}

/// Whether `name` is matched by the denylist AND only by its word-shaped
/// families ([`NAME_SUBSTRING_PATTERNS`]).
///
/// Why: the read rule needs to tell `id_rsa` — a filename and nothing else —
/// from `secrets`, which is a word an agent writes all day. The first must be
/// screened wherever it appears; the second only when it is written as a path.
/// What: `true` when at least one denylist pattern matches and every pattern
/// that matches is a [`NAME_SUBSTRING_PATTERNS`] member; `false` when nothing
/// matches, and `false` as soon as an extension- or prefix-typed family matches
/// too (`credentials.pem` is a `*.pem`, not a word).
/// Test: `matches_only_name_substring_family_separates_word_families_from_file_families`.
// #7266 round 5: `pub(crate)` so the read rule reads this list rather than
// re-listing the three word families at its own scope.
pub(crate) fn matches_only_name_substring_family(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let mut matched = false;
    for pattern in SECRET_BEARING_FILE_PATTERNS {
        if pattern_reaches(pattern, &lower) {
            if !NAME_SUBSTRING_PATTERNS.contains(pattern) {
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// Whether ONE denylist entry reaches `lower_candidate` — the per-entry
/// question [`secret_pattern_overlaps`] and [`any_pattern_overlaps`] ask of the
/// whole list (#7498).
///
/// Why: [`matches_only_name_substring_family`] decided WHICH family produced a
/// deny by literal matching, while the deny itself was produced by GLOB
/// overlap. The two disagreed on every glob: `s*` denied because it overlaps
/// the word family `*secrets*`'s core, but matched no entry literally, so the
/// shape gate read it as a file family and skipped the rule that a word family
/// counts only when written as a path. A one-character regex fragment therefore
/// refused `gh issue list --search` and the BASE-ENGINEER file-size precheck.
/// Asking one question of one entry is what keeps the two halves in agreement.
/// What: a literal [`glob_match`], then — for a candidate carrying a wildcard —
/// [`globs_overlap`] against the entry's [`pattern_literal_core`], and against
/// the FULL entry when the candidate carries the `?` [`glob_match`] does not
/// implement. That is exactly the union the two list-level functions apply.
/// Test: `a_one_character_glob_fragment_does_not_name_a_secret_family`,
/// `matches_only_name_substring_family_separates_word_families_from_file_families`.
fn pattern_reaches(pattern: &str, lower_candidate: &str) -> bool {
    let lower_pattern = pattern.to_ascii_lowercase();
    if glob_match(&lower_pattern, lower_candidate) {
        return true;
    }
    if !lower_candidate.bytes().any(|b| b == b'*' || b == b'?') {
        return false;
    }
    let core = pattern_literal_core(&lower_pattern);
    if !core.is_empty() && globs_overlap(lower_candidate.as_bytes(), core.as_bytes()) {
        return true;
    }
    lower_candidate.contains('?')
        && globs_overlap(lower_candidate.as_bytes(), lower_pattern.as_bytes())
}

/// Whether `name` matches one of [`SECRET_BEARING_FILE_PATTERNS`], case-insensitively.
fn is_secret_bearing_name(name: &str) -> bool {
    // #7479: a placeholder name is not a secret under any family.
    if is_placeholder_name(name) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    SECRET_BEARING_FILE_PATTERNS
        .iter()
        .any(|pattern| glob_match(&pattern.to_ascii_lowercase(), &lower))
}

/// The literal core of a denylist entry — the pattern with every `*` removed.
///
/// Why: `*.pem` -> `.pem`, `id_rsa*` -> `id_rsa`, `*credentials*` ->
/// `credentials`, `.env.*` -> `.env.`. A caller's GLOB is screened against
/// these cores rather than against the full entries; see
/// [`secret_pattern_overlaps`] for why.
fn pattern_literal_core(pattern: &str) -> String {
    pattern.replace('*', "")
}

/// Whether `candidate` — a caller-supplied GLOB, or a literal basename — names
/// a file in [`SECRET_BEARING_FILE_PATTERNS`]'s classes.
///
/// Why: #7266 round 3, critic CRITICAL 1. [`is_secret_bearing_name`] compares a
/// literal name against the denylist, so a caller's PATTERN was screened as if
/// it were a filename: `Grep(glob = "*.env")` matched no entry and was allowed,
/// while `glob = "*.pem"` was denied only because that entry happens to carry a
/// `*` in the same place.
///
/// Round 3 answered that with a sound two-pattern OVERLAP test, and overlap is
/// too much: four entries (`*credentials*`, `*secrets*`, `token*`, `.env.*`)
/// carry unbounded wildcards, so they intersect nearly every extension glob an
/// agent writes. That build denied `Grep(glob = "*.toml")` — and `*.json`,
/// `*.txt`, `*.log`, `*.csv`, `*.tf`, `*test*` — because `credentials.toml` and
/// `.env.log` are names those families really do reach (#7266 round 4, measured
/// against the round-3 binary). Taxing every ordinary tree search is a worse
/// outcome than the leak it prevents.
/// What: a candidate carrying no wildcard is [`is_secret_bearing_name`],
/// unchanged and sound — that is the path every `cp`/`mv`/read operand takes. A
/// candidate carrying `*` or `?` is additionally screened against each entry's
/// [`pattern_literal_core`]: it denies when it MATCHES a core, which is the
/// narrower question of whether the glob targets a credential family by name.
/// `*.env` matches the core `.env` and denies; `*.toml` matches no core and
/// allows.
/// Test: `every_pattern_family_is_reachable_by_its_natural_glob`,
/// `a_glob_denies_only_when_it_targets_a_family_by_name`,
/// `overlap_answers_where_literal_matching_did_not`.
pub(crate) fn secret_pattern_overlaps(candidate: &str) -> bool {
    let lower = candidate.to_ascii_lowercase();
    // #7479: `*.example`/`*.sample`/`*.template`, literal or glob, is a
    // placeholder and reaches only placeholder names.
    if is_placeholder_name(&lower) {
        return false;
    }
    if is_secret_bearing_name(&lower) {
        return true;
    }
    if !lower.bytes().any(|b| b == b'*' || b == b'?') {
        return false;
    }
    SECRET_BEARING_FILE_PATTERNS.iter().any(|pattern| {
        let core = pattern_literal_core(&pattern.to_ascii_lowercase());
        !core.is_empty() && globs_overlap(lower.as_bytes(), core.as_bytes())
    })
}

/// Whether two globs can both match the same string.
///
/// What: a bottom-up dynamic program over the two patterns' suffixes.
/// `dp[i][j]` answers for `a[i..]` against `b[j..]`: a `*` on either side either
/// matches nothing (advance that side) or one character (advance the other
/// side), and two literal positions must agree unless one is a `?`. A side that
/// has run out is satisfiable only when the other side's remainder is all `*`.
/// `?` is honoured on both sides even though [`glob_match`] does not implement
/// it, so a caller glob spelled with one over-matches rather than slipping past.
/// [`secret_pattern_overlaps`] passes a wildcard-free literal core on the `b`
/// side, which reduces this to "does `a` match that core"; the two-sided form is
/// kept because it is what makes `?` answerable at all.
/// Test: `overlap_answers_where_literal_matching_did_not`,
/// `a_glob_denies_only_when_it_targets_a_family_by_name`.
fn globs_overlap(a: &[u8], b: &[u8]) -> bool {
    let (la, lb) = (a.len(), b.len());
    let width = lb + 1;
    let mut dp = vec![false; (la + 1) * width];
    dp[la * width + lb] = true;
    for j in (0..lb).rev() {
        dp[la * width + j] = b[j] == b'*' && dp[la * width + j + 1];
    }
    for i in (0..la).rev() {
        dp[i * width + lb] = a[i] == b'*' && dp[(i + 1) * width + lb];
        for j in (0..lb).rev() {
            let hit = if a[i] == b'*' || b[j] == b'*' {
                dp[(i + 1) * width + j] || dp[i * width + j + 1]
            } else {
                (a[i] == b[j] || a[i] == b'?' || b[j] == b'?') && dp[(i + 1) * width + j + 1]
            };
            dp[i * width + j] = hit;
        }
    }
    dp[0]
}

/// Whether `candidate` can name the same file as any denylist entry, comparing
/// the two as GLOBS on both sides.
///
/// Why: #7266 round 6. [`secret_pattern_overlaps`] compares a candidate against
/// each entry's wildcard-free [`pattern_literal_core`], which reduces to "does
/// the candidate match that core" — and a candidate whose LITERAL part sits
/// outside the core is then unreachable. `terraform.tfvar?` is `*.tfvars` with
/// one character wildcarded, names the file exactly, and matched no core, so it
/// ALLOWED. Comparing against the full entries answers it, which is round 3's
/// sound-but-too-wide test; the read rule therefore asks this question only for
/// a candidate carrying `?`, the one metacharacter
/// [`is_secret_bearing_name`]'s matcher does not implement.
/// What: [`globs_overlap`] against each full entry, case-insensitively.
/// Test: `pm_guard_secret_read::tests::denies_a_glob_that_expands_onto_a_secret_file`,
/// `single_character_wildcards_reach_the_full_entries`.
pub(crate) fn any_pattern_overlaps(candidate: &str) -> bool {
    let lower = candidate.to_ascii_lowercase();
    // #7479: same placeholder exemption the sibling matchers apply, so the
    // `?` arm cannot reintroduce the deny the `*` arm just withdrew.
    if is_placeholder_name(&lower) {
        return false;
    }
    SECRET_BEARING_FILE_PATTERNS
        .iter()
        .any(|pattern| globs_overlap(lower.as_bytes(), pattern.to_ascii_lowercase().as_bytes()))
}

/// A token with a process-substitution wrapper removed (#7266 round 3).
///
/// Why: critic CRITICAL 2 — `shlex::split("diff <(cat .env) /dev/null")` yields
/// the tokens `<(cat` and `.env)`, so every basename rule in this guard tree saw
/// `.env)`, a name no pattern matches, while `diff .env /dev/null` denied.
/// Stripping the wrapper is what lets the operand rules see the real path.
/// What: removes a leading `<(`, `>(` or `(`, then every trailing `)`.
/// Test: `strips_a_process_substitution_wrapper`,
/// `denies_a_secret_read_through_process_substitution`.
pub(crate) fn strip_process_substitution(token: &str) -> &str {
    let inner = token
        .strip_prefix("<(")
        .or_else(|| token.strip_prefix(">("))
        .or_else(|| token.strip_prefix('('))
        .unwrap_or(token);
    inner.trim_end_matches(')')
}

/// Expand a simple, non-nested `{a,b,c}` brace alternation in `token`.
///
/// Why: `shlex::split` tokenizes a Bash command literally — it does not
/// perform the shell's own brace expansion — so `cp secret.{tfvars,bak}
/// worktree/` arrives as the single literal source token
/// `secret.{tfvars,bak}`, which matched none of
/// [`SECRET_BEARING_FILE_PATTERNS`] even though a real shell would copy BOTH
/// `secret.tfvars` (denylisted) and `secret.bak` (not) — critic finding on
/// the original #7122 PR.
/// What: recursively expands every `{comma,separated,alternative}` group in
/// `token`, left to right, returning every resulting literal string. A token
/// with no `{` returns `Some(vec![token])` unchanged. `None` — never a
/// partial answer — when a group is unbalanced (no matching `}`) or nested (a
/// `{` inside a `{…}` group's alternatives): both are residual shapes the
/// module doc's bypass list names, and [`is_secret_bearing_source`] treats
/// `None` as secret-shaped rather than guessing.
/// Test: `denies_brace_expanded_source_copy_into_a_worktree`,
/// `allows_brace_expanded_source_with_no_secret_alternative`,
/// `denies_source_with_an_unresolved_brace_group`.
// #7266 round 3: `pub(crate)` so the read guard expands a caller's brace group
// with THIS expander before screening each alternative — `*.{rs,ts}` must be
// judged as `*.rs` and `*.ts`, not as one literal whose extension is `{rs,ts}`.
pub(crate) fn expand_brace_alternatives(token: &str) -> Option<Vec<String>> {
    let Some(start) = token.find('{') else {
        return Some(vec![token.to_string()]);
    };
    let after_open = &token[start + 1..];
    let end_rel = after_open.find('}')?;
    let alternatives = &after_open[..end_rel];
    // #7499: a group with no `,` is not an alternation, so a shell leaves it
    // literal — `{{.Id}}` names the file `{{.Id}}`, never `.Id`. Keeping the
    // braces is therefore the FAITHFUL reading, not a relaxation: the group
    // reaches no name it did not already reach.
    if !alternatives.contains(',') {
        let after_close = start + 1 + end_rel + 1;
        let head = &token[..after_close];
        let tails = expand_brace_alternatives(&token[after_close..])?;
        return Some(tails.iter().map(|tail| format!("{head}{tail}")).collect());
    }
    if alternatives.contains('{') {
        // Nested group — not a shape this expander resolves; caller fails closed.
        return None;
    }
    let prefix = &token[..start];
    let suffix = &after_open[end_rel + 1..];
    let suffix_candidates = expand_brace_alternatives(suffix)?;
    let mut out = Vec::new();
    for alt in alternatives.split(',') {
        for tail in &suffix_candidates {
            out.push(format!("{prefix}{alt}{tail}"));
        }
    }
    Some(out)
}

/// Whether a `cp`/`mv` source basename is secret-shaped, expanding a brace
/// alternation first.
///
/// Why: see [`expand_brace_alternatives`]'s doc for the gap this closes.
/// What: `true` when [`is_secret_bearing_name`] matches ANY brace-expanded
/// candidate, OR when [`expand_brace_alternatives`] returns `None` (a brace
/// shape it could not resolve) — that residual case fails closed rather than
/// letting an unexamined source through.
/// Test: see [`expand_brace_alternatives`]'s test list.
// #7266: `pub(crate)` so the read guard
// (`crate::commands::pm_guard_secret_read`) screens a `sed`/`head`/`grep` READ
// against THIS list rather than growing a second one — one classifier, two
// rules, per the common-entry-point convention.
pub(crate) fn is_secret_bearing_source(basename: &str) -> bool {
    match expand_brace_alternatives(basename) {
        Some(candidates) => candidates.iter().any(|c| is_secret_bearing_name(c)),
        None => true,
    }
}

/// Classify a Bash command for a secret-shaped `cp`/`mv` into a worktree:
/// `Some(reason)` denies, `None` allows.
///
/// Why: the one entry point `pm_guard` calls, kept to the same
/// process-environment-reading wrapper shape as the sibling ABSOLUTE guards
/// so the policy underneath stays testable without touching `std::env`.
/// Test: see the module doc's test list.
pub(crate) fn evaluate_secret_file_copy_command(command: &str, cwd: &Path) -> Option<String> {
    evaluate_secret_file_copy_command_in(command, cwd, &PathEnv::from_process())
}

/// [`evaluate_secret_file_copy_command`] against an explicit environment —
/// see [`PathEnv`] for why production and tests must not share `std::env`
/// mutation.
fn evaluate_secret_file_copy_command_in(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
) -> Option<String> {
    let mut effective_cwd = cwd.to_path_buf();
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Same `cd`-tracking shape as the sibling ABSOLUTE guards: a
        // deliberate, partial closing of `cd worktree && cp secret .`.
        if first_command_token(trimmed).as_deref() == Some("cd") {
            if let Some(argv) = shlex::split(trimmed)
                && let Some(dest) = argv.get(1)
            {
                effective_cwd = resolve_target_path(dest, &effective_cwd, env);
            }
            continue;
        }
        // Unparseable segment: skip rather than fail closed — see the module
        // doc's residual-bypass list for why this differs from
        // `destructive_delete`'s blanket refusal.
        let Some(argv) = shlex::split(trimmed) else {
            continue;
        };
        // #7266 fix round: the rename rule runs FIRST and ignores the
        // destination's scope entirely — see `laundered_source_rename`.
        if let Some(reason) = laundered_source_rename(&argv) {
            return Some(reason);
        }
        let Some(verb_idx) = argv
            .iter()
            .position(|tok| COPY_VERBS.contains(&tok.strip_prefix('\\').unwrap_or(tok)))
        else {
            continue;
        };
        let tail = &argv[verb_idx + 1..];
        let positional = positional_args(tail);
        // `cp`/`mv` need at least one source and one destination; anything
        // shorter (a bare `cp --help`, a typo) names no copy this rule cares
        // about.
        let Some((dest_token, sources)) = positional.split_last() else {
            continue;
        };
        if sources.is_empty() {
            continue;
        }
        let dest_path = resolve_target_path(dest_token, &effective_cwd, env);
        // #7122 fix round (critic finding): a destination like `"$WT/live.tfvars"`
        // resolves to a literal `$WT` path component that `is_worktree_path`
        // correctly answers `false` for, since it lexically does not look
        // like a worktree — but the guard cannot prove it ISN'T one either.
        // Treat an unresolved destination variable the same way
        // `main_checkout`/`worktree_remove` already do: deny, naming what
        // could not be established, rather than allow what cannot be
        // verified.
        // #7234: a leading `~` left literal by an unset `$HOME` is the same
        // shape as `$WT` and gets the same refusal.
        let dest_variable = unresolved_target(&dest_path).map(|u| u.token);
        if !is_worktree_path(&dest_path) && dest_variable.is_none() {
            continue;
        }
        for source in sources {
            let basename = Path::new(source)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or(source.as_str());
            if !is_secret_bearing_source(basename) {
                continue;
            }
            return Some(match &dest_variable {
                Some(variable) => unresolved_destination_deny_reason(source, dest_token, variable),
                None => SECRET_FILE_COPY_REASON.to_string(),
            });
        }
    }
    None
}

/// Deny reason for a secret-shaped source whose destination still carries an
/// unresolved shell variable (issue #7122 fix round, critic finding).
///
/// Why: [`resolve_target_path`] expands only `$TMPDIR`, `$TMP`, `$HOME` and
/// `$PWD` — anything else (`$WT`, `$MAIN`, an operator's own variable)
/// survives as a literal path component, and [`is_worktree_path`] correctly
/// answers `false` for a directory that does not lexically look like a
/// worktree, e.g. `/repo/$WT/live.tfvars`. Denying only a PROVEN worktree
/// destination let `cp secret.tfvars "$WT/live.tfvars"` through unexamined —
/// the same shape [`super::main_checkout`] and [`super::worktree_remove`]
/// already refuse to guess about, via the same [`unresolved_target`] check
/// (which since #7234 also answers for a `~` an unset `$HOME` left literal).
/// What: names the source, the destination token as written, and the
/// unresolved variable, then points at the remedy.
/// Test: `denies_secret_copy_to_a_destination_with_unresolved_variable`.
fn unresolved_destination_deny_reason(source: &str, dest_token: &str, variable: &str) -> String {
    format!(
        "`cp`/`mv` must not copy the secret-shaped source `{source}` to a destination that still \
         carries the unexpanded shell variable `{variable}` (`{dest_token}`) (issue #7122) — the \
         guard expands only `$TMPDIR`, `$TMP`, `$HOME` and `$PWD`, so it cannot prove this lands \
         outside a session worktree, and it must not allow what it cannot verify. Re-run the \
         command with the destination path written out in full, or reference the file by its \
         absolute path instead of copying it."
    )
}

/// Strip leading flags (and honor a `--` end-of-flags marker) from a `cp`/`mv`
/// argument tail, returning the remaining positional arguments in order.
///
/// Why: `cp -rp secret.pem worktree/` must resolve to the same two positional
/// arguments as `cp secret.pem worktree/` — the flags carry no path
/// information this rule needs. A local copy of
/// `destructive_delete::delete_targets`'s non-`find` branch: kept separate
/// rather than extracted into a shared helper so this new, security-relevant
/// module never perturbs that one's existing test surface (#7122 review).
fn positional_args(tail: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut positional_only = false;
    for tok in tail {
        if !positional_only && tok == "--" {
            positional_only = true;
            continue;
        }
        if !positional_only && tok.starts_with('-') && tok.len() > 1 {
            continue;
        }
        out.push(tok.clone());
    }
    out
}

/// The basename of a path token, with a process-substitution wrapper and a
/// leading `\` quote removed.
fn token_basename(token: &str) -> &str {
    // #7266 round 3: `<(cat .env)` reaches this rule as the token `.env)`.
    let token = strip_process_substitution(token);
    let token = token.strip_prefix('\\').unwrap_or(token);
    Path::new(token)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(token)
}

/// Whether `token` lexically names a DIRECTORY, so a copy into it keeps the
/// source's own basename and renames nothing.
fn names_a_directory(token: &str) -> bool {
    let trimmed = token.trim_end_matches('/');
    token.ends_with('/') || trimmed.is_empty() || trimmed == "." || trimmed == ".."
}

/// Whether `basename` is one of the four SSH private-key families.
///
/// Why: those are the only credential filenames in
/// [`SECRET_BEARING_FILE_PATTERNS`] normally written with no `.` at all, so
/// [`is_written_as_a_path`] would otherwise read `id_rsa` as a bare word.
fn is_ssh_private_key_name(basename: &str) -> bool {
    let lower = basename.to_ascii_lowercase();
    ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"]
        .iter()
        .any(|family| lower.starts_with(family))
}

/// Whether `token` is written the way a FILE PATH is written.
///
/// Why: since #7266 round 3 the rename rule fires on every destination, so this
/// is the only thing between it and an ordinary command whose ARGUMENT merely
/// matches the denylist by shape — `npm install token-bucket express` and
/// `grep -rn credentials src/ > out.md` both name a secret-shaped bare WORD
/// that is a package name and a search pattern, not a file.
/// What: a leading `/`, `./`, `../` or `~`, a `.` anywhere in the basename, or
/// an SSH private-key name. A bare word carries none of those.
/// Test: `allows_a_grep_pattern_word_redirected_into_a_markup_name`,
/// `allows_an_npm_install_whose_package_name_is_secret_shaped`.
fn is_written_as_a_path(token: &str) -> bool {
    let stripped = strip_process_substitution(token);
    let basename = token_basename(stripped);
    stripped.starts_with('/')
        || stripped.starts_with("./")
        || stripped.starts_with("../")
        || stripped.starts_with('~')
        || basename.contains('.')
        || is_ssh_private_key_name(basename)
}

/// Whether `token` names a file the READ guard would refuse to print.
///
/// Why: the rename rule is about moving bytes from a name that cannot be read
/// into one that can. A source the read guard ALREADY prints is not being
/// laundered, so `mv crates/trusty-audit/src/grounding/secrets.rs renamed.rs`
/// — an ordinary source file whose name matches the `*secrets*` pattern — must
/// not deny. This is [`crate::commands::pm_guard_secret_read`]'s own target
/// predicate, called directly rather than reassembled here, so the two rules
/// can never disagree about which names are unreadable.
fn names_an_unreadable_secret(token: &str) -> bool {
    is_secret_read_target(token)
}

/// Whether `token` names an unreadable secret AND is written as a file path.
fn names_an_unreadable_secret_file(token: &str) -> bool {
    is_written_as_a_path(token) && names_an_unreadable_secret(token)
}

/// A secret-shaped file about to be reproduced under a name the read guard
/// prints freely: `Some(reason)` denies, `None` allows (issue #7266 fix round).
///
/// Why: [`crate::commands::pm_guard_secret_read`] narrows its own file class by
/// its private `has_transparent_source_extension`, so `cat secrets.rs` is allowed by
/// design — the alternative denied 24 ordinary tracked files. That carve-out
/// held only while nothing could put a credential INTO such a name, and this
/// module's other rule scopes its destination check to worktree paths. From a
/// main checkout, `$HOME` or `/tmp`, `cp terraform.tfvars secrets.rs` therefore
/// passed both rules and `cat secrets.rs` then printed the credential. Closing
/// it on the copy side is the half that cannot be routed around: the read guard
/// cannot tell a laundered `.rs` from a real one.
/// What: denies when a [`RENAME_VERBS`] invocation reproduces a secret-shaped
/// source under a single destination whose basename is NOT itself one the read
/// guard refuses, or when an output redirection (`> notes.txt`, `tee out.md`)
/// writes such a name in a segment that also names a secret-shaped FILE.
/// Destination SCOPE is not consulted at all — a rename out of the read guard's
/// file class is the leak wherever it lands.
/// Both halves require the SOURCE to be one the read guard actually refuses
/// ([`names_an_unreadable_secret`]) and written as a path
/// ([`is_written_as_a_path`]), so renaming an ordinary `secrets.rs` and
/// installing a `token-bucket` package are untouched. A destination that keeps
/// a secret-shaped name (`cp .env .env.bak`) and a directory destination
/// (`cp .env /tmp/`) rename nothing and are likewise untouched.
/// Test: `denies_a_secret_renamed_to_a_source_extension_outside_a_worktree`,
/// `denies_a_secret_renamed_to_a_source_extension_by_any_copy_verb`,
/// `denies_a_secret_renamed_to_an_unsuspicious_name`,
/// `denies_a_secret_renamed_through_a_brace_group`,
/// `denies_a_secret_redirected_into_a_markup_name`,
/// `allows_a_secret_copy_that_keeps_its_own_extension`,
/// `allows_a_secret_copy_into_a_directory`,
/// `allows_an_npm_install_whose_package_name_is_secret_shaped`,
/// `allows_a_grep_pattern_word_redirected_into_a_markup_name`,
/// `allows_renaming_an_ordinary_source_file_the_read_guard_already_prints`.
fn laundered_source_rename(argv: &[String]) -> Option<String> {
    renamed_by_a_copy_verb(argv).or_else(|| renamed_by_a_redirection(argv))
}

/// The positional operands a copy verb will really see, with every resolvable
/// brace group expanded into separate words (#7266 round 4).
///
/// Why: bash expands `{a,b}` before the verb runs, so one shlex token can be two
/// operands. `cp {terraform.tfvars,notes.txt}` reached
/// [`renamed_by_a_copy_verb`] as a single positional, left no source behind the
/// destination, and was allowed.
/// What: flat-maps [`expand_brace_alternatives`]; a token whose braces that
/// expander cannot resolve is kept verbatim, which is what leaves it to the
/// fail-closed [`is_secret_read_target`] downstream.
/// Test: `denies_a_secret_renamed_through_a_brace_group`.
fn brace_expanded_positionals(positional: &[String]) -> Vec<String> {
    positional
        .iter()
        .flat_map(|token| expand_brace_alternatives(token).unwrap_or_else(|| vec![token.clone()]))
        .collect()
}

/// The [`RENAME_VERBS`] half of [`laundered_source_rename`].
fn renamed_by_a_copy_verb(argv: &[String]) -> Option<String> {
    let verb_idx = argv
        .iter()
        .position(|tok| RENAME_VERBS.contains(&tok.strip_prefix('\\').unwrap_or(tok)))?;
    let verb = argv[verb_idx].strip_prefix('\\').unwrap_or(&argv[verb_idx]);
    // #7266 round 4: bash expands a brace group into SEPARATE words before the
    // verb ever runs, so `cp {terraform.tfvars,notes.txt}` is `cp
    // terraform.tfvars notes.txt`. `shlex::split` leaves it as one token, which
    // arrived here as a lone positional with no source behind it and allowed the
    // rename outright. Expanding first is what makes the operand split match
    // what the shell will actually do.
    let positional = brace_expanded_positionals(&positional_args(&argv[verb_idx + 1..]));
    let (dest, sources) = positional.split_last()?;
    // A directory destination keeps each source's own basename, so nothing is
    // renamed. The source COUNT is not a proxy for that: `positional_args` does
    // not consume a flag's value, so `install -m 600 server.pem key.rs` arrives
    // with `600` sitting among the sources.
    if sources.is_empty() || names_a_directory(dest) {
        return None;
    }
    // #7266 round 3 (critic HIGH 3): the destination test is now "is this name
    // one the read guard also refuses", not "does it end in a source
    // extension" — `cp .env ./notes.txt` laundered a credential just as well.
    if names_an_unreadable_secret(dest) {
        return None;
    }
    let source = sources.iter().find(|source| {
        names_an_unreadable_secret_file(source) && source.as_str() != dest.as_str()
    })?;
    Some(source_rename_deny_reason(
        source,
        dest,
        &format!("`{verb}`"),
    ))
}

/// The redirection half of [`laundered_source_rename`].
///
/// Why: `cat .env > notes.txt` and `tee out.md` reproduce the file through the
/// shell rather than a copy verb. The read guard already refuses the `cat` that
/// feeds the first shape, but a rule that depends on the neighbour firing first
/// is not a rule.
/// What: the source must be a token [`names_an_unreadable_secret_file`] answers
/// for. A bare word is excluded on purpose: `grep -rn credentials src/ > out.md`
/// names a secret-shaped PATTERN, not a file, and denying it would tax ordinary
/// work the read guard is careful to allow.
fn renamed_by_a_redirection(argv: &[String]) -> Option<String> {
    let dest = redirection_target(argv)?;
    if names_a_directory(&dest) || names_an_unreadable_secret(&dest) {
        return None;
    }
    let source = argv
        .iter()
        .find(|tok| names_an_unreadable_secret_file(tok))?;
    Some(source_rename_deny_reason(
        source,
        &dest,
        "a shell redirection",
    ))
}

/// The file an output redirection or a `tee` in `argv` writes, if any.
///
/// What: answers for the separated (`> f`, `>> f`, `2> f`, `&> f`) and attached
/// (`>f`, `>|f`) spellings, and for `tee [flags] f`. Flags between the operand
/// marker and the file are skipped so `tee -a notes.md` resolves to the file.
fn redirection_target(argv: &[String]) -> Option<String> {
    let mut expect_operand = false;
    for tok in argv {
        if expect_operand {
            if tok.starts_with('-') && tok.len() > 1 {
                continue;
            }
            return Some(tok.clone());
        }
        let bare = tok.strip_prefix('\\').unwrap_or(tok);
        if token_basename(bare) == "tee" {
            expect_operand = true;
            continue;
        }
        match redirect_tail(bare) {
            Some("") => expect_operand = true,
            Some(attached) => return Some(attached.to_string()),
            None => {}
        }
    }
    None
}

/// The text a redirect token carries after its operator: `Some("")` for the
/// operator alone, `Some(target)` when the file is attached, `None` when the
/// token is not an output redirection at all.
fn redirect_tail(token: &str) -> Option<&str> {
    let (fd, rest) = token.split_once('>')?;
    if !(fd.is_empty() || fd == "&" || fd.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let rest = rest.strip_prefix('>').unwrap_or(rest);
    Some(rest.strip_prefix('|').unwrap_or(rest))
}

/// Deny reason for a secret-shaped source about to land under a name the read
/// guard prints freely (issue #7266 fix round).
fn source_rename_deny_reason(source: &str, dest: &str, how: &str) -> String {
    format!(
        "reproducing the secret-shaped file `{source}` as `{dest}` through {how} is refused \
         (issue #7266) — `{dest}` is not itself a name the READ guard refuses, so `cat {dest}`, a \
         line range of it and the `Read` tool would all print the credential the guard just \
         refused to print from `{source}`. This holds for every destination BASENAME outside that \
         file class, whatever its extension and whether or not it lands in a worktree, because \
         the read guard cannot tell a laundered name from a real one. A copy that keeps a \
         secret-shaped name (`cp .env .env.bak`) and a copy into a directory (`cp .env /tmp/`) \
         are untouched. Reference the file by its absolute path instead (`-var-file`, `-state`, \
         `--env-file`)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> PathEnv {
        PathEnv {
            tmpdir: None,
            tmp: None,
            home: Some("/Users/agent".to_string()),
        }
    }

    fn eval(command: &str) -> Option<String> {
        evaluate_secret_file_copy_command_in(
            command,
            Path::new("/repo/.claude/worktrees/agent-x"),
            &env(),
        )
    }

    #[test]
    fn denies_tfvars_copy_into_a_worktree() {
        assert!(
            eval("cp ~/live/terraform.tfvars .claude/worktrees/agent-x/terraform.tfvars").is_some()
        );
    }

    #[test]
    fn denies_dotenv_copy_into_a_worktree() {
        assert!(eval("cp /repo/.env /repo/.claude/worktrees/agent-x/.env").is_some());
    }

    #[test]
    fn denies_pem_copy_into_a_worktree() {
        assert!(
            eval("cp -p /Users/agent/keys/server.pem /repo/.claude/worktrees/agent-x/").is_some()
        );
    }

    #[test]
    fn denies_credentials_named_source() {
        assert!(
            eval("cp /Users/agent/.aws/credentials /repo/.claude/worktrees/agent-x/credentials")
                .is_some()
        );
    }

    #[test]
    fn denies_mv_of_a_secret_into_a_worktree() {
        assert!(eval("mv /tmp/id_rsa /repo/.claude/worktrees/agent-x/id_rsa").is_some());
    }

    #[test]
    fn denies_case_insensitive_match() {
        assert!(
            eval("cp /Users/agent/Secrets.JSON /repo/.claude/worktrees/agent-x/Secrets.JSON")
                .is_some()
        );
    }

    #[test]
    fn allows_secret_copy_to_a_non_worktree_destination() {
        assert!(eval("cp /repo/.env /Users/agent/backup/.env").is_none());
    }

    #[test]
    fn allows_ordinary_copy_into_a_worktree() {
        assert!(eval("cp README.md /repo/.claude/worktrees/agent-x/README.md").is_none());
    }

    #[test]
    fn allows_secret_copy_hidden_in_an_unparseable_segment() {
        // Unbalanced quote: this rule skips rather than fails closed (see
        // the module doc's residual-bypass list), unlike the sibling
        // destructive-delete rule.
        assert!(eval("cp 'unterminated .claude/worktrees/agent-x/.env").is_none());
    }

    #[test]
    fn denies_secret_copy_behind_a_tracked_cd() {
        assert!(
            evaluate_secret_file_copy_command_in(
                "cd /repo/.claude/worktrees/agent-x && cp /repo/.env .env",
                Path::new("/repo"),
                &env(),
            )
            .is_some()
        );
    }

    #[test]
    fn glob_match_is_case_insensitive_via_lowercasing() {
        assert!(is_secret_bearing_name("MY.TFVARS.tfvars"));
        assert!(is_secret_bearing_name("terraform.TFVARS"));
        assert!(!is_secret_bearing_name("README.md"));
    }

    // --- #7122 fix round: brace expansion (critic HIGH finding) -----------

    #[test]
    fn denies_brace_expanded_source_copy_into_a_worktree() {
        // `shlex::split` never expands the brace, so without expansion this
        // arrives as one literal token matching no pattern in
        // `SECRET_BEARING_FILE_PATTERNS`.
        assert!(
            eval("cp secret.{tfvars,bak} .claude/worktrees/agent-x/").is_some(),
            "one brace alternative (`secret.tfvars`) is secret-shaped, so the whole copy denies"
        );
    }

    #[test]
    fn allows_brace_expanded_source_with_no_secret_alternative() {
        assert!(
            eval("cp notes.{md,txt} .claude/worktrees/agent-x/").is_none(),
            "neither brace alternative is secret-shaped"
        );
    }

    #[test]
    fn denies_source_with_an_unresolved_brace_group() {
        // Unbalanced brace: `expand_brace_alternatives` returns `None`, and
        // `is_secret_bearing_source` fails closed on that residual case.
        assert!(eval("cp 'notes.{md,txt' .claude/worktrees/agent-x/").is_some());
        // Nested group: also `None`, also denied.
        assert!(eval("cp 'notes.{md,{txt,csv}}' .claude/worktrees/agent-x/").is_some());
    }

    // --- #7122 fix round: unresolved destination variable (critic HIGH) ---

    #[test]
    fn denies_secret_copy_to_a_destination_with_unresolved_variable() {
        // `$WT` is not one of the four variables `resolve_target_path`
        // expands, so it survives as a literal path component and
        // `is_worktree_path` answers `false` for it — the guard must deny
        // anyway, because it cannot prove the destination is safe.
        assert!(
            evaluate_secret_file_copy_command_in(
                "cp secret.tfvars \"$WT/live.tfvars\"",
                Path::new("/repo"),
                &env(),
            )
            .is_some()
        );
    }

    #[test]
    fn allows_ordinary_copy_to_a_destination_with_unresolved_variable() {
        // The unresolved-variable rule only fires for a secret-shaped
        // source — an ordinary file copied to an unresolved destination is
        // still out of scope for this guard.
        assert!(
            evaluate_secret_file_copy_command_in(
                "cp README.md \"$WT/README.md\"",
                Path::new("/repo"),
                &env(),
            )
            .is_none()
        );
    }

    // --- #7122 fix round: extended pattern list (security hardening) ------

    #[test]
    fn denies_every_pattern_in_the_secret_bearing_list() {
        let cases: &[(&str, &str)] = &[
            ("*.tfvars", "prod.tfvars"),
            ("*.tfvars.json", "prod.tfvars.json"),
            ("*.tfstate", "terraform.tfstate"),
            ("*.tfstate.backup", "terraform.tfstate.backup"),
            (".env", ".env"),
            (".env.local", ".env.local"),
            (".env.*", ".env.production"),
            ("*.pem", "server.pem"),
            ("*.key", "server.key"),
            ("id_rsa*", "id_rsa"),
            ("id_dsa*", "id_dsa"),
            ("id_ecdsa*", "id_ecdsa_sk"),
            ("id_ed25519*", "id_ed25519.pub"),
            ("*credentials*", "aws_credentials.json"),
            ("*secrets*", "app_secrets.yaml"),
            (".netrc", ".netrc"),
            ("*.p12", "client.p12"),
            ("*.pfx", "client.pfx"),
            ("*.jks", "keystore.jks"),
            ("*.kdbx", "vault.kdbx"),
            ("token*", "token.json"),
            ("*.ovpn", "client.ovpn"),
        ];
        assert_eq!(
            cases.len(),
            SECRET_BEARING_FILE_PATTERNS.len(),
            "every pattern in the denylist needs exactly one covering case here"
        );
        for (pattern, sample) in cases {
            assert!(
                eval(&format!("cp {sample} .claude/worktrees/agent-x/{sample}")).is_some(),
                "pattern `{pattern}` sample `{sample}` should have denied"
            );
        }
    }

    #[test]
    fn allows_non_secret_named_sources() {
        for name in ["id_generator.rs", "README.md"] {
            assert!(
                eval(&format!("cp {name} .claude/worktrees/agent-x/{name}")).is_none(),
                "`{name}` shares no shape with `SECRET_BEARING_FILE_PATTERNS` and must not deny"
            );
        }
    }

    // --- #7266 fix round: the copy-to-transparent-name seam (critic CRITICAL) ---

    /// Evaluate from a MAIN CHECKOUT — the cwd that made this seam reachable.
    fn eval_outside_a_worktree(command: &str) -> Option<String> {
        evaluate_secret_file_copy_command_in(command, Path::new("/repo"), &env())
    }

    #[test]
    fn denies_a_secret_renamed_to_a_source_extension_outside_a_worktree() {
        // Fails on 11c08ed44: `/repo` is not a worktree path, so the
        // destination check above skipped the segment, and `cat secrets.rs`
        // then passed the read guard's transparent-extension carve-out.
        let reason = eval_outside_a_worktree("cp terraform.tfvars secrets.rs").expect("denies");
        assert!(reason.contains("terraform.tfvars"), "{reason}");
        assert!(reason.contains("secrets.rs"), "{reason}");
        assert!(reason.contains("#7266"), "{reason}");
        // The same command from $HOME and /tmp, the other two out-of-scope cwds.
        for cwd in ["/Users/agent", "/tmp"] {
            assert!(
                evaluate_secret_file_copy_command_in(
                    "cp /Users/agent/live/terraform.tfvars notes.md",
                    Path::new(cwd),
                    &env(),
                )
                .is_some(),
                "cwd `{cwd}` must deny"
            );
        }
    }

    #[test]
    fn denies_a_secret_renamed_to_a_source_extension_by_any_copy_verb() {
        for verb in RENAME_VERBS {
            let command = format!("{verb} /Users/agent/.aws/credentials app_config.rs");
            assert!(
                eval_outside_a_worktree(&command).is_some(),
                "`{command}` must deny"
            );
        }
        // A flag between the verb and the operands does not hide them.
        assert!(eval_outside_a_worktree("install -m 600 server.pem key.rs").is_some());
        assert!(eval_outside_a_worktree("ln -s /etc/app/.env config.ts").is_some());
    }

    #[test]
    fn denies_a_secret_redirected_into_a_markup_name() {
        for command in [
            "cat .env > notes.md",
            "cat .env >> notes.md",
            "cat .env >notes.md",
            "tee notes.md < .env",
            "tee -a report.md < /repo/infra/terraform.tfvars",
            "base64 server.pem &> dump.txt.md",
        ] {
            assert!(
                eval_outside_a_worktree(command).is_some(),
                "`{command}` must deny"
            );
        }
    }

    #[test]
    fn allows_a_secret_copy_that_keeps_its_own_extension() {
        // Today's verdict for a backup copy outside a worktree: allowed. The
        // rename rule fires on the DESTINATION's extension, and `.tfvars` is
        // not one the read guard prints.
        assert_eq!(
            eval_outside_a_worktree("cp terraform.tfvars backup.tfvars"),
            None
        );
        assert_eq!(eval_outside_a_worktree("cp /repo/.env /tmp/.env.bak"), None);
    }

    #[test]
    fn allows_a_grep_pattern_word_redirected_into_a_markup_name() {
        // `credentials` here is a PATTERN, not a file — the read guard is
        // careful to allow this shape and the rename rule must not tax it.
        for command in [
            "grep -rn credentials src/ > out.md",
            "grep -rn secrets crates/ >> audit.md",
            "cargo test 2> failures.md",
        ] {
            assert_eq!(eval_outside_a_worktree(command), None, "`{command}`");
        }
    }

    #[test]
    fn allows_renaming_an_ordinary_source_file_the_read_guard_already_prints() {
        // Both sides of these renames are files `cat` prints today, so nothing
        // is laundered. Denying them would tax the 24 tracked files whose names
        // carry `credentials`/`secrets`/`token` — the exact over-match the read
        // guard's carve-out exists to avoid.
        for command in [
            "mv crates/trusty-audit/src/grounding/secrets.rs grounding/redacted.rs",
            "cp docs/design/UI/design-system/tokens.css tokens.scss",
            "mv website/src/lib/theme/tokens.test.ts tokens.spec.ts",
        ] {
            assert_eq!(eval_outside_a_worktree(command), None, "`{command}`");
        }
    }

    // --- #7266 round 3: pattern overlap (critic CRITICAL 1) ---------------

    #[test]
    fn every_pattern_family_is_reachable_by_its_natural_glob() {
        // One natural caller glob per denylist family. On ec8ea341d the three
        // literal-only entries (`.env`, `.env.local`, `.netrc`) answered
        // `false` for every glob spelling, because a caller pattern was
        // compared as though it were a filename.
        let cases: &[(&str, &str)] = &[
            ("*.tfvars", "*.tfvars"),
            ("*.tfvars.json", "*.tfvars.json"),
            ("*.tfstate", "*.tfstate"),
            ("*.tfstate.backup", "*.tfstate.*"),
            (".env", "*.env"),
            (".env.local", "*.local"),
            (".env.*", ".env*"),
            ("*.pem", "*.pem"),
            ("*.key", "*.key"),
            ("id_rsa*", "id_*"),
            ("id_dsa*", "id_d*"),
            ("id_ecdsa*", "id_ec*"),
            ("id_ed25519*", "id_ed*"),
            ("*credentials*", "*credentials*"),
            ("*secrets*", "*secret*"),
            (".netrc", "*.netrc"),
            ("*.p12", "*.p12"),
            ("*.pfx", "*.pfx"),
            ("*.jks", "*.jks"),
            ("*.kdbx", "*.kdbx"),
            ("token*", "token*"),
            ("*.ovpn", "*.ovpn"),
        ];
        assert_eq!(
            cases.len(),
            SECRET_BEARING_FILE_PATTERNS.len(),
            "every pattern in the denylist needs exactly one covering glob here"
        );
        for (pattern, glob) in cases {
            let core = pattern_literal_core(pattern);
            assert!(
                globs_overlap(glob.as_bytes(), core.as_bytes()),
                "glob `{glob}` must target the `{pattern}` family by name (core `{core}`)"
            );
            assert!(secret_pattern_overlaps(glob), "glob `{glob}` must deny");
        }
    }

    #[test]
    fn single_character_wildcards_reach_the_full_entries() {
        // #7266 round 6: `secret_pattern_overlaps` compares against each
        // entry's wildcard-free CORE, so a candidate whose literal part sits
        // outside the core answers false however exactly it names the file.
        for candidate in [
            "terraform.tfvar?",
            "terraform.tfstat?",
            "server.pe?",
            "vault.kdb?",
        ] {
            assert!(
                !secret_pattern_overlaps(candidate),
                "`{candidate}` is the gap this exists to close"
            );
            assert!(
                any_pattern_overlaps(candidate),
                "`{candidate}` must overlap"
            );
        }
        // Two-sided overlap, not a substring test: an ordinary `?` glob is
        // still unreachable from every entry.
        for candidate in ["file?.txt", "core.?", "notes?.md", "??.rs"] {
            assert!(
                !any_pattern_overlaps(candidate),
                "`{candidate}` must not overlap"
            );
        }
    }

    #[test]
    fn a_glob_denies_only_when_it_targets_a_family_by_name() {
        // #7266 round 4. Round 3 screened a caller glob by sound pattern
        // OVERLAP, and every one of these denied against that binary because
        // `credentials.toml`, `.env.log` and friends are names the four
        // unbounded families really do reach. They are also the globs an agent
        // writes all day, so the screen now asks whether the glob targets a
        // family by NAME instead.
        for glob in [
            "*.toml",
            "*.json.map",
            "*.txt",
            "*.log",
            "*.csv",
            "*.tf",
            "*test*",
            "*.rs",
            "*.md",
            "**/*",
            "*.yaml",
            "*.snap",
            "src/**",
            "*.html",
        ] {
            assert!(
                !secret_pattern_overlaps(glob),
                "ordinary glob `{glob}` must not be screened as a secret"
            );
        }
        // `*.json` is the one extension glob that stays denied, and it earns it:
        // `*.tfvars.json` is a credential family spelled in JSON.
        assert!(secret_pattern_overlaps("*.json"));
    }

    #[test]
    fn name_substring_patterns_are_denylist_entries() {
        for pattern in NAME_SUBSTRING_PATTERNS {
            assert!(
                SECRET_BEARING_FILE_PATTERNS.contains(pattern),
                "`{pattern}` must be spelled exactly as its denylist entry"
            );
        }
    }

    #[test]
    fn matches_only_name_substring_family_separates_word_families_from_file_families() {
        // Word-shaped: an agent writes these in prose all day.
        for word in [
            "credentials",
            "secrets",
            "token",
            "tokens",
            "my-credentials",
        ] {
            assert!(
                matches_only_name_substring_family(word),
                "`{word}` is matched only by a word-shaped family"
            );
        }
        // File-shaped: a spelling that is a filename and nothing else.
        for name in [
            "id_rsa",
            "id_ed25519",
            ".netrc",
            "live.tfvars",
            "server.pem",
        ] {
            assert!(
                !matches_only_name_substring_family(name),
                "`{name}` names a file family, not a word"
            );
        }
        // A word family that ALSO hits an extension family is a file.
        assert!(!matches_only_name_substring_family("credentials.pem"));
        // Nothing matches at all.
        assert!(!matches_only_name_substring_family("README"));
        assert!(!matches_only_name_substring_family("Cargo.toml"));
    }

    #[test]
    fn overlap_answers_where_literal_matching_did_not() {
        // The two shapes the critic drove live against the round-2 binary.
        assert!(secret_pattern_overlaps("*.env"));
        assert!(secret_pattern_overlaps("*.netrc"));
        assert!(
            !is_secret_bearing_name("*.env"),
            "the literal test misses it"
        );
        assert!(!is_secret_bearing_name("*.netrc"));
        // A literal name still takes the sound `is_secret_bearing_name` path, so
        // the read guard's transparent-extension narrowing is what keeps
        // `credentials.rs` readable — not this predicate.
        assert!(secret_pattern_overlaps("credentials.rs"));
        assert!(secret_pattern_overlaps("terraform.tfvars"));
        // A literal name that can reach no family stays clear.
        assert!(!secret_pattern_overlaps("Cargo.toml"));
        assert!(!secret_pattern_overlaps("README.md"));
    }

    #[test]
    fn strips_a_process_substitution_wrapper() {
        assert_eq!(strip_process_substitution("<(cat"), "cat");
        assert_eq!(strip_process_substitution(">(tee"), "tee");
        assert_eq!(strip_process_substitution(".env)"), ".env");
        assert_eq!(strip_process_substitution("/repo/.env))"), "/repo/.env");
        assert_eq!(strip_process_substitution("README.md"), "README.md");
    }

    // --- #7266 round 3: laundering to ANY destination (critic HIGH 3) ------

    #[test]
    fn denies_a_secret_renamed_to_an_unsuspicious_name() {
        // The critic's exact bypass: `.txt` is in none of the read guard's
        // transparent extensions, so round 2 allowed this and `cat notes.txt`
        // afterwards.
        let reason = eval_outside_a_worktree("cp .env ./notes.txt").expect("denies");
        assert!(reason.contains(".env"), "{reason}");
        assert!(reason.contains("notes.txt"), "{reason}");
        for command in [
            "cp .env notes.txt",
            "mv /repo/infra/terraform.tfvars vars.bin",
            "cp ~/.ssh/id_rsa ./deploy_key_backup",
            "cat .env > notes.txt",
            "tee dump.log < /repo/.env",
        ] {
            assert!(
                eval_outside_a_worktree(command).is_some(),
                "`{command}` must deny"
            );
        }
    }

    #[test]
    fn allows_a_secret_copy_into_a_directory() {
        // A directory destination keeps the source's own basename, so the read
        // guard still refuses the copy.
        for command in [
            "cp /repo/.env /Users/agent/backup/",
            "cp /repo/infra/terraform.tfvars .",
            "mv server.pem ..",
        ] {
            assert_eq!(eval_outside_a_worktree(command), None, "`{command}`");
        }
    }

    #[test]
    fn allows_an_npm_install_whose_package_name_is_secret_shaped() {
        // `install` is a rename verb and `token-bucket` matches `token*`, but a
        // bare word is a package name, not a file.
        for command in [
            "npm install token-bucket token-bucket",
            "npm install token-bucket express",
            "npm install @scope/token-bucket express",
            "npm install --save-dev secrets-manager lodash",
        ] {
            assert_eq!(eval_outside_a_worktree(command), None, "`{command}`");
        }
    }

    #[test]
    fn denies_a_secret_renamed_through_a_brace_group() {
        // #7266 round 4: bash expands this to `cp terraform.tfvars notes.txt`,
        // but `shlex::split` keeps it as ONE token, so the rename rule saw a
        // destination with no source behind it and allowed the laundering.
        for command in [
            "cp {terraform.tfvars,notes.txt}",
            "mv {.env,notes.md}",
            "cp {/repo/.env,/tmp/dump.bin}",
        ] {
            assert!(
                eval_outside_a_worktree(command).is_some(),
                "`{command}` must deny"
            );
        }
        // Expansion must not invent a rename where bash makes none.
        for command in [
            "cp {a,b}.rs dist/",
            "cp Cargo{,.bak}.toml",
            "mv {README,NOTES}.md docs/",
        ] {
            assert_eq!(eval_outside_a_worktree(command), None, "`{command}`");
        }
    }

    #[test]
    fn denies_a_secret_copied_through_process_substitution() {
        // `shlex::split` yields `<(cat` and `.env)`; the wrapper comes off
        // before the basename match.
        assert!(eval_outside_a_worktree("cp <(cat .env) notes.txt").is_some());
    }

    #[test]
    fn redirection_target_reads_both_spellings() {
        let argv = |s: &str| shlex::split(s).expect("lexes");
        assert_eq!(
            redirection_target(&argv("cat .env > notes.md")).as_deref(),
            Some("notes.md")
        );
        assert_eq!(
            redirection_target(&argv("cat .env >>notes.md")).as_deref(),
            Some("notes.md")
        );
        assert_eq!(
            redirection_target(&argv("tee -a notes.md")).as_deref(),
            Some("notes.md")
        );
        assert_eq!(redirection_target(&argv("cat .env")), None);
        // A `2>&1` fd-dup names no file this rule cares about.
        assert_eq!(
            redirection_target(&argv("cargo test 2>&1")).as_deref(),
            Some("&1")
        );
    }
}
