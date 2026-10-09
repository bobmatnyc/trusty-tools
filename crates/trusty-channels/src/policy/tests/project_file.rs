//! Project route files (#8454 S2a): v1 and v2 parse strictly; a refused
//! file loses only its own project's routes.

use std::path::Path;

use super::{host, input, names, routes_file, state, A_V2, B_V1, HOME, HOST_ALL, PROJ_A, PROJ_B};
use crate::policy::{merge, parse_project_file, FileState, Finding, ProjectFileError};

const SLACK_ROUTE: &str =
    "[[slack.routes]]\nname = \"bob-dm\"\nrecipient = \"U0ABCDEF1\"\nkinds = [\"question\"]\n";

fn parse(text: &str) -> Result<crate::policy::ProjectFile, ProjectFileError> {
    parse_project_file(text, Some(Path::new(HOME)))
}

/// Merge project A's `text` beside the good project B and assert A alone is
/// refused with a `FileRefused` finding.
fn assert_only_a_refused(what: &str, text: &str) {
    let report = merge(
        host(HOST_ALL),
        vec![input(PROJ_A, text), input(PROJ_B, B_V1)],
    );
    assert!(!report.denied, "{what}: a file fault denied the whole load");
    assert_eq!(state(&report, PROJ_A), FileState::Refused, "{what}");
    assert_eq!(names(&report), ["janet"], "{what}");
    assert!(
        report.findings.iter().any(|f| matches!(
            f,
            Finding::FileRefused { file, .. } if *file == routes_file(PROJ_A)
        )),
        "{what}: {:?}",
        report.findings
    );
}

/// Project A's v2 file with a `[gchat.connection]` table.
fn v2_with_connection(subscription: &str, key_file: &str) -> String {
    A_V2.replace(
        "version = 2\n",
        &format!(
            "version = 2\n\n[gchat.connection]\nproject_id = \"p\"\n\
             subscription = \"{subscription}\"\nkey_file = \"{key_file}\"\n"
        ),
    )
}

#[test]
fn project_file_faults_are_refused() {
    type Is = fn(&ProjectFileError) -> bool;
    let parse_err: Is = |e| matches!(e, ProjectFileError::Parse { .. });
    let invalid: Is = |e| matches!(e, ProjectFileError::Invalid { .. });
    let v1_gchat = B_V1.to_string();
    let cases: Vec<(&str, String, Is)> = vec![
        ("not toml", "version = \n".into(), parse_err),
        ("no version", SLACK_ROUTE.into(), parse_err),
        ("version as a string", format!("version = \"2\"\n{SLACK_ROUTE}"), parse_err),
        ("version 3", format!("version = 3\n{SLACK_ROUTE}"), |e| {
            matches!(e, ProjectFileError::Version { found: 3 })
        }),
        ("unknown top-level key", format!("{A_V2}extra = 1\n"), parse_err),
        (
            "v2 top-level rate_limit is ceiling-only",
            A_V2.replace("version = 2\n", "version = 2\nrate_limit = { limit = 1, window_secs = 1 }\n"),
            parse_err,
        ),
        (
            "v2 enabled key is ceiling-only",
            A_V2.replace("[[slack.routes]]", "[slack]\nenabled = true\n\n[[slack.routes]]"),
            parse_err,
        ),
        (
            "v2 slack connection: no credentials in project files",
            A_V2.replace(
                "[[slack.routes]]",
                "[slack.connection]\ncredential_ref = \"slack\"\n\n[[slack.routes]]",
            ),
            parse_err,
        ),
        (
            "unknown route key",
            A_V2.replace("kinds = [\"question\"]\n\n[[telegram", "kinds = [\"question\"]\nchannel = \"C1\"\n\n[[telegram"),
            parse_err,
        ),
        ("unknown rate_limit key", A_V2.replace(
            "recipient = \"U0ABCDEF1\"",
            "recipient = \"U0ABCDEF1\"\nrate_limit = { limit = 1, window_secs = 60, burst = 2 }",
        ), parse_err),
        ("unknown kind", A_V2.replacen("[\"question\"]", "[\"chat\"]", 1), parse_err),
        (
            "telegram recipient as an integer",
            A_V2.replace("\"123456789\"", "123456789"),
            parse_err,
        ),
        (
            "v1 gchat without connection",
            v1_gchat.replace(
                "[gchat.connection]\nproject_id = \"p\"\nsubscription = \"s\"\nkey_file = \"/k.json\"\n",
                "",
            ),
            parse_err,
        ),
        (
            "v1 per-route rate_limit is v2-only",
            v1_gchat.replace(
                "kinds = [\"question\", \"review_notice\"]",
                "kinds = [\"question\"]\nrate_limit = { limit = 1, window_secs = 60 }",
            ),
            parse_err,
        ),
        (
            "connection subscription as a path",
            v1_gchat.replace("\"s\"", "\"projects/p/subscriptions/s\""),
            invalid,
        ),
        (
            "connection key_file ~/ without a known home",
            v1_gchat.replace("/k.json", "~/k.json"),
            invalid,
        ),
        // #8454: the v2 arm validates its optional gchat connection too.
        (
            "v2 connection key_file holds a PEM line",
            v2_with_connection("s", "-----BEGIN PRIVATE KEY-----"),
            invalid,
        ),
        (
            "v2 connection subscription as a path",
            v2_with_connection("projects/p/subscriptions/s", "/k.json"),
            invalid,
        ),
        (
            "malformed space",
            v1_gchat.replace(
                "kinds = [\"question\", \"review_notice\"]",
                "kinds = [\"question\"]\nspace = \"rooms/X\"",
            ),
            invalid,
        ),
    ];
    for (what, text, is) in cases {
        // The home-less case is the only one parsed without a home.
        let home = (!what.contains("without a known home")).then(|| Path::new(HOME));
        match parse_project_file(&text, home) {
            Err(e) => assert!(is(&e), "{what}: wrong error {e:?}"),
            Ok(f) => panic!("{what}: parsed {f:?}\n{text}"),
        }
        if home.is_some() {
            assert_only_a_refused(what, &text);
        }
    }
}

#[test]
fn v2_slack_route_requires_v2() {
    let v1 = format!("version = 1\n\n{SLACK_ROUTE}");
    assert!(
        matches!(parse(&v1), Err(ProjectFileError::Parse { .. })),
        "a v1 file carried a Slack route"
    );
    assert_only_a_refused("slack route in v1", &v1);

    let v2 = format!("version = 2\n\n{SLACK_ROUTE}");
    let file = parse(&v2).expect("v2 carries Slack routes");
    assert_eq!(file.version, 2);
    assert_eq!(file.routes[0].entry, r#"slack.routes[0] "bob-dm""#);
    let report = merge(host(HOST_ALL), vec![input(PROJ_A, &v2)]);
    assert_eq!(names(&report), ["bob-dm"]);
}

#[test]
fn project_parse_errors_withhold_input_values() {
    // #8454 S2b: a token typed into a project file must not reach a finding.
    const TOKEN: &str = "xoxb-123-secret";
    let route = |kinds: &str| {
        format!("version = 2\n[[slack.routes]]\nname = \"b\"\nrecipient = \"U0ABCDEF1\"\nkinds = {kinds}\n")
    };
    let cases = [
        ("version is a string", format!("version = \"{TOKEN}\"\n")),
        ("unknown key", format!("version = 2\n{TOKEN} = 1\n")),
        ("unknown kind", route(&format!("[\"{TOKEN}\"]"))),
        ("wrong type", route(&format!("\"{TOKEN}\""))),
        (
            "duplicate key",
            format!("version = 2\n{TOKEN} = 1\n{TOKEN} = 2\n"),
        ),
        ("bare value", format!("version = 2\nx = {TOKEN}\n")),
        (
            "quoted key path",
            format!("version = 2\n[\"{TOKEN}\"]\nx = 1\n"),
        ),
    ];
    for (what, text) in cases {
        let err = parse(&text).expect_err(what);
        assert!(
            matches!(err, ProjectFileError::Parse { .. }),
            "{what}: {err:?}"
        );
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("xoxb"), "{what}: {shown}");
    }
}
