//! `channels.slack_read_allowlist` (#10387): strict parse, every fault
//! refuses the whole host, and the guard's host-only read.

use std::path::Path;

use super::repo::tempdir;
use super::{host, HOST_ALL};
use crate::policy::{read_host_ceiling, HostError, SLACK_READ_TOOLS};

/// [`HOST_ALL`] with `entries` (YAML list items, two-space indented under
/// `channels:`) as its `slack_read_allowlist`.
fn with_allowlist(entries: &str) -> String {
    format!("{HOST_ALL}  slack_read_allowlist:\n{entries}")
}

/// The tm-apex entry granting both read tools.
const TM_APEX: &str = concat!(
    "    - project: /work/tm-apex\n",
    "      name: tm-apex\n",
    "      tools: [slack_read_channel, slack_read_thread]\n",
);

#[test]
fn slack_read_allowlist_parses_both_tools() {
    let ceiling = host(&with_allowlist(TM_APEX)).expect("parses");
    let list = ceiling.slack_read_allowlist();
    assert_eq!(list.entries().len(), 1);
    let entry = list.entry_for(Path::new("/work/tm-apex")).expect("entry");
    assert_eq!(entry.name(), "tm-apex");
    assert_eq!(entry.tools(), SLACK_READ_TOOLS);
    assert!(list.entry_for(Path::new("/work/a")).is_none());
    // No key at all is an empty allowlist, not a fault.
    let plain = host(HOST_ALL).expect("parses");
    assert!(plain.slack_read_allowlist().entries().is_empty());
}

#[test]
fn slack_read_allowlist_refuses_each_bad_entry() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "posting tool",
            "    - { project: /p, name: p, tools: [slack_send_message] }\n",
            "not slack_read_channel or slack_read_thread",
        ),
        (
            "canvas read",
            "    - { project: /p, name: p, tools: [slack_read_canvas] }\n",
            "not slack_read_channel or slack_read_thread",
        ),
        (
            "file read",
            "    - { project: /p, name: p, tools: [slack_read_file] }\n",
            "not slack_read_channel or slack_read_thread",
        ),
        (
            "case variant",
            "    - { project: /p, name: p, tools: [Slack_Read_Channel] }\n",
            "not slack_read_channel or slack_read_thread",
        ),
        (
            "empty tools",
            "    - { project: /p, name: p, tools: [] }\n",
            "tools is empty",
        ),
        (
            "duplicate tool",
            "    - { project: /p, name: p, tools: [slack_read_thread, slack_read_thread] }\n",
            "twice",
        ),
        (
            "relative project",
            "    - { project: work/p, name: p, tools: [slack_read_thread] }\n",
            "not an absolute path",
        ),
        (
            "dotdot project",
            "    - { project: /work/../p, name: p, tools: [slack_read_thread] }\n",
            "..",
        ),
        (
            "project twice",
            concat!(
                "    - { project: /p, name: p, tools: [slack_read_thread] }\n",
                "    - { project: /p, name: q, tools: [slack_read_channel] }\n",
            ),
            "listed twice",
        ),
        (
            "bad name",
            "    - { project: /p, name: \"tm apex\", tools: [slack_read_thread] }\n",
            "name must be",
        ),
    ];
    for (what, entries, want) in cases {
        let err = host(&with_allowlist(entries)).expect_err(what);
        assert!(
            matches!(err, HostError::SlackReadAllowlist { .. }),
            "{what}: {err:?}"
        );
        assert!(err.to_string().contains(want), "{what}: {err}");
    }
}

#[test]
fn slack_read_allowlist_refuses_a_wildcard() {
    for tools in ["[\"*\"]", "[\"slack_read_*\"]", "[\"slack_read_?hread\"]"] {
        let entries = format!("    - {{ project: /p, name: p, tools: {tools} }}\n");
        let err = host(&with_allowlist(&entries)).expect_err(tools);
        assert_eq!(
            err.to_string(),
            "channels.slack_read_allowlist[0]: tools holds a wildcard; name each tool exactly",
            "{tools}"
        );
    }
}

#[test]
fn slack_read_allowlist_unknown_key_or_shape_is_invalid() {
    for (what, entries) in [
        (
            "unknown entry key",
            "    - { project: /p, name: p, tools: [slack_read_thread], all: true }\n",
        ),
        (
            "missing name",
            "    - { project: /p, tools: [slack_read_thread] }\n",
        ),
        ("not a list", "    project: /p\n"),
    ] {
        let err = host(&with_allowlist(entries)).expect_err(what);
        assert!(matches!(err, HostError::Invalid { .. }), "{what}: {err:?}");
    }
}

#[test]
fn read_host_ceiling_reads_the_allowlist_and_refuses_faults() {
    let (_tmp, home) = tempdir();
    let path = trusty_common::crate_config::crate_config_path_at(&home, "trusty-mpm");
    assert_eq!(read_host_ceiling(&home), Err(HostError::Missing));
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, with_allowlist(TM_APEX)).expect("write");
    let ceiling = read_host_ceiling(&home).expect("reads");
    assert!(ceiling
        .slack_read_allowlist()
        .entry_for(Path::new("/work/tm-apex"))
        .is_some());
    std::fs::write(&path, "channels: [").expect("write");
    assert!(matches!(
        read_host_ceiling(&home),
        Err(HostError::Malformed { .. })
    ));
}
