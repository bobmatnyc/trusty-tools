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
//! Residual bypasses, stated rather than hidden (mirrors
//! [`super::destructive_delete`]'s own list):
//! - `TRUSTY_MPM_DISABLE_HOOKS` and `TRUSTY_MPM_PM_UNRESTRICTED=1` short-
//!   circuit this rule exactly like every other ABSOLUTE guard in this tree —
//!   that is pre-existing design (`pm_guard.rs:~341-347`), not a gap specific
//!   to this module. A `.claude/settings.json` write that sets either var is
//!   itself unblocked by this guard, so the PM or an exempt subagent can
//!   self-exempt via that path; tracked separately as issue #3981.
//! - `rsync`, `install`, `tar`, `scp`, and shell-builtin redirection
//!   (`cat secret.pem > worktree/secret.pem`) are different verbs, not
//!   covered here. The redirection case is partially covered separately by
//!   [`super::has_file_write_redirection`]'s PM-authorship rule, but that
//!   rule is not scoped to secret-shaped sources.
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
//! `allows_non_secret_named_sources`.

use std::path::Path;

use trusty_mpm::core::project_aliases::is_worktree_path;
use trusty_mpm::daemon::managed_routes::inproject::untracked_sync::glob_match;

use super::{PathEnv, resolve_target_path, split_shell_segments, unresolved_target};
use crate::commands::hook_rewrite::first_command_token;

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

/// The two verbs this guard scans every segment's tokens for.
const COPY_VERBS: &[&str] = &["cp", "mv"];

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

/// Whether `name` matches one of [`SECRET_BEARING_FILE_PATTERNS`], case-insensitively.
fn is_secret_bearing_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_BEARING_FILE_PATTERNS
        .iter()
        .any(|pattern| glob_match(&pattern.to_ascii_lowercase(), &lower))
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
fn expand_brace_alternatives(token: &str) -> Option<Vec<String>> {
    let Some(start) = token.find('{') else {
        return Some(vec![token.to_string()]);
    };
    let after_open = &token[start + 1..];
    let end_rel = after_open.find('}')?;
    let alternatives = &after_open[..end_rel];
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
fn is_secret_bearing_source(basename: &str) -> bool {
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
        if first_command_token(trimmed) == Some("cd") {
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
}
