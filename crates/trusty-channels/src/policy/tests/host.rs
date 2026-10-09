//! Host ceiling (#8454 S2a): any fault in the `channels:` section denies
//! every route.

use std::path::{Path, PathBuf};

use super::{assert_denied, host, input, names, A_V2, B_V1, HOST_ALL, PROJ_A, PROJ_B};
use crate::policy::{
    merge, parse_host, Channel, FileState, Finding, HostError, MessageKind, RateLimit,
};

/// Merge `yaml` with the two good projects and assert the load is denied
/// by a `HostRefused` finding.
fn assert_host_denies(what: &str, result: Result<crate::policy::HostCeiling, HostError>) {
    let report = merge(result, vec![input(PROJ_A, A_V2), input(PROJ_B, B_V1)]);
    assert_denied(&report);
    assert!(
        matches!(report.findings.last(), Some(Finding::HostRefused { .. })),
        "{what}: {:?}",
        report.findings
    );
    assert!(
        report
            .per_file
            .iter()
            .all(|s| s.state == FileState::Withheld),
        "{what}: {:?}",
        report.per_file
    );
}

#[test]
fn host_all_fixture_loads_every_route() {
    let report = merge(
        host(HOST_ALL),
        vec![input(PROJ_A, A_V2), input(PROJ_B, B_V1)],
    );
    assert!(!report.denied, "{:?}", report.findings);
    assert_eq!(names(&report), ["bob-dm", "bob-tg", "janet"]);
    assert!(report.findings.is_empty(), "{:?}", report.findings);
}

#[test]
fn host_unknown_key_denies_all() {
    let cases = [
        (
            "channels level",
            HOST_ALL.replace("  version: 1", "  version: 1\n  extra: 1"),
        ),
        (
            "channel level",
            HOST_ALL.replace("  gchat:\n", "  gchat:\n    mode: x\n"),
        ),
        (
            "connection level",
            HOST_ALL.replace("credential_ref: slack", "credential_ref: slack, token: x"),
        ),
        (
            "rate_limit level",
            HOST_ALL.replace("window_secs: 60", "window_secs: 60, burst: 5"),
        ),
        (
            "unknown channel",
            HOST_ALL.replace("  telegram:", "  discord: { enabled: true }\n  telegram:"),
        ),
    ];
    for (what, yaml) in cases {
        let result = host(&yaml);
        assert!(
            matches!(result, Err(HostError::Invalid { .. })),
            "{what}: {result:?}\n{yaml}"
        );
        assert_host_denies(what, result);
    }
}

#[test]
fn no_channels_section_denies_all() {
    for yaml in [
        "",
        "# a comment only\n",
        "unrelated: 1\n",
        "channel:\n  version: 1\n",
    ] {
        let result = host(yaml);
        assert_eq!(result, Err(HostError::NoChannelsSection), "{yaml:?}");
        assert_host_denies(yaml, result);
    }
}

#[test]
fn host_faults_deny_all() {
    type Is = fn(&HostError) -> bool;
    let cases: Vec<(&str, String, Is)> = vec![
        ("not yaml", "channels: [unclosed\n".into(), |e| {
            matches!(e, HostError::Malformed { .. })
        }),
        ("top level a list", "- channels\n".into(), |e| {
            matches!(e, HostError::Malformed { .. })
        }),
        (
            "enabled not a bool",
            HOST_ALL.replacen("enabled: true", "enabled: maybe", 1),
            |e| matches!(e, HostError::Invalid { .. }),
        ),
        (
            "enabled missing",
            HOST_ALL.replace("  telegram:\n    enabled: true\n", "  telegram:\n"),
            |e| matches!(e, HostError::Invalid { .. }),
        ),
        ("channels null", "channels:\n".into(), |e| {
            matches!(e, HostError::Invalid { .. })
        }),
        (
            "version 2",
            HOST_ALL.replace("version: 1", "version: 2"),
            |e| matches!(e, HostError::Version { found: 2 }),
        ),
        (
            "rate limit zero",
            HOST_ALL.replace("limit: 100", "limit: 0"),
            |e| matches!(e, HostError::RateLimit { .. }),
        ),
        (
            "empty kinds ceiling",
            HOST_ALL.replace("  gchat:\n", "  gchat:\n    kinds: []\n"),
            |e| matches!(e, HostError::Kinds { channel: Channel::Gchat, .. }),
        ),
        (
            "review_notice ceiling on slack",
            HOST_ALL.replace("  slack:\n", "  slack:\n    kinds: [review_notice]\n"),
            |e| matches!(e, HostError::Kinds { channel: Channel::Slack, .. }),
        ),
        (
            "relative project",
            HOST_ALL.replacen("/work/a", "work/a", 1),
            |e| matches!(e, HostError::Project { .. }),
        ),
        (
            "project with ..",
            HOST_ALL.replacen("/work/a", "/work/../etc", 1),
            |e| matches!(e, HostError::Project { .. }),
        ),
        (
            "empty project",
            HOST_ALL.replacen("/work/a", "''", 1),
            |e| matches!(e, HostError::Project { .. }),
        ),
        (
            "gchat connection a resource path",
            HOST_ALL.replace(
                "  gchat:\n",
                "  gchat:\n    connection: { project_id: p, subscription: projects/p/subscriptions/s, key_file: /k.json }\n",
            ),
            |e| matches!(e, HostError::Connection { .. }),
        ),
        (
            "user token ref",
            HOST_ALL.replace("credential_ref: slack", "credential_ref: slack-user"),
            |e| matches!(e, HostError::CredentialRef { channel: Channel::Slack, .. }),
        ),
        (
            "token pasted as ref",
            HOST_ALL.replace("credential_ref: slack", "credential_ref: xoxb-123-pasted"),
            |e| matches!(e, HostError::CredentialRef { .. }),
        ),
    ];
    for (what, yaml, is) in cases {
        let result = host(&yaml);
        match &result {
            Err(e) => {
                assert!(is(e), "{what}: wrong error {e:?}");
                assert!(!e.to_string().contains("xoxb"), "{what}: value echoed: {e}");
            }
            Ok(_) => panic!("{what}: loaded\n{yaml}"),
        }
        assert_host_denies(what, result);
    }

    // `~/` with no known home is a ceiling fault, never a cwd-relative path.
    let yaml = HOST_ALL.replacen("/work/a", "~/work/a", 1);
    let result = parse_host(&yaml, None);
    assert!(
        matches!(result, Err(HostError::Project { .. })),
        "{result:?}"
    );
    assert_host_denies("~/ without home", result);
}

#[test]
fn host_ceiling_parses_every_field() {
    let yaml = "\
channels:
  version: 1
  rate_limit: { limit: 40, window_secs: 120 }
  gchat:
    enabled: true
    connection: { project_id: p, subscription: s, key_file: ~/sa.json }
    kinds: [question]
    projects: [~/proj, /abs/proj/]
  slack: { enabled: true, connection: { credential_ref: slack-app }, projects: [/abs] }
  telegram: { enabled: false, projects: [] }
";
    let ceiling = host(yaml).expect("valid ceiling");
    assert_eq!(ceiling.rate_limit().limit(), 40);
    assert_eq!(ceiling.rate_limit().window_secs(), 120);
    let gchat = ceiling.channel(Channel::Gchat).expect("gchat");
    assert!(gchat.enabled());
    let conn = gchat.gchat_connection().expect("connection");
    assert_eq!(conn.key_file, Path::new("/home/t/sa.json"));
    assert_eq!(
        gchat.kinds().iter().copied().collect::<Vec<_>>(),
        [MessageKind::Question]
    );
    assert_eq!(
        gchat.projects(),
        [PathBuf::from("/home/t/proj"), PathBuf::from("/abs/proj")]
    );
    assert!(
        gchat.lists(Path::new("/abs/proj")),
        "trailing slash compares equal"
    );
    let slack = ceiling.channel(Channel::Slack).expect("slack");
    assert_eq!(slack.credential_ref(), Some("slack-app"));
    assert_eq!(slack.kinds().len(), 1, "slack defaults to question only");
    let telegram = ceiling.channel(Channel::Telegram).expect("telegram");
    assert!(!telegram.enabled());

    let defaulted = host("channels:\n  version: 1\n").expect("no channels named");
    assert_eq!(defaulted.rate_limit(), RateLimit::DEFAULT);
    assert!(defaulted.channel(Channel::Gchat).is_none());
}
