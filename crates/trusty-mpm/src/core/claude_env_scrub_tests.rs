//! Tests for [`super`] — the managed-spawn env scrub (issue #4467).
//!
//! Why: the scrub is a silent-failure guard, so its own tests have to be able to
//! fail. The two directions that matter are asserted independently: the
//! transcript-suppressing marker must be scrubbed (under-scrub → managed
//! sessions lose transcripts), and `CLAUDE_CONFIG_DIR` must NOT be scrubbed
//! (over-scrub → issue #4451 returns and every delegation degrades to
//! `general-purpose`).

use super::*;

/// Every marker, by LITERAL name.
///
/// Why (review follow-up on #4467): every other test in this file iterates
/// `INHERITED_SESSION_MARKERS`, so deleting an entry made them all pass
/// vacuously — `CLAUDE_PID` and `CLAUDE_EFFORT` in particular were pinned by
/// nothing, and removing either kept the whole suite green. Restating the names
/// here is the point: this list and the production constant must be edited
/// together, deliberately.
const EXPECTED_MARKERS: &[&str] = &[
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDE_CODE_EXECPATH",
];

/// The single most important assertion in this file: the variable that
/// disables transcript saving is actually on the scrub list (issue #4467).
#[test]
fn suppressing_marker_is_scrubbed() {
    assert_eq!(
        TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CODE_CHILD_SESSION",
        "the suppressing marker's identity is load-bearing; it is the sole trigger \
         in Claude Code's persistence gate"
    );
    assert!(
        INHERITED_SESSION_MARKERS.contains(&TRANSCRIPT_SUPPRESSING_MARKER),
        "the marker that disables transcript saving must be scrubbed; \
         without it managed sessions get no --resume/--continue/rewind recovery"
    );
}

/// Pins the full marker set by literal name, in order, so deleting ANY entry
/// fails — including `CLAUDE_PID` and `CLAUDE_EFFORT`, which previously had no
/// literal assertion anywhere in the suite.
#[test]
fn every_marker_is_pinned_by_literal_name() {
    assert_eq!(
        INHERITED_SESSION_MARKERS, EXPECTED_MARKERS,
        "the scrub list changed. If that is intentional, update EXPECTED_MARKERS in \
         this file too — and re-check the kept/scrubbed rationale in the module doc, \
         since every entry there has a stated reason"
    );
}

/// Over-scrub guard: the deliberate spawn env and the scrub list must be
/// disjoint. Scrubbing `CLAUDE_CONFIG_DIR` re-breaks #4451 (#4455 relies on it).
#[test]
fn marker_list_is_free_of_deliberate_spawn_env() {
    for deliberate in DELIBERATE_SPAWN_ENV {
        assert!(
            !INHERITED_SESSION_MARKERS.contains(deliberate),
            "{deliberate} is set deliberately by a managed spawn and must never \
             be scrubbed — scrubbing CLAUDE_CONFIG_DIR re-breaks #4451"
        );
    }
}

/// `CLAUDE_CONFIG_DIR` named explicitly, not just via the loop above, so the
/// guard survives someone emptying `DELIBERATE_SPAWN_ENV` (which would make
/// `marker_list_is_free_of_deliberate_spawn_env` pass vacuously).
#[test]
fn config_dir_is_never_scrubbed() {
    assert!(
        !INHERITED_SESSION_MARKERS.contains(&"CLAUDE_CONFIG_DIR"),
        "CLAUDE_CONFIG_DIR relocation is what puts the bundled roster in the \
         `user` settings tier a managed session loads (#4455); scrubbing it \
         silently restores the #4451 zero-specialists failure"
    );
    assert!(
        DELIBERATE_SPAWN_ENV.contains(&"CLAUDE_CONFIG_DIR"),
        "CLAUDE_CONFIG_DIR must stay listed as deliberate spawn env"
    );
}

/// A duplicate would emit a redundant `-u` flag on every managed pane command.
#[test]
fn marker_list_has_no_duplicates() {
    let mut seen: Vec<&str> = Vec::new();
    for name in INHERITED_SESSION_MARKERS {
        assert!(
            !seen.contains(name),
            "{name} appears twice in INHERITED_SESSION_MARKERS"
        );
        seen.push(name);
    }
}

/// Every marker must reach the `env` prefix — a marker on the list but missing
/// from the flags would be scrubbed on the `Command` paths only.
#[test]
fn env_unset_flags_covers_every_marker() {
    let flags = env_unset_flags();
    for name in INHERITED_SESSION_MARKERS {
        assert!(
            flags.contains(&format!(" -u {name}")),
            "env_unset_flags() must unset {name}; got {flags:?}"
        );
    }
    assert_eq!(
        parse_env_unset_vars(&flags),
        INHERITED_SESSION_MARKERS.to_vec(),
        "the flags must round-trip back to exactly the marker list"
    );
}

/// The flags concatenate onto an existing `env -u ANTHROPIC_API_KEY` prefix, so
/// each needs a LEADING space; a missing one would fuse two tokens together and
/// break every managed spawn.
#[test]
fn env_unset_flags_are_space_prefixed() {
    let flags = env_unset_flags();
    assert!(
        flags.starts_with(" -u "),
        "flags must start with a leading space: {flags:?}"
    );
    assert!(
        !flags.contains("  "),
        "no double spaces (would survive but signals a formatting bug): {flags:?}"
    );
}

/// `scrub_command` must surface every marker as an explicit removal, which
/// `get_envs()` reports as `(key, None)`.
#[test]
fn scrub_command_removes_every_marker() {
    let mut cmd = std::process::Command::new("claude");
    scrub_command(&mut cmd);
    let removed: Vec<String> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    for name in INHERITED_SESSION_MARKERS {
        assert!(
            removed.contains(&(*name).to_owned()),
            "scrub_command must env_remove({name}); removed={removed:?}"
        );
    }
}

/// Over-scrub guard on the `Command` path: an explicit assignment made before
/// the scrub must survive it, because `build_inplace_exec_command` sets
/// `CLAUDE_CONFIG_DIR` and the OAuth token on the same command.
#[test]
fn scrub_command_leaves_deliberate_env_intact() {
    let mut cmd = std::process::Command::new("claude");
    cmd.env("CLAUDE_CONFIG_DIR", "/tm/config");
    cmd.env(crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR, "tok");
    scrub_command(&mut cmd);
    let kept: Vec<(String, Option<String>)> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert!(
        kept.contains(&(
            "CLAUDE_CONFIG_DIR".to_owned(),
            Some("/tm/config".to_owned())
        )),
        "the deliberate CLAUDE_CONFIG_DIR assignment must survive the scrub: {kept:?}"
    );
    assert!(
        kept.contains(&(
            crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR.to_owned(),
            Some("tok".to_owned())
        )),
        "the deliberate OAuth token assignment must survive the scrub: {kept:?}"
    );
}

/// The parser is what makes the doctor probe non-tautological, so it is tested
/// against a realistic prefix shape rather than only its own output.
#[test]
fn parse_env_unset_vars_reads_the_real_spawn_prefix() {
    let prefix = format!(
        "env -u ANTHROPIC_API_KEY{} CLAUDE_CONFIG_DIR='/tm/config' /abs/claude",
        env_unset_flags()
    );
    let unset = parse_env_unset_vars(&prefix);
    assert_eq!(
        unset.first(),
        Some(&"ANTHROPIC_API_KEY"),
        "the pre-existing API-key scrub must still be detected: {unset:?}"
    );
    assert!(
        unset.contains(&TRANSCRIPT_SUPPRESSING_MARKER),
        "the suppressing marker must be visible in the parsed prefix: {unset:?}"
    );
}

/// POSIX option termination: the first `NAME=VALUE` ends option parsing, so a
/// later `-u` is NOT an unset — `env` would try to exec `-u` as a command.
///
/// Why this matters (review follow-up on #4467): the earlier parser reported
/// `FOO` unset here, which meant the doctor probe would have passed on an
/// ordering that kills every spawn with `env: -u: No such file or directory`
/// (exit 127). Reporting nothing is the correct answer — the prefix is broken.
#[test]
fn parse_env_unset_vars_stops_at_the_first_assignment() {
    let unset = parse_env_unset_vars("env CLAUDE_CODE_CHILD_SESSION=1 -u FOO /abs/claude");
    assert_eq!(
        unset,
        Vec::<&str>::new(),
        "an assignment ends option parsing, so no later -u counts as an unset: {unset:?}"
    );
    // A well-formed prefix — flags BEFORE the assignment — still parses fully.
    let good = parse_env_unset_vars("env -u FOO CLAUDE_CONFIG_DIR=/tmp/x /abs/claude");
    assert_eq!(
        good,
        vec!["FOO"],
        "flags preceding the assignment must still be collected: {good:?}"
    );
}

/// The COMMAND word ends `env`'s argument list, so flags belonging to the
/// command are never mistaken for `env -u` operands.
///
/// Why this matters (review round 2 LOW): `model_inject::build_claude_command`
/// emits NO `NAME=VALUE` assignments, so the assignment stop never fires on its
/// output. Without a COMMAND stop the scan runs to the end of the line, and a
/// future `claude` flag pair spelled `-u <value>` would be counted as an unset
/// variable — inflating what the `tm doctor` probe believes is scrubbed.
#[test]
fn parse_env_unset_vars_stops_at_the_command_word() {
    let unset = parse_env_unset_vars("env -u REAL claude --some-flag -u NOT_AN_UNSET");
    assert_eq!(
        unset,
        vec!["REAL"],
        "only operands before the command word are unsets: {unset:?}"
    );

    // The daemon shape: the command word can be an absolute path, and the
    // disclaim wrapper puts a quoted tm path there. Neither may be scanned past.
    let wrapped =
        parse_env_unset_vars("env -u REAL '/w/tm' internal-spawn-disclaimed /abs/claude -u NOPE");
    assert_eq!(
        wrapped,
        vec!["REAL"],
        "the wrapper path is a command word, not an operand: {wrapped:?}"
    );
}

/// A malformed trailing `-u` must not panic.
#[test]
fn parse_env_unset_vars_tolerates_trailing_flag() {
    assert_eq!(parse_env_unset_vars("env -u"), Vec::<&str>::new());
}

#[test]
fn markers_present_in_detects_a_set_marker() {
    let present = markers_present_in(|name| name == TRANSCRIPT_SUPPRESSING_MARKER);
    assert_eq!(present, vec![TRANSCRIPT_SUPPRESSING_MARKER]);
}

#[test]
fn markers_present_in_is_empty_for_a_clean_env() {
    assert!(markers_present_in(|_| false).is_empty());
}

#[test]
fn markers_present_in_reports_every_marker_when_all_are_set() {
    assert_eq!(
        markers_present_in(|_| true),
        INHERITED_SESSION_MARKERS.to_vec()
    );
}
