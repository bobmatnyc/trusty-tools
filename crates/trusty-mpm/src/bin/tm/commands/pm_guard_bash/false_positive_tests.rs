//! Regression tests for reported pm_guard FALSE POSITIVES (#7498, #7479,
//! #7499, #7477, #7436).
//!
//! Why: the guard is quote-unaware and conservatively over-blocking by design
//! (see `super`'s module doc), so every false positive it produces is a real
//! cost paid for a real guarantee, and each one is fixed by narrowing exactly
//! one reading — never by weakening a deny. These rows are kept apart from
//! `tests.rs` so the allow case and the deny case that bounds it sit side by
//! side for each issue.
//! What: one module per issue, each pairing the issue's verbatim repro command
//! with the deny case that must survive the fix.
//! Test: itself.
//!
//! This file is classified as a test file (3000-SLOC cap) by its
//! `_tests.rs` basename.

use super::{evaluate_bash_command, unclassifiable_command};
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_command;

/// #7498: a bare `s*` token is a regex fragment, not a glob naming `secrets`.
mod secret_file_glob_matches_an_unrelated_token {
    use super::*;

    /// The issue's verbatim repro command must allow.
    ///
    /// Why: `s*` carries a `*`, so the candidate took
    /// `secret_pattern_overlaps`'s GLOB branch and was overlap-tested against
    /// the literal core `secrets`. `s` + `*` does reach `secrets`, so a
    /// one-character regex fragment denied a `gh` search and the
    /// BASE-ENGINEER file-size precheck alike.
    /// What: asserts the two reported spellings allow.
    /// Test: itself.
    #[test]
    fn a_one_character_glob_fragment_does_not_name_a_secret_family() {
        for command in [
            r#"gh issue list --search "\s* guard" --state all"#,
            r"grep -cvE '^\s*(//|\*|/\*|\*/|$)' crates/trusty-mpm/src/lib.rs",
            "gh issue list --search 's* guard' --state all",
        ] {
            assert_eq!(
                evaluate_secret_file_read_command(command),
                None,
                "a regex fragment must not read as a secret glob: {command}"
            );
        }
    }

    /// The deny this fix must not weaken.
    ///
    /// Why: narrowing the glob branch is only correct while a glob that really
    /// does target a credential family by name still denies.
    /// What: asserts the natural globs for the file families, a word-family
    /// glob written as a path, and three literal secret paths, all still deny.
    ///
    /// `cat *secrets*` is absent deliberately: a word family written with no
    /// directory in front of it has been allowed since #7266 round 6, which is
    /// why `./secrets*` is the row here.
    /// Test: itself.
    #[test]
    fn a_real_secret_glob_and_a_real_secret_path_still_deny() {
        for command in [
            "cat *.env",
            "cat .e*",
            "cat *.pem",
            "cat ./secrets*",
            "cat .env",
            "cat secrets.pem",
            "cat id_rsa",
        ] {
            assert!(
                evaluate_secret_file_read_command(command).is_some(),
                "a real secret target must still deny: {command}"
            );
        }
    }
}

/// #7479: `.env.example` is a placeholder file, not a secret.
mod dotenv_placeholder_files {
    use super::*;

    /// The issue's verbatim repro shapes must allow.
    ///
    /// Why: `.env.*` classed every suffix as secret-bearing, so a tracked
    /// placeholder could not be read by any verb and an operator had to make
    /// the change by hand.
    /// What: asserts the three placeholder suffixes allow under a reading verb,
    /// bare and with a directory in front.
    /// Test: itself.
    #[test]
    fn placeholder_suffixes_are_readable() {
        for command in [
            "cat apps/advisor/.env.example",
            "printf 'KEY=\\n' >> apps/advisor/.env.example",
            "cat .env.example",
            "cat .env.sample",
            "cat .env.template",
            "cat config/.env.example",
        ] {
            assert_eq!(
                evaluate_secret_file_read_command(command),
                None,
                "a placeholder env file must be readable: {command}"
            );
        }
    }

    /// The deny this fix must not weaken.
    ///
    /// Why: the exemption is keyed on the SUFFIX, so every other `.env.*`
    /// spelling — and a real secret file of another family — must still deny.
    /// What: asserts the secret-bearing `.env` spellings and two other families
    /// still deny.
    ///
    /// `secrets.yaml` is absent deliberately: `yaml` has been a transparent
    /// source extension since #7266 round 6, so that name allowed before this
    /// change and still does.
    /// Test: itself.
    #[test]
    fn real_dotenv_files_still_deny() {
        for command in [
            "cat .env",
            "cat .env.local",
            "cat .env.production",
            "cat apps/advisor/.env.local",
            "cat terraform.tfvars",
            "cat id_rsa",
        ] {
            assert!(
                evaluate_secret_file_read_command(command).is_some(),
                "a real secret env file must still deny: {command}"
            );
        }
    }

    /// The exemption covers the dotenv family and no other.
    ///
    /// Why: review round on #7479. The first round keyed only on the final
    /// extension, so it handed the SSH-key and word families a placeholder
    /// convention neither the issue nor its reporter asked for — exactly the
    /// two families where a misnamed file leaks a key rather than a variable
    /// name.
    /// What: asserts a `.env` placeholder allows while the same suffix on the
    /// two PREFIX-typed families — the ones the exemption really did reach —
    /// still denies.
    ///
    /// An EXTENSION-typed family is not testable here and is not a regression:
    /// `server.pem.example` allowed before this change too, because `*.pem`
    /// matches only a name ENDING `.pem` and the placeholder suffix displaces
    /// it. That is the pre-existing `*.<ext>` rule, not the exemption.
    /// Test: itself.
    #[test]
    fn the_placeholder_exemption_covers_only_the_dotenv_family() {
        assert_eq!(evaluate_secret_file_read_command("cat .env.example"), None);
        for command in ["cat id_rsa.sample", "cat secrets.example"] {
            assert!(
                evaluate_secret_file_read_command(command).is_some(),
                "only the dotenv family is exempt: {command}"
            );
        }
    }

    /// The placeholder suffix counts only as the FINAL extension.
    ///
    /// Why: the exemption is a naming convention, and a convention that
    /// matched anywhere in the name would let `.env.example.bak` — a real
    /// dump of a real environment — read freely.
    /// What: asserts a placeholder suffix followed by another extension still
    /// denies.
    /// Test: itself.
    #[test]
    fn a_placeholder_suffix_is_honoured_only_as_the_final_extension() {
        for command in ["cat .env.example.bak", "cat .env.sample.local"] {
            assert!(
                evaluate_secret_file_read_command(command).is_some(),
                "a placeholder suffix behind another extension must deny: {command}"
            );
        }
    }
}

/// #7499: a Go-template `--format` argument is a brace literal, not an
/// alternation.
mod go_template_braces {
    use super::*;

    /// The issue's verbatim repro command must allow.
    ///
    /// Why: #7414 repaired the ORPHANED brace a word cut creates, but a
    /// Go-template argument survives the cut with BOTH braces intact — `{{`
    /// and `}}` — so it reached the shared expander as a real alternation and
    /// failed closed on a shape it cannot resolve.
    /// What: asserts the reported `docker` spellings allow.
    /// Test: itself.
    #[test]
    fn a_go_template_format_argument_is_allowed() {
        for command in [
            "docker images --format '{{.Repository}}:{{.Tag}}'",
            "docker ps --format '{{.ID}}'",
            "docker inspect --format '{{.Created}}' abc123",
            "docker ps --format \"{{.Names}}\\t{{.Status}}\"",
        ] {
            assert_eq!(
                evaluate_secret_file_read_command(command),
                None,
                "a Go-template format argument must allow: {command}"
            );
        }
    }

    /// A Bash SEQUENCE group is expanded, not read as literal text.
    ///
    /// Why: review round on #7499. A comma-free body is not always literal —
    /// `{v..v}` is a degenerate sequence a shell expands to `v`, so
    /// `cat .en{v..v}` opens `.env`. `origin/main` denied that by ACCIDENT:
    /// stripping the group produced `.env..v`, which `.env.*` matches. Reading
    /// the group as literal removed the accident and the coverage with it.
    /// Rows 2 and 4 are the wider hole the accident never covered — open on
    /// `origin/main` too — closed here because the fix is in this function.
    /// What: asserts every sequence spelling that expands onto a secret name
    /// denies.
    /// Test: itself.
    #[test]
    fn a_sequence_group_that_expands_onto_a_secret_name_denies() {
        for command in [
            "cat .en{v..v}",
            "cat .en{u..v}",
            "cat id_rs{a..a}",
            "cat id_rs{a..c}",
            "cat secret{s..s}.txt",
            "cp .en{v..v} /tmp/x/",
        ] {
            assert!(
                evaluate_secret_file_read_command(command).is_some(),
                "a sequence group reaching a secret name must deny: {command}"
            );
        }
    }

    /// An ordinary sequence group naming no secret still allows.
    ///
    /// Why: expanding sequences is only affordable while the shapes an agent
    /// writes all day stay allowed. These are the two common ones.
    /// What: asserts a numeric and an alphabetic range allow, alongside the
    /// Go-template rows above.
    /// Test: itself.
    #[test]
    fn an_ordinary_sequence_group_is_allowed() {
        for command in [
            "for i in {1..5}; do echo $i; done",
            "echo {a..e}",
            "mkdir -p build/{1..3}",
            "echo {1..5000}",
        ] {
            assert_eq!(
                evaluate_secret_file_read_command(command),
                None,
                "an ordinary sequence group must allow: {command}"
            );
        }
    }

    /// The #7414 deny cases must still deny.
    ///
    /// Why: a brace pair that survives the cut whole is a real alternation, and
    /// the whole point of failing closed on an unresolvable one is that a
    /// secret cannot be hidden inside it.
    /// What: asserts a real alternation naming a secret still denies, and that
    /// an unresolvable single-brace alternation still fails closed.
    /// Test: itself.
    #[test]
    fn a_real_brace_alternation_still_denies() {
        for command in [
            "cat {.env,.env.prod}",
            "cp secret.{tfvars,bak} /tmp/",
            "cat .{e,env}",
        ] {
            assert!(
                evaluate_secret_file_read_command(command).is_some(),
                "a real brace alternation naming a secret must still deny: {command}"
            );
        }
    }
}

/// #7477 and #7436: the reported refusals do not come from this guard, and
/// this guard's own answer for them never varies.
///
/// The refusal text both issues quote — "too complex to verify that it stays
/// inside the worktree", inside "This agent is isolated in the worktree …" —
/// is emitted by the Claude Code binary, not by any `trusty-*` binary. That was
/// already established for a different shape family by #6982 (see
/// `HARNESS_REFUSED_SHAPES` and `harness_refused_shapes_stay_classifiable_here`
/// in this module's `tests` sibling); these rows extend the same catalogue with
/// the shapes these two issues report, and add the DETERMINISM claim neither
/// issue could make about the harness.
mod harness_refusals_are_not_this_guard {
    use super::*;

    /// The command shapes #7477 and #7436 report as refused.
    ///
    /// Rows 1-4 are #7477's verbatim list. Rows 5-7 are #7436's scratchpad
    /// read-back, plus two shapes refused live in the session that produced
    /// this fix — a multi-file `grep` and a `grep` carrying context flags —
    /// each of which has an allowed twin differing only in a flag.
    const REPORTED_REFUSALS: &[&str] = &[
        "ls apps",
        "ls -la",
        "grep -rl healthz services",
        "grep -n healthz docs/specs/domain-service-shell.md",
        r#"grep -E "^test result" /private/tmp/claude-502/proj/sess/scratchpad/gate-7359-test.txt"#,
        "grep -n scratchpad crates/trusty-mpm/src/bin/tm/commands/pm_guard.rs \
         crates/trusty-mpm/src/bin/tm/commands/pm_guard_bash/mod.rs",
        "grep -n fn -B 4 -A 12 crates/trusty-mpm/src/bin/tm/commands/pm_guard_bash/mod.rs",
    ];

    /// Every reported shape is classifiable and allowed here.
    ///
    /// Why: a `Some` from either band would mean the refusal really did
    /// originate in this guard and a fix belonged in this repository. `None`
    /// from both is the finding: the shapes are answerable, so the refusal came
    /// from the harness and a change here would fix nothing.
    /// What: asserts both bands answer `None` — [`unclassifiable_command`], the
    /// ABSOLUTE band a dispatched subagent reaches, and
    /// [`evaluate_bash_command`], the PM classifier.
    /// Test: itself.
    #[test]
    fn every_reported_shape_is_classifiable_and_allowed_here() {
        for command in REPORTED_REFUSALS {
            assert_eq!(
                unclassifiable_command(command),
                None,
                "the harness refused this as unverifiable; this guard must still \
                 establish what it runs: {command}"
            );
            assert_eq!(
                evaluate_bash_command(command),
                None,
                "a read-only command must allow: {command}"
            );
        }
    }

    /// The same command evaluated 50 times IN ONE PROCESS yields the same
    /// verdict every time.
    ///
    /// Why: #7477 reports "no complexity pattern predicts the refusal" and
    /// #7436's comment reports an identical command flipping PASS to REFUSED
    /// four calls later. Both readings blame a nondeterministic decision. This
    /// guard's decision is a pure function of the command string — no clock, no
    /// filesystem read, no environment read, no map iteration — so it cannot be
    /// the varying party.
    ///
    /// Scope, narrowed in review: one process is all this row proves, and the
    /// reported flips happened BETWEEN `tm hook` invocations — separate
    /// processes with separate `RandomState` seeds and separate environments.
    /// `pm_guard_gives_one_command_the_same_verdict_across_separate_processes`
    /// in `tests/tm_hook_pm_guard_false_positives.rs` is the row that covers
    /// that; this one covers the pure policy underneath it.
    /// What: evaluates each row 50 times and asserts every verdict equals the
    /// first.
    /// Test: itself.
    #[test]
    fn the_verdict_for_one_command_never_varies_within_one_process() {
        for command in REPORTED_REFUSALS {
            let first = (
                unclassifiable_command(command),
                evaluate_bash_command(command),
            );
            for round in 1..50 {
                assert_eq!(
                    (
                        unclassifiable_command(command),
                        evaluate_bash_command(command)
                    ),
                    first,
                    "verdict changed on round {round} for: {command}"
                );
            }
        }
    }
}
