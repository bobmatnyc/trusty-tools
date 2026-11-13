//! Tests for `lifecycle.rs`.
//!
//! Why a sibling file: the #10975 presence filter added a fake host and four
//! regression tests, and `lifecycle.rs` is production source under the 500-SLOC
//! cap. The `service_bootstrap_tests.rs` pattern keeps the tests out of it.
//!
//! What: ordering, the report shape and exit code, the verb names, and the
//! #10975 not-installed filter driven end to end through [`HostFake`].

use super::*;
use crate::commands::service_bootstrap::ServiceEnv;
use std::cell::RefCell;
use trusty_common::launchd_labels::LabelEviction;

fn member(binary: &str, manage: ManageStrategy) -> StableMember {
    StableMember {
        crate_name: binary.to_owned(),
        binary: binary.to_owned(),
        daemon: manage != ManageStrategy::None,
        manage,
        required: true,
        soft: false,
        internal: false,
    }
}

/// Per-member fake host for the #10975 lifecycle tests.
///
/// Why: the presence filter reads three signals per member, so the fake holds
/// each signal per binary. It also records every port-guard call: each
/// lifecycle branch that would `launchctl bootstrap` (start or restart) calls
/// the port guard first, so an empty record proves no such branch ran.
/// What: the port guard REFUSES unless `port_clear` is set, so a test that
/// wrongly reaches a restart stops before `restart_gracefully` and never bounces
/// a daemon on the machine running the tests.
#[derive(Default)]
struct HostFake {
    binaries: Vec<&'static str>,
    plists: Vec<&'static str>,
    loaded: Vec<&'static str>,
    port_clear: bool,
    port_guard_calls: RefCell<Vec<String>>,
    installs: RefCell<Vec<String>>,
}

impl ServiceEnv for HostFake {
    fn plist_present(&self, binary: &str) -> bool {
        self.plists.contains(&binary)
    }
    fn run_service_install(
        &self,
        binary: &str,
        _exe_path: Option<&std::path::Path>,
    ) -> anyhow::Result<()> {
        self.installs.borrow_mut().push(binary.to_owned());
        Ok(())
    }
    fn is_loaded(&self, binary: &str) -> bool {
        self.loaded.contains(&binary)
    }
    fn bootstrap_fallback(&self, binary: &str) -> anyhow::Result<()> {
        anyhow::bail!("lifecycle never calls bootstrap_fallback ({binary})")
    }
    fn port_guard(&self, binary: &str) -> Result<(), String> {
        self.port_guard_calls.borrow_mut().push(binary.to_owned());
        if self.port_clear {
            Ok(())
        } else {
            Err(format!("test fake: port guard refuses {binary}"))
        }
    }
    fn evict_retired(&self, _binary: &str) -> Vec<LabelEviction> {
        Vec::new()
    }
    fn binary_resolves(&self, binary: &str) -> bool {
        self.binaries.contains(&binary)
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

/// Why: start must be forward order, stop/restart reverse, and non-daemons
/// must be filtered out.
/// What: orders a 3-member list for each verb; asserts direction + filtering.
/// Test: This is the test.
#[test]
fn ordering_is_directional() {
    let sel = vec![
        member("trusty-search", ManageStrategy::Launchd),
        member("tga", ManageStrategy::None),
        member("trusty-mpm", ManageStrategy::OwnVerb),
    ];
    let start: Vec<String> = order_for(Verb::Start, &sel)
        .into_iter()
        .map(|m| m.binary)
        .collect();
    assert_eq!(start, vec!["trusty-search", "trusty-mpm"]);

    let stop: Vec<String> = order_for(Verb::Stop, &sel)
        .into_iter()
        .map(|m| m.binary)
        .collect();
    assert_eq!(stop, vec!["trusty-mpm", "trusty-search"]);

    let restart: Vec<String> = order_for(Verb::Restart, &sel)
        .into_iter()
        .map(|m| m.binary)
        .collect();
    assert_eq!(restart, vec!["trusty-mpm", "trusty-search"]);
}

/// Why: the verb name is used as both a label and a subcommand; pin it.
/// What: asserts each verb's name.
/// Test: This is the test.
#[test]
fn verb_name() {
    assert_eq!(Verb::Start.name(), "start");
    assert_eq!(Verb::Stop.name(), "stop");
    assert_eq!(Verb::Restart.name(), "restart");
}

/// Why: the JSON envelope is a public contract; pin its shape.
/// What: builds a report and asserts keys.
/// Test: This is the test.
#[test]
fn report_serialises() {
    let report = LifecycleReport::build(
        Verb::Start,
        vec![LifecycleOutcome {
            member: "trusty-search".to_owned(),
            ok: true,
            detail: "bootstrapped com.trusty.trusty-search".to_owned(),
        }],
    );
    let v = serde_json::to_value(&report).expect("serialises");
    assert_eq!(v["verb"], "start");
    assert_eq!(v["all_ok"], true);
    assert_eq!(v["members"][0]["member"], "trusty-search");
}

/// Why: the exit code must track the verdict for automation.
/// What: asserts 0 for all-ok, 2 for any failure.
/// Test: This is the test.
#[test]
fn exit_code_reflects_all_ok() {
    let ok = LifecycleReport::build(
        Verb::Stop,
        vec![LifecycleOutcome {
            member: "a".to_owned(),
            ok: true,
            detail: String::new(),
        }],
    );
    assert_eq!(ok.exit_code(), 0);
    let bad = LifecycleReport::build(
        Verb::Stop,
        vec![LifecycleOutcome {
            member: "a".to_owned(),
            ok: false,
            detail: "boom".to_owned(),
        }],
    );
    assert_eq!(bad.exit_code(), 2);
}

/// Why: a process-managed member (mpm) must route through its own verb, not
/// launchd. `apply_to_member` for an absent OwnVerb binary must error with a
/// "not installed" message (proving it took the OwnVerb path, not launchd).
/// What: applies Start to an absent OwnVerb member; asserts the error text.
/// Test: This is the test.
#[test]
fn own_verb_path_for_absent_binary_errors() {
    let m = member("definitely-not-a-real-binary-xyz", ManageStrategy::OwnVerb);
    let err = apply_to_member(&RealServiceEnv, Verb::Start, &m).expect_err("absent binary errors");
    assert!(err.to_string().contains("not installed"));
}

/// REGRESSION (#10975): a member counts as present when ANY of its binary,
/// plist or loaded label is on the host — not on the plist alone.
///
/// Why: binary-only must stay present so `start` installs its service (#2556);
/// loaded-without-plist must stay present so the preflight can refuse it.
/// What: checks each single signal, and none.
/// Test: This is the test.
#[test]
fn presence_reads_all_three_signals() {
    let b = "trusty-search";
    let none = HostFake::default();
    assert!(!member_installed(&none, b));
    let binary = HostFake {
        binaries: vec![b],
        ..HostFake::default()
    };
    assert!(member_installed(&binary, b));
    let plist = HostFake {
        plists: vec![b],
        ..HostFake::default()
    };
    assert!(member_installed(&plist, b));
    let loaded = HostFake {
        loaded: vec![b],
        ..HostFake::default()
    };
    assert!(member_installed(&loaded, b));
}

/// REGRESSION (#10975): a bare `tctl start|stop|restart` skips every member
/// not installed on the host, with no failure and no launchd action.
///
/// Why: on a host without trusty-events, `tctl restart` booted out and failed
/// to bootstrap `com.trusty.events` (error 5) and exited nonzero; with
/// trusty-mpm absent it failed "not installed", exit 2.
/// What: an empty host; each verb with no member named exits 0 and reaches
/// neither the port guard nor `service install`.
/// Test: This is the test.
#[test]
fn bare_verbs_skip_members_that_are_not_installed() {
    for verb in [Verb::Restart, Verb::Start, Verb::Stop] {
        let host = HostFake::default();
        let code = run_lifecycle_in(&host, verb, &[], true, false);
        assert_eq!(code, 0, "bare {} on an empty host must exit 0", verb.name());
        assert!(host.port_guard_calls.borrow().is_empty(), "{}", verb.name());
        assert!(host.installs.borrow().is_empty(), "{}", verb.name());
    }
}

/// REGRESSION (#10975): naming a member that is not installed exits 2 and acts
/// on nothing, including the installed members named beside it.
///
/// Why: `tctl stop trusty-events` printed "ok booted out" for a unit that never
/// existed; `restart` failed with launchd's raw error 5.
/// What: trusty-search present, trusty-events absent. Naming the absent
/// member, alone or beside the present one, exits 2 with no port-guard call.
/// trusty-search has its binary only, so a regression that did act on it
/// stops at the preflight or the refusing port guard, never a real bootout.
/// Test: This is the test.
#[test]
fn named_member_not_installed_exits_2_with_no_action() {
    for verb in [Verb::Restart, Verb::Start, Verb::Stop] {
        for named in [&["trusty-events"][..], &["trusty-search", "trusty-events"]] {
            let host = HostFake {
                binaries: vec!["trusty-search"],
                ..HostFake::default()
            };
            let code = run_lifecycle_in(&host, verb, &names(named), true, false);
            assert_eq!(code, 2, "{} {named:?}", verb.name());
            assert!(
                host.port_guard_calls.borrow().is_empty(),
                "{} {named:?} must act on nothing",
                verb.name()
            );
        }
    }
}

/// REGRESSION (#10975, keeps #2556): `start` of a member whose binary is present
/// but whose plist is absent still runs `service install`.
///
/// Why: plist absence must not read as "not installed", or a fresh host never
/// gets its services.
/// What: binary only; `tctl start trusty-search` exits 0 and installs once.
/// macOS-only because launchd control, and so `service install`, exists only
/// there; the presence half is pinned cross-platform by
/// `presence_reads_all_three_signals`.
/// Test: This is the test.
#[cfg(target_os = "macos")]
#[test]
fn start_installs_a_member_whose_plist_is_absent() {
    let host = HostFake {
        binaries: vec!["trusty-search"],
        port_clear: true,
        ..HostFake::default()
    };
    let code = run_lifecycle_in(&host, Verb::Start, &names(&["trusty-search"]), true, false);
    assert_eq!(code, 0);
    assert_eq!(*host.installs.borrow(), vec!["trusty-search".to_owned()]);
}

/// REGRESSION (#10975): a loaded label whose plist is gone is an error before
/// any bootout, so the running daemon is left alone.
///
/// Why: booting it out killed a daemon launchd could not bootstrap again.
/// What: `tctl restart` of such a member exits 2 without reaching the port
/// guard, which the restart path calls before its bootout. Stop is checked at
/// the preflight only: driving it end to end would, on a regression, issue a
/// real `launchctl bootout` on the test host. Both errors name the plist.
/// Test: This is the test.
#[test]
fn loaded_label_without_plist_errors_before_bootout() {
    let host = HostFake {
        binaries: vec!["trusty-search"],
        loaded: vec!["trusty-search"],
        ..HostFake::default()
    };
    let code = run_lifecycle_in(
        &host,
        Verb::Restart,
        &names(&["trusty-search"]),
        true,
        false,
    );
    assert_eq!(code, 2);
    assert!(host.port_guard_calls.borrow().is_empty());
    for verb in [Verb::Restart, Verb::Stop] {
        let err = launchd_preflight(&host, verb, "trusty-search").expect_err("loaded, no plist");
        assert!(err.to_string().contains("plist is gone"), "{err}");
    }
}

/// REGRESSION (#10975): restarting a member that has no plist and no loaded
/// label (installed with `--no-service`) is a clear skip, never error 5.
///
/// Why: `tctl upgrade` restarts through the same path and printed the raw
/// launchd error for such a member.
/// What: the preflight answers with a note for restart and stop, and lets
/// start through to its `service install`.
/// Test: This is the test.
#[test]
fn preflight_skips_an_unloaded_member_without_plist() {
    let host = HostFake {
        binaries: vec!["trusty-search"],
        ..HostFake::default()
    };
    for verb in [Verb::Restart, Verb::Stop] {
        let note = launchd_preflight(&host, verb, "trusty-search")
            .expect("no error")
            .expect("a skip note");
        assert!(note.contains("no launchd plist"), "{note}");
    }
    assert!(launchd_preflight(&host, Verb::Start, "trusty-search")
        .expect("no error")
        .is_none());
}
