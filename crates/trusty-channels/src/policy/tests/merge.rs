//! The merge rules (#8454 plan §3.4, Architect Q1, Q2, Q7): every dropped
//! route is named, a file fault isolates to its file, a cross-file overlap
//! denies all.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{
    assert_denied, host, input, names, routes_file, state, A_V2, B_V1, HOST_ALL, JANET, PROJ_A,
    PROJ_B,
};
use crate::policy::merge::attribute;
use crate::policy::{
    merge, merge_for, Channel, FileState, Finding, FindingScope, MessageKind, Origin, PolicyError,
};

fn origin(dir: &str, entry: &str) -> Origin {
    Origin {
        file: routes_file(dir),
        entry: entry.into(),
    }
}

/// A v2 file with one Slack route, its recipient and extra keys given.
fn slack(name: &str, recipient: &str, extra: &str) -> String {
    format!(
        "version = 2\n\n[[slack.routes]]\nname = \"{name}\"\nrecipient = \"{recipient}\"\nkinds = [\"question\"]\n{extra}"
    )
}

#[test]
fn disabled_channel_drops_routes_with_finding() {
    // Slack disabled, Telegram absent; gchat stays on.
    let yaml = HOST_ALL
        .replace(
            "  slack:\n    enabled: true",
            "  slack:\n    enabled: false",
        )
        .replace(
            "  telegram:\n    enabled: true\n    projects: [/work/a, /work/b]\n",
            "",
        );
    let report = merge(host(&yaml), vec![input(PROJ_A, A_V2), input(PROJ_B, B_V1)]);
    assert!(!report.denied);
    assert_eq!(names(&report), ["janet"]);
    assert_eq!(
        report.findings,
        [
            Finding::ChannelDisabled {
                origin: origin(PROJ_A, r#"slack.routes[0] "bob-dm""#),
                channel: Channel::Slack,
            },
            Finding::ChannelDisabled {
                origin: origin(PROJ_A, r#"telegram.routes[0] "bob-tg""#),
                channel: Channel::Telegram,
            },
        ]
    );
    assert!(report
        .findings
        .iter()
        .all(|f| f.scope() == FindingScope::Route));
    // The file is not broken: it loaded, with nothing in effect.
    assert_eq!(state(&report, PROJ_A), FileState::Effective { routes: 0 });
}

#[test]
fn unlisted_project_has_no_routes() {
    // Q7: projects is per channel. A is listed for Telegram only.
    let yaml = HOST_ALL.replacen(
        "    connection: { credential_ref: slack }\n    projects: [/work/a, /work/b]",
        "    connection: { credential_ref: slack }\n    projects: [/work/b]",
        1,
    );
    let report = merge(host(&yaml), vec![input(PROJ_A, A_V2)]);
    assert_eq!(names(&report), ["bob-tg"]);
    assert_eq!(
        report.findings,
        [Finding::ProjectNotListed {
            origin: origin(PROJ_A, r#"slack.routes[0] "bob-dm""#),
            channel: Channel::Slack,
            project: PathBuf::from(PROJ_A),
        }]
    );

    // A project absent from every list contributes nothing.
    let report = merge(host(HOST_ALL), vec![input("/work/c", A_V2)]);
    assert!(report.policy.is_empty());
    assert_eq!(report.findings.len(), 2, "{:?}", report.findings);
}

#[test]
fn widening_rate_limit_fails_that_file_only() {
    let yaml = HOST_ALL.replace("limit: 100, window_secs: 60", "limit: 50, window_secs: 60");
    for (what, limit) in [
        ("higher limit", "{ limit = 51, window_secs = 60 }"),
        ("shorter window", "{ limit = 50, window_secs = 59 }"),
    ] {
        let a = slack("bob-dm", "U0ABCDEF1", &format!("rate_limit = {limit}\n"));
        let b = slack(
            "eve-dm",
            "U0EVE",
            "rate_limit = { limit = 10, window_secs = 60 }\n",
        );
        let report = merge(host(&yaml), vec![input(PROJ_A, &a), input(PROJ_B, &b)]);
        assert!(!report.denied, "{what}");
        assert_eq!(state(&report, PROJ_A), FileState::Refused, "{what}");
        assert_eq!(names(&report), ["eve-dm"], "{what}: a lowered limit stands");
        let [Finding::Widening {
            origin: o, field, ..
        }] = report.findings.as_slice()
        else {
            panic!("{what}: {:?}", report.findings);
        };
        assert_eq!(*o, origin(PROJ_A, r#"slack.routes[0] "bob-dm""#));
        assert_eq!(*field, "rate_limit");
    }
}

#[test]
fn kind_outside_ceiling_fails_file() {
    let yaml = HOST_ALL.replace("  gchat:\n", "  gchat:\n    kinds: [question]\n");
    let report = merge(host(&yaml), vec![input(PROJ_A, A_V2), input(PROJ_B, B_V1)]);
    assert!(!report.denied);
    assert_eq!(state(&report, PROJ_B), FileState::Refused);
    assert_eq!(names(&report), ["bob-dm", "bob-tg"]);
    let [Finding::Widening {
        origin: o,
        field,
        detail,
    }] = report.findings.as_slice()
    else {
        panic!("{:?}", report.findings);
    };
    assert_eq!(*o, origin(PROJ_B, r#"gchat.routes[0] "janet""#));
    assert_eq!(*field, "kinds");
    assert!(detail.contains("review_notice"), "{detail}");
}

#[test]
fn review_notice_on_slack_fails_file() {
    let a = A_V2.replacen("[\"question\"]", "[\"question\", \"review_notice\"]", 1);
    let report = merge(host(HOST_ALL), vec![input(PROJ_A, &a), input(PROJ_B, B_V1)]);
    assert_eq!(state(&report, PROJ_A), FileState::Refused);
    assert_eq!(names(&report), ["janet"]);
    let [Finding::RouteRejected { origin: o, error }] = report.findings.as_slice() else {
        panic!("{:?}", report.findings);
    };
    assert_eq!(*o, origin(PROJ_A, r#"slack.routes[0] "bob-dm""#));
    assert!(matches!(
        error,
        PolicyError::KindNotOnChannel {
            kind: MessageKind::ReviewNotice,
            channel: Channel::Slack,
            ..
        }
    ));
}

const HOST_CONN: &str =
    "  gchat:\n    connection: { project_id: p, subscription: s, key_file: ~/k.json }\n";

#[test]
fn connection_mismatch_fails_file() {
    let yaml = HOST_ALL.replace("  gchat:\n", HOST_CONN);
    for field in ["project_id", "subscription", "key_file"] {
        let b = match field {
            "project_id" => B_V1.replace("project_id = \"p\"", "project_id = \"other\""),
            "subscription" => B_V1.replace("subscription = \"s\"", "subscription = \"other\""),
            _ => B_V1.replace("/k.json", "/other.json"),
        };
        let report = merge(host(&yaml), vec![input(PROJ_A, A_V2), input(PROJ_B, &b)]);
        assert!(!report.denied, "{field}");
        assert_eq!(state(&report, PROJ_B), FileState::Refused, "{field}");
        assert_eq!(names(&report), ["bob-dm", "bob-tg"], "{field}");
        let [Finding::Widening {
            origin: o,
            field: f,
            detail,
        }] = report.findings.as_slice()
        else {
            panic!("{field}: {:?}", report.findings);
        };
        assert_eq!(*o, origin(PROJ_B, "gchat.connection"));
        assert_eq!(*f, "gchat.connection");
        assert!(detail.contains(field), "{detail}");
        assert!(
            !detail.contains("other"),
            "a connection value leaked: {detail}"
        );
    }
}

#[test]
fn host_connection_is_authoritative() {
    let yaml = HOST_ALL.replace("  gchat:\n", HOST_CONN);
    let ceiling = host(&yaml).expect("ceiling");
    let host_conn = ceiling
        .channel(Channel::Gchat)
        .and_then(|c| c.gchat_connection())
        .cloned();
    // An equal project connection, and a v2 file with none, both load and
    // run on the host's connection.
    let equal = B_V1.replace("/k.json", "/home/t/k.json");
    let none = "version = 2\n\n[[gchat.routes]]\nname = \"janet\"\nrecipient = \"janet@example.com\"\nkinds = [\"question\"]\n";
    for (what, b) in [("equal", equal.as_str()), ("absent", none)] {
        let report = merge(Ok(ceiling.clone()), vec![input(PROJ_B, b)]);
        assert_eq!(names(&report), ["janet"], "{what}: {:?}", report.findings);
        assert_eq!(report.per_file[0].gchat_connection, host_conn, "{what}");
    }
    // With no host connection, the v1 file's own connection is used.
    let report = merge(host(HOST_ALL), vec![input(PROJ_B, B_V1)]);
    let conn = report.per_file[0]
        .gchat_connection
        .as_ref()
        .expect("v1 connection");
    assert_eq!(conn.key_file, Path::new("/k.json"));
}

#[test]
fn gchat_routes_without_any_connection_fail_file() {
    let b = "version = 2\n\n[[gchat.routes]]\nname = \"janet\"\nrecipient = \"janet@example.com\"\nkinds = [\"question\"]\n";
    let report = merge(host(HOST_ALL), vec![input(PROJ_A, A_V2), input(PROJ_B, b)]);
    assert_eq!(state(&report, PROJ_B), FileState::Refused);
    assert_eq!(names(&report), ["bob-dm", "bob-tg"]);
    assert_eq!(
        report.findings,
        [Finding::NoGchatConnection {
            file: routes_file(PROJ_B)
        }]
    );
}

#[test]
fn overlap_across_files_names_both_files() {
    let cases = [
        ("name", slack("bob-dm", "U0OTHER", ""), "bob-dm"),
        ("recipient", slack("other-dm", "U0ABCDEF1", ""), "U0ABCDEF1"),
    ];
    for (field, b, value) in cases {
        let report = merge(host(HOST_ALL), vec![input(PROJ_A, A_V2), input(PROJ_B, &b)]);
        assert_denied(&report);
        assert_eq!(state(&report, PROJ_A), FileState::Withheld);
        assert_eq!(state(&report, PROJ_B), FileState::Withheld);
        let second = format!(
            "slack.routes[0] {:?}",
            if field == "name" {
                "bob-dm"
            } else {
                "other-dm"
            }
        );
        let expected = Finding::Overlap {
            first: origin(PROJ_A, r#"slack.routes[0] "bob-dm""#),
            second: origin(PROJ_B, &second),
            field,
            value: value.into(),
        };
        assert_eq!(report.findings, std::slice::from_ref(&expected), "{field}");
        assert_eq!(expected.scope(), FindingScope::DenyAll);
        let msg = expected.to_string();
        assert!(
            msg.contains("/work/a/") && msg.contains("/work/b/"),
            "{msg}"
        );
    }
}

#[test]
fn broken_file_in_one_project_leaves_other_project_effective() {
    // Q1: an overlap inside one file refuses that file only.
    let dup = A_V2.replace(
        "[[telegram.routes]]",
        "[[slack.routes]]\nname = \"bob-2\"\nrecipient = \"U0ABCDEF1\"\nkinds = [\"question\"]\n\n[[telegram.routes]]",
    );
    let report = merge(
        host(HOST_ALL),
        vec![input(PROJ_A, &dup), input(PROJ_B, B_V1)],
    );
    assert!(!report.denied);
    assert_eq!(state(&report, PROJ_A), FileState::Refused);
    assert_eq!(state(&report, PROJ_B), FileState::Effective { routes: 1 });
    assert_eq!(names(&report), ["janet"]);
    let [f] = report.findings.as_slice() else {
        panic!("{:?}", report.findings);
    };
    let Finding::Overlap { first, second, .. } = f else {
        panic!("{f:?}");
    };
    assert_eq!(*first, origin(PROJ_A, r#"slack.routes[0] "bob-dm""#));
    assert_eq!(*second, origin(PROJ_A, r#"slack.routes[1] "bob-2""#));
    assert_eq!(f.scope(), FindingScope::File);

    // A parse failure in A, likewise.
    let report = merge(
        host(HOST_ALL),
        vec![input(PROJ_A, "version = 2\nbad = 1\n"), input(PROJ_B, B_V1)],
    );
    assert!(!report.denied);
    assert_eq!(names(&report), ["janet"]);
}

#[test]
fn policy_errors_name_the_file_entry() {
    // The bad route is S1's routes[1]; the finding names the file's own entry.
    let a = A_V2.replace("\"123456789\"", "\"0123\"");
    let report = merge(host(HOST_ALL), vec![input(PROJ_A, &a)]);
    let [Finding::RouteRejected { origin: o, error }] = report.findings.as_slice() else {
        panic!("{:?}", report.findings);
    };
    assert_eq!(*o, origin(PROJ_A, r#"telegram.routes[0] "bob-tg""#));
    assert!(
        matches!(error, PolicyError::InvalidRoute { .. }),
        "{error:?}"
    );
    // Every other S1 rule is attributed the same way.
    for (what, a) in [
        ("empty kinds", A_V2.replacen("[\"question\"]", "[]", 1)),
        ("bad name", A_V2.replace("\"bob-dm\"", "\"bob dm\"")),
        (
            "zero limit",
            A_V2.replacen(
                "kinds = [\"question\"]",
                "kinds = [\"question\"]\nrate_limit = { limit = 0, window_secs = 60 }",
                1,
            ),
        ),
    ] {
        let report = merge(host(HOST_ALL), vec![input(PROJ_A, &a)]);
        let [Finding::RouteRejected { origin: o, .. }] = report.findings.as_slice() else {
            panic!("{what}: {:?}", report.findings);
        };
        assert_eq!(o.file, routes_file(PROJ_A), "{what}");
        assert!(
            o.entry.starts_with("slack.routes[0]"),
            "{what}: {}",
            o.entry
        );
    }
}

#[test]
fn unmapped_policy_error_keeps_raw_entry() {
    let err = PolicyError::EmptyKinds {
        entry: "routes[9] slack \"x\"".into(),
    };
    let finding = attribute(err.clone(), &HashMap::new(), Path::new("/f"));
    assert_eq!(
        finding,
        Finding::RouteRejected {
            origin: Origin {
                file: PathBuf::from("/f"),
                entry: "routes[9] slack \"x\"".into(),
            },
            error: err,
        }
    );
}

// ── S2b (#8454 S2b plan §8): items carried from the S2a critic ──

/// A gchat connection and one route to janet, with no version line.
fn gchat_janet(name: &str, space: Option<&str>) -> String {
    let space = space
        .map(|s| format!("space = \"{s}\"\n"))
        .unwrap_or_default();
    format!(
        "\n[gchat.connection]\nproject_id = \"p\"\nsubscription = \"s\"\nkey_file = \"/k.json\"\n\n\
         [[gchat.routes]]\nname = \"{name}\"\nrecipient = \"{JANET}\"\nkinds = [\"question\"]\n{space}"
    )
}

#[test]
fn combined_overlap_spans_only_the_consumer_channels() {
    // A and B both route gchat to janet; A also has Slack and Telegram.
    let bots = A_V2.strip_prefix("version = 2\n").expect("v2 fixture");
    let a = format!("version = 2\n{}{bots}", gchat_janet("j-a", None));
    let b = format!("version = 1\n{}", gchat_janet("j-b", None));
    let inputs = || vec![input(PROJ_A, &a), input(PROJ_B, &b)];
    let daemon = merge_for(
        host(HOST_ALL),
        inputs(),
        &[Channel::Slack, Channel::Telegram],
    );
    assert!(
        !daemon.denied,
        "a gchat overlap denied the daemon: {:#?}",
        daemon.findings
    );
    assert_eq!(names(&daemon), ["bob-dm", "bob-tg"]);
    // The gchat consumer still sees the overlap and is denied.
    assert_denied(&merge_for(host(HOST_ALL), inputs(), &[Channel::Gchat]));
}

#[test]
fn duplicate_project_input_does_not_overlap_itself() {
    let report = merge(
        host(HOST_ALL),
        vec![input(PROJ_A, A_V2), input(PROJ_A, A_V2)],
    );
    assert!(!report.denied, "{:#?}", report.findings);
    assert_eq!(names(&report), ["bob-dm", "bob-tg"]);
    assert_eq!(report.per_file.len(), 1);
}

#[test]
fn deny_all_clears_gchat_connection_and_spaces() {
    // B's space route is effective alone; A's overlap denies the load.
    let b = format!(
        "version = 1\n{}",
        gchat_janet("j-b", Some("spaces/AAAAexample"))
    );
    let alone = merge(host(HOST_ALL), vec![input(PROJ_B, &b)]);
    let status = &alone.per_file[0];
    assert!(status.gchat_connection.is_some() && !status.gchat_spaces.is_empty());
    let a = format!("version = 1\n{}", gchat_janet("j-a", None));
    let report = merge(host(HOST_ALL), vec![input(PROJ_B, &b), input(PROJ_A, &a)]);
    assert_denied(&report);
    for s in &report.per_file {
        assert_eq!(s.gchat_connection, None, "{s:?}");
        assert!(s.gchat_spaces.is_empty(), "{s:?}");
    }
}
