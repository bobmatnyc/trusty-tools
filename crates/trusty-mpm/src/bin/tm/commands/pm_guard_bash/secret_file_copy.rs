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
//! into a harness worktree is refused outright, for the PM and any subagent
//! alike — there is no legitimate reason to shell-copy a credential file into
//! a worktree, only to reference it by absolute path (`-var-file`, `-state`,
//! `--env-file`, …) or to use the sanctioned, gitignore-verified channel
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
//! ([`untracked_sync::glob_match`], promoted to `pub` for this reuse) is
//! shared between them.
//!
//! What: [`evaluate_secret_file_copy_command`] scans every composition
//! segment (via [`super::split_shell_segments`]) for a `cp`/`mv` verb token —
//! scanning every token rather than only the first, matching
//! [`super::destructive_delete`]'s wrapper-resistance rationale — resolves
//! its last positional argument (after `--`/flag stripping) as the
//! destination and every other positional argument as a source, and denies
//! when the destination resolves under a harness worktree
//! ([`is_worktree_path`]) AND any source's basename matches
//! [`SECRET_BEARING_FILE_PATTERNS`]. Case-insensitive: a credential file
//! named `Secrets.json` or `AWS_Credentials` is exactly as real as a
//! lowercase one, and filename casing carries no security meaning worth
//! missing a match over.
//!
//! Residual bypasses, stated rather than hidden (mirrors
//! [`super::destructive_delete`]'s own list):
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
//! - Indirection through a shell variable or command substitution for either
//!   the source or destination argument is not resolved.
//! - A directory copy (`cp -r secrets/ dest/`) is not inspected recursively —
//!   only the literal source token's basename is checked against the
//!   denylist.
//!
//! Test: `denies_tfvars_copy_into_a_worktree`, `denies_dotenv_copy_into_a_worktree`,
//! `denies_pem_copy_into_a_worktree`, `denies_credentials_named_source`,
//! `denies_mv_of_a_secret_into_a_worktree`, `denies_case_insensitive_match`,
//! `allows_secret_copy_to_a_non_worktree_destination`,
//! `allows_ordinary_copy_into_a_worktree`,
//! `allows_secret_copy_hidden_in_an_unparseable_segment`,
//! `denies_secret_copy_behind_a_tracked_cd`.

use std::path::Path;

use trusty_mpm::core::project_aliases::is_worktree_path;
use trusty_mpm::daemon::managed_routes::inproject::untracked_sync::glob_match;

use super::{PathEnv, resolve_target_path, split_shell_segments};
use crate::commands::hook_rewrite::first_command_token;

/// Deny reason for a `cp`/`mv` of a secret-shaped source into a worktree (#7122).
pub(crate) const SECRET_FILE_COPY_REASON: &str = "`cp`/`mv` must not copy a secret-shaped file \
     (`*.tfvars`, `*.tfstate`, `.env*`, `*.pem`, `*.key`, `id_*`, or a name containing \
     `credentials`/`secrets`) into a session worktree (issue #7122) — a subsequent directory-wide \
     command (`terraform fmt`, `cat`, a formatter) can print its contents, credentials included, \
     into the transcript. Reference the file by its absolute path instead (Terraform's \
     `-var-file`/`-state`, a tool's `--env-file`), or declare it in the operator's \
     `untracked_sync` allowlist so the daemon copies it through the gitignore-verified channel.";

/// The two verbs this guard scans every segment's tokens for.
const COPY_VERBS: &[&str] = &["cp", "mv"];

/// Filename glob patterns this guard treats as secret-bearing (issue #7122).
///
/// Why: the incident's own closure conditions name this exact list. See the
/// module doc for why this is a deliberately separate list from
/// [`trusty_mpm::core::trusty_tools_config::DEFAULT_UNTRACKED_SYNC_PATTERNS`]
/// rather than a reuse of it.
/// What: matched case-insensitively against a source argument's BASENAME via
/// [`glob_match`]. `id_*` is deliberately broad enough to also catch
/// `id_rsa.pub` (a public key, not a secret) — over-matching a public key
/// costs nothing but a redundant absolute-path reference, and the guard's
/// bias is the same one [`super::destructive_delete`] states explicitly: a
/// missed secret is the dangerous direction, not an over-cautious allow.
const SECRET_BEARING_FILE_PATTERNS: &[&str] = &[
    "*.tfvars",
    "*.tfstate",
    "*.tfstate.backup",
    ".env",
    ".env.local",
    ".env.*",
    "*.pem",
    "*.key",
    "id_*",
    "*credentials*",
    "*secrets*",
];

/// Whether `name` matches one of [`SECRET_BEARING_FILE_PATTERNS`], case-insensitively.
fn is_secret_bearing_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_BEARING_FILE_PATTERNS
        .iter()
        .any(|pattern| glob_match(&pattern.to_ascii_lowercase(), &lower))
}

/// Classify a Bash command for a secret-shaped `cp`/`mv` into a worktree:
/// `Some(reason)` denies, `None` allows.
///
/// Why: the one entry point `pm_guard` calls, kept to the same
/// process-environment-reading wrapper shape as the sibling ABSOLUTE guards
/// so the policy underneath stays testable without touching `std::env`.
/// Test: see the module doc's test list.
pub(crate) fn evaluate_secret_file_copy_command(command: &str, cwd: &Path) -> Option<&'static str> {
    evaluate_secret_file_copy_command_in(command, cwd, &PathEnv::from_process())
}

/// [`evaluate_secret_file_copy_command`] against an explicit environment —
/// see [`PathEnv`] for why production and tests must not share `std::env`
/// mutation.
fn evaluate_secret_file_copy_command_in(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
) -> Option<&'static str> {
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
        if !is_worktree_path(&dest_path) {
            continue;
        }
        for source in sources {
            let basename = Path::new(source)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or(source.as_str());
            if is_secret_bearing_name(basename) {
                return Some(SECRET_FILE_COPY_REASON);
            }
        }
    }
    None
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

    fn eval(command: &str) -> Option<&'static str> {
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
}
