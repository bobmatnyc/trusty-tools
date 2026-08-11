//! Tests for [`super`] — the `transcript_saving` doctor probe (issue #4467).
//!
//! Why: this check exists because the defect was silent, so its own tests have
//! to prove it can FAIL — in both directions, and for every launch line it
//! claims to cover. `verdict` is exercised directly for the failure branches (the
//! production builders are correct, so the wrapper can only ever show the happy
//! path), and the wrapper is exercised against every real builder so the two can
//! never drift apart.

use super::*;

/// A representative path label for direct `verdict` calls.
const PATH: &str = "probe::builder";

/// The gate that matters: EVERY real launch line must preserve transcript
/// saving. Fails if anyone drops the scrub from any builder.
#[test]
fn every_production_launch_line_preserves_transcript_saving() {
    let check = check_transcript_saving();
    assert_eq!(
        check.status,
        CheckStatus::Ok,
        "every production launch line must preserve transcript saving; got: {}",
        check.message
    );
    assert_eq!(check.name, CHECK_NAME);
}

/// Per-builder assertion, so a failure names the offender instead of only
/// reporting that "something" leaks. Marker names are hard-coded: deriving them
/// from the constant would let an emptied list pass.
#[test]
fn launch_lines_covers_every_builder() {
    let lines = super::launch_lines();
    let labels: Vec<&str> = lines.iter().map(|(label, _)| *label).collect();

    for expected in [
        "runtime::claude_code::env_bin_prefix",
        "core::model_inject::build_claude_command",
        "core::model_inject::build_inplace_session_command",
        "core::model_inject::build_client_session_command",
        "daemon::spawn_command::relaunch_command",
        "core::standalone::run::build_launch_command",
        "control::backend::stream_json::build_claude_command",
    ] {
        assert!(
            labels.iter().any(|l| l.contains(expected)),
            "the check must cover {expected}; covered: {labels:?}"
        );
    }

    // Every covered builder must actually unset the suppressing marker, and none
    // may unset a deliberate variable.
    for (label, unset) in &lines {
        assert!(
            unset.iter().any(|n| n == "CLAUDE_CODE_CHILD_SESSION"),
            "{label} must unset CLAUDE_CODE_CHILD_SESSION; unsets: {unset:?}"
        );
        assert!(
            !unset.iter().any(|n| n == "CLAUDE_CONFIG_DIR"),
            "{label} must NOT unset CLAUDE_CONFIG_DIR (#4455); unsets: {unset:?}"
        );
        assert!(
            !unset.iter().any(|n| n == "CLAUDE_CODE_OAUTH_TOKEN"),
            "{label} must NOT unset CLAUDE_CODE_OAUTH_TOKEN (#2246); unsets: {unset:?}"
        );
    }
}

/// Every source site that names `claude` as a command word, with the number of
/// non-comment occurrences expected in that file and why they are acceptable.
///
/// Why (review round 2 MEDIUM): `launch_lines()` is hand-maintained and
/// `launch_lines_covers_every_builder` only asserts the listed labels are
/// PRESENT — nothing failed when a launch line existed that was not on the list.
/// That is how `tm session start`'s in-place line stayed uncovered through two
/// review rounds, and it is an anti-drift helper that reintroduces drift one
/// level down. This allowlist inverts the direction: the test enumerates what is
/// actually IN THE SOURCE and fails on anything unaccounted for, so a new spawn
/// site is a build failure rather than a silent gap.
const ALLOWLISTED_CLAUDE_SITES: &[(&str, usize, &str)] = &[
    (
        "core/model_inject.rs",
        3,
        "the three shared launch-line builders (build_claude_command, \
         build_inplace_session_command, build_client_session_command — all three in \
         launch_lines()) plus the `head()` test helper that reconstructs their prefix. \
         #4181 dropped this from 4 to 3 WITHOUT removing a launch line: \
         build_claude_command gained optional CLAUDE_CONFIG_DIR / \
         CLAUDE_CODE_OAUTH_TOKEN assignments between the scrub flags and the \
         program word, so its `format!(\"env{} claude\")` became a push_str \
         sequence and the literal it emits is ` claude`, which this scanner's \
         patterns do not match. The builder is still registered in launch_lines() \
         — see `claude_command_relocates_the_config_dir`, which pins the whole \
         emitted line",
    ),
    (
        "core/standalone/run.rs",
        2,
        "build_launch_command (tm run, in launch_lines()) and build_login_command \
         (`claude auth login` — an auth subcommand, not an interactive session, so it \
         creates no transcript to suppress)",
    ),
    (
        "daemon/spawn_command.rs",
        1,
        "relaunch_command; in launch_lines()",
    ),
    (
        "control/backend/stream_json.rs",
        1,
        "build_claude_command (headless); in launch_lines()",
    ),
    (
        "core/claude_env_scrub_tests.rs",
        2,
        "test fixtures for scrub_command — not launch lines",
    ),
    (
        "core/spawn_disclaim/pane.rs",
        3,
        "test fixtures for the disclaim wrapper — not launch lines",
    ),
    (
        "daemon/discovery.rs",
        1,
        "a test fixture pane-line string parsed by discovery — not a launch line",
    ),
];

/// Fails when a `claude` launch site appears anywhere in the crate that is not
/// accounted for above.
///
/// Honest about its reach, because a coverage test that overstates itself is the
/// defect it is guarding against: it matches only sites that name `claude`
/// LITERALLY. Builders that spawn a RESOLVED binary path held in a variable
/// (`runtime::claude_code`, `bin/tm/commands/guided_inplace.rs`) carry no literal
/// and are invisible here — they are covered by `launch_lines()` and by their own
/// tests respectively. What this catches is the shape that actually went wrong
/// twice: someone hand-writing `format!("claude …")` or `Command::new("claude")`
/// at a new call site.
#[test]
fn every_claude_launch_site_in_the_crate_is_allowlisted() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut stack = vec![src.clone()];

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src must be readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            // This file necessarily contains every pattern it searches for — in
            // the allowlist's own reason strings and in the failure message. It
            // is the one self-exclusion, and it is a test file, so it can hold
            // no production launch line.
            if path.file_name().and_then(|n| n.to_str())
                == Some("doctor_transcript_saving_tests.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("source must be readable");
            let hits = text
                .lines()
                // Prose in a doc comment is not a spawn site. Filtering comments
                // is what keeps this test from failing on its own explanations.
                .filter(|line| {
                    let t = line.trim_start();
                    !(t.starts_with("//") || t.starts_with("*"))
                })
                .filter(|line| {
                    line.contains(r#"Command::new("claude")"#)
                        || line.contains(r#""claude ""#)
                        || line.contains(r#""claude {"#)
                        || line.contains(r#""claude --"#)
                        || line.contains(r#"} claude""#)
                })
                .count();
            if hits > 0 {
                let rel = path
                    .strip_prefix(&src)
                    .expect("under src")
                    .to_string_lossy()
                    .replace('\\', "/");
                found.insert(rel, hits);
            }
        }
    }

    let expected: std::collections::BTreeMap<String, usize> = ALLOWLISTED_CLAUDE_SITES
        .iter()
        .map(|(f, n, _)| ((*f).to_owned(), *n))
        .collect();

    assert_eq!(
        found, expected,
        "\nA `claude` launch site changed.\n\
         If you ADDED a launch line: route it through `core::claude_env_scrub` (never a second \
         mechanism), register it in `daemon::doctor_transcript_saving::launch_lines()` so the \
         `transcript_saving` check covers it, and then update ALLOWLISTED_CLAUDE_SITES.\n\
         If it is not a launch line, add it with a reason saying why.\n\
         found:    {found:#?}\n\
         expected: {expected:#?}"
    );
}

/// The Ok message must enumerate the binary-crate gaps, not describe one of
/// them — the prose that made a sixth uncovered launch line read as covered.
#[test]
fn ok_message_names_every_binary_crate_gap() {
    let check = check_transcript_saving();
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    for gap in BINARY_CRATE_BUILDERS {
        assert!(
            check.message.contains(gap),
            "the Ok message must name the binary-crate gap {gap}: {}",
            check.message
        );
    }
    assert!(
        check.message.contains(&format!(
            "{} launch line(s) in the `tm` BINARY",
            BINARY_CRATE_BUILDERS.len()
        )),
        "the message must COUNT the gaps rather than implying one: {}",
        check.message
    );
}

/// The probe must supply an OAuth token, or an over-scrub of it stays invisible.
#[test]
fn probe_supplies_an_oauth_token_so_over_scrub_is_visible() {
    let config_dir = std::path::PathBuf::from(PROBE_CONFIG_DIR);
    let prefix = crate::runtime::env_bin_prefix("claude", Some(&config_dir), Some(PROBE_TOKEN));
    assert!(
        prefix.contains("CLAUDE_CODE_OAUTH_TOKEN="),
        "the probe prefix must carry the token assignment: {prefix}"
    );
    assert!(
        prefix.contains("CLAUDE_CONFIG_DIR="),
        "the probe prefix must carry the config-dir assignment: {prefix}"
    );
}

#[test]
fn ok_when_the_marker_is_scrubbed() {
    let check = verdict(
        PATH,
        &["ANTHROPIC_API_KEY", TRANSCRIPT_SUPPRESSING_MARKER],
        &[],
    );
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

/// Under-scrub: the whole point of the check.
#[test]
fn fails_when_the_suppressing_marker_is_not_scrubbed() {
    let check = verdict(PATH, &["ANTHROPIC_API_KEY"], &[]);
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "omitting the suppressing marker must FAIL: {}",
        check.message
    );
    assert!(
        check.message.contains(TRANSCRIPT_SUPPRESSING_MARKER),
        "the message must name the marker: {}",
        check.message
    );
    assert!(
        check.message.contains("4467"),
        "the message must cite the issue: {}",
        check.message
    );
}

/// A failure has to say WHICH builder leaks.
#[test]
fn failure_names_the_leaking_path() {
    let check = verdict("core::some::builder", &["ANTHROPIC_API_KEY"], &[]);
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("core::some::builder"),
        "the message must name the offending builder: {}",
        check.message
    );
}

/// Over-scrub: the opposite failure, which would silently restore #4451.
#[test]
fn fails_when_the_scrub_would_take_the_config_dir() {
    let check = verdict(
        PATH,
        &[
            "ANTHROPIC_API_KEY",
            TRANSCRIPT_SUPPRESSING_MARKER,
            "CLAUDE_CONFIG_DIR",
        ],
        &[],
    );
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "unsetting CLAUDE_CONFIG_DIR must FAIL: {}",
        check.message
    );
    assert!(
        check.message.contains("CLAUDE_CONFIG_DIR"),
        "the message must name the variable: {}",
        check.message
    );
    assert!(
        check.message.contains("4451"),
        "the message must cite the roster regression it prevents: {}",
        check.message
    );
}

/// The OTHER deliberate variable: invisible to the earlier literal-only form.
#[test]
fn over_scrub_of_the_oauth_token_is_detected() {
    let check = verdict(
        PATH,
        &[TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CODE_OAUTH_TOKEN"],
        &[],
    );
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "unsetting the OAuth token must FAIL: {}",
        check.message
    );
    assert!(
        check.message.contains("CLAUDE_CODE_OAUTH_TOKEN"),
        "the message must name the token: {}",
        check.message
    );
    assert!(
        check.message.contains("2246"),
        "the message must cite the login-loop regression: {}",
        check.message
    );
}

/// An over-scrub is reported even though the transcript marker IS present, so
/// the two branches cannot mask each other.
#[test]
fn config_dir_over_scrub_takes_precedence_in_the_message() {
    let check = verdict(
        PATH,
        &[TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CONFIG_DIR"],
        &[],
    );
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("4455"),
        "the deliberate-env branch must be the one reported: {}",
        check.message
    );
}

/// Data loss is not a preference — both failures must be hard.
#[test]
fn failure_is_a_hard_fail_not_a_warn() {
    for unset in [
        vec!["ANTHROPIC_API_KEY"],
        vec![TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CONFIG_DIR"],
        vec![TRANSCRIPT_SUPPRESSING_MARKER, "CLAUDE_CODE_OAUTH_TOKEN"],
    ] {
        let check = verdict(PATH, &unset, &[]);
        assert_ne!(
            check.status,
            CheckStatus::Warn,
            "must never be a Warn for {unset:?}: {}",
            check.message
        );
        assert_eq!(check.status, CheckStatus::Fail);
    }
}

/// The live-marker set is context in the message, never the pass/fail
/// condition — a clean environment must still produce a meaningful Ok.
#[test]
fn ok_message_reports_markers_live_in_the_environment() {
    let clean = verdict(PATH, &[TRANSCRIPT_SUPPRESSING_MARKER], &[]);
    assert_eq!(clean.status, CheckStatus::Ok);
    assert!(
        clean.message.contains("no inherited markers"),
        "a clean env must say so: {}",
        clean.message
    );

    let leaking = verdict(
        PATH,
        &[TRANSCRIPT_SUPPRESSING_MARKER],
        &[TRANSCRIPT_SUPPRESSING_MARKER],
    );
    assert_eq!(
        leaking.status,
        CheckStatus::Ok,
        "a live marker is context, not a failure: {}",
        leaking.message
    );
    assert!(
        leaking.message.contains("running with the leak present"),
        "a leaking env must be reported as context: {}",
        leaking.message
    );
}
