//! Tests for the doctor drawer-count check and report (#9283).
//! Every test runs on a tempdir palace root; none reads the live data dir.

use super::*;
use crate::commands::doctor::CheckStatus;
use crate::drawer_counts::{take_snapshot, CountLine, CountSource};
use chrono::Duration;
use trusty_common::memory_core::maintenance_log::{record, DeletionReason, MaintenanceDeletion};
use trusty_common::memory_core::{Palace, PalaceId, PalaceRegistry};
use uuid::Uuid;

fn count(palace: &str, at: DateTime<Utc>, drawers: Option<usize>) -> CountLine {
    CountLine {
        v: counts::SCHEMA_VERSION,
        at,
        palace: palace.into(),
        drawers,
        src: if drawers.is_some() {
            CountSource::Disk
        } else {
            CountSource::Unavailable
        },
        ack: None,
        reason: None,
    }
}

fn write_history(root: &Path, lines: &[CountLine]) {
    let text: String = lines
        .iter()
        .map(|l| format!("{}\n", serde_json::to_string(l).expect("ser")))
        .collect();
    std::fs::write(counts::history_path(root), text).expect("write history");
}

/// Journal `n` deletions of `reason` in `palace`, stamped `at`.
fn journal(root: &Path, palace: &str, reason: DeletionReason, n: usize, at: DateTime<Utc>) {
    let dir = root.join(palace);
    std::fs::create_dir_all(&dir).expect("palace dir");
    for _ in 0..n {
        let mut rec = MaintenanceDeletion::new(&PalaceId::new(palace), Uuid::new_v4(), reason);
        rec.at = at;
        record(Some(&dir), &rec);
    }
}

fn detail(r: &CheckResult) -> String {
    r.detail.clone().unwrap_or_default()
}

/// A real palace on disk holding `drawers` drawers, left closed.
fn seed(root: &Path, name: &str, drawers: usize) {
    let registry = PalaceRegistry::with_max_open(4);
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.to_string(),
        description: None,
        created_at: Utc::now(),
        data_dir: root.join(name),
    };
    let handle = registry.create_palace(root, palace).expect("create_palace");
    for i in 0..drawers {
        let d = trusty_common::memory_core::Drawer::new(Uuid::new_v4(), format!("drawer {i}"));
        handle.kg.store().upsert_drawer(&d).expect("upsert");
    }
    drop(handle);
    registry.remove(&PalaceId::new(name));
}

/// Why (#9283 AC 2): a drop no journal record accounts for is silent loss.
/// What: 10 -> 7 with one journaled deletion leaves 2 unexplained: red.
#[test]
fn drawer_counts_doctor_goes_red_on_unjournaled_drop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let now = Utc::now();
    let t0 = now - Duration::days(1);
    write_history(
        tmp.path(),
        &[count("p", t0, Some(10)), count("p", now, Some(7))],
    );
    journal(
        tmp.path(),
        "p",
        DeletionReason::DreamDedup,
        1,
        now - Duration::hours(2),
    );
    let r = verdict(tmp.path(), now);
    assert_eq!(r.status, CheckStatus::Fail, "{r:?}");
    assert!(
        detail(&r).contains("p: 10 -> 7") && detail(&r).contains("2 unexplained"),
        "{r:?}"
    );
}

/// Why: a drop the journal fully accounts for is not loss — including records
/// stamped just before the earlier count (the 60 s slack, design §7).
#[test]
fn drawer_counts_doctor_green_when_journaled() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let now = Utc::now();
    let t0 = now - Duration::days(1);
    write_history(
        tmp.path(),
        &[count("p", t0, Some(10)), count("p", now, Some(7))],
    );
    journal(
        tmp.path(),
        "p",
        DeletionReason::DreamPrune,
        2,
        now - Duration::hours(3),
    );
    journal(
        tmp.path(),
        "p",
        DeletionReason::ExpiredPurge,
        1,
        t0 - Duration::seconds(30),
    );
    // Outside the window: neither may explain this drop.
    journal(
        tmp.path(),
        "p",
        DeletionReason::DreamPrune,
        5,
        t0 - Duration::hours(1),
    );
    let r = verdict(tmp.path(), now);
    assert_eq!(r.status, CheckStatus::Pass, "{r:?}");
}

/// Why (#9283 Gap A): a user forget is journaled now, so it explains its drop.
/// What: a resident palace, snapshot, `forget` through the handle, snapshot
/// again — the check is green and the journal reason is `user_forget`.
#[tokio::test]
async fn drawer_counts_doctor_green_for_user_forget() {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = PalaceRegistry::with_max_open(4);
    let palace = Palace {
        id: PalaceId::new("uf"),
        name: "uf".into(),
        description: None,
        created_at: Utc::now(),
        data_dir: tmp.path().join("uf"),
    };
    let handle = registry.create_palace(tmp.path(), palace).expect("create");
    let mut ids = Vec::new();
    for text in ["first fact to keep around", "second fact the user forgets"] {
        let id = handle
            .remember(
                text.into(),
                trusty_common::memory_core::RoomType::General,
                vec![],
                0.5,
            )
            .await
            .expect("remember");
        ids.push(id);
    }
    let t0 = Utc::now() - Duration::days(1);
    take_snapshot(&registry, tmp.path(), t0).expect("snapshot 1");
    handle.forget(ids[1]).await.expect("forget");
    let t1 = Utc::now() + Duration::seconds(1);
    take_snapshot(&registry, tmp.path(), t1).expect("snapshot 2");

    let r = verdict(tmp.path(), t1);
    assert_eq!(r.status, CheckStatus::Pass, "{r:?}");
    let report = build_report(tmp.path(), t1, 7).expect("report");
    assert_eq!(report.palaces[0].journaled.get("user_forget"), Some(&1));
}

/// Why (design §2 Gap B): the health-probe palace's drawers come and go by
/// design; neither the writer nor the check may count it.
#[test]
fn drawer_counts_ignores_health_probe_palace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let probe = crate::transport::methods::HEALTH_PROBE_PALACE;
    seed(tmp.path(), probe, 2);
    seed(tmp.path(), "real", 1);
    let registry = PalaceRegistry::with_max_open(4);
    take_snapshot(&registry, tmp.path(), Utc::now()).expect("snapshot");
    let history = counts::read_history(tmp.path()).expect("read");
    assert!(
        history.lines.iter().all(|l| l.palace != probe),
        "{history:?}"
    );

    let now = Utc::now();
    let t0 = now - Duration::days(1);
    write_history(
        tmp.path(),
        &[
            count(probe, t0, Some(5)),
            count(probe, now, Some(0)),
            count("real", t0, Some(1)),
            count("real", now, Some(1)),
        ],
    );
    let r = verdict(tmp.path(), now);
    assert_eq!(r.status, CheckStatus::Pass, "{r:?}");
    assert!(!detail(&r).contains(probe), "{r:?}");
}

/// Why (design §2): a `*.v2-incompatible` quarantine replaces a store with an
/// empty one and journals nothing — the alarm this check exists for.
/// What: a real 5-drawer palace is counted, its store is swapped for an empty
/// one under the same id, and the next count turns doctor red.
#[test]
fn v2_incompatible_reset_to_empty_turns_doctor_red() {
    let tmp = tempfile::tempdir().expect("tempdir");
    seed(tmp.path(), "victim", 5);
    let registry = PalaceRegistry::with_max_open(4);
    let now = Utc::now();
    take_snapshot(&registry, tmp.path(), now - Duration::days(1)).expect("snapshot 1");
    let dir = tmp.path().join("victim");
    std::fs::rename(dir.join("kg.redb"), dir.join("kg.redb.v2-incompatible")).expect("quarantine");
    std::fs::remove_dir_all(&dir).expect("reset");
    seed(tmp.path(), "victim", 0);
    take_snapshot(&registry, tmp.path(), now).expect("snapshot 2");

    let r = verdict(tmp.path(), now);
    assert_eq!(r.status, CheckStatus::Fail, "{r:?}");
    assert!(detail(&r).contains("victim: 5 -> 0"), "{r:?}");
}

/// Why (ADR-0067 D2): a newer binary's line must survive this binary's
/// rewrite, and this binary must refuse to judge a history it cannot read.
#[test]
fn future_schema_line_is_refused_not_dropped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let future =
        r#"{"v":2,"at":"2026-10-01T00:00:00Z","palace":"p","drawers":3,"src":"disk","extra":1}"#;
    std::fs::write(counts::history_path(tmp.path()), format!("{future}\n")).expect("w");
    seed(tmp.path(), "p", 1);
    let registry = PalaceRegistry::with_max_open(4);
    let out = take_snapshot(&registry, tmp.path(), Utc::now()).expect("snapshot");
    assert_eq!(out.written, 1);
    let text = std::fs::read_to_string(counts::history_path(tmp.path())).expect("read");
    assert!(
        text.lines().any(|l| l == future),
        "the newer line was dropped: {text}"
    );

    let r = verdict(tmp.path(), Utc::now());
    assert_eq!(r.status, CheckStatus::Unknown, "{r:?}");
    assert!(detail(&r).contains("newer than v1"), "{r:?}");
    assert!(build_report(tmp.path(), Utc::now(), 7).is_err());
}

/// A seven-day history for palace `a`: one journaled drop, one unexplained.
fn week(root: &Path, now: DateTime<Utc>) {
    let counts_by_day = [10, 10, 9, 9, 7, 7, 7];
    let lines: Vec<CountLine> = counts_by_day
        .iter()
        .enumerate()
        .map(|(i, n)| count("a", now - Duration::days(6 - i as i64), Some(*n)))
        .collect();
    write_history(root, &lines);
    journal(
        root,
        "a",
        DeletionReason::DreamDedup,
        1,
        now - Duration::days(4) - Duration::hours(1),
    );
}

/// Why (#9283 AC 3): the 7-day report is the 1.0 release signal.
/// What: seven daily counts with one unexplained drop of 2 on day 5 give six
/// green days, `6/7 clean`, the dedup reason, and a failing exit.
#[test]
fn report_lists_seven_days_and_exits_one_on_unexplained() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let now = Utc::now();
    week(tmp.path(), now);
    let r = build_report(tmp.path(), now, 7).expect("report");
    assert_eq!(r.days.len(), 7);
    assert_eq!(r.palaces.len(), 1);
    let a = &r.palaces[0];
    assert_eq!(a.counts.len(), 7);
    assert_eq!(a.net_delta, Some(-3));
    assert_eq!(a.journaled.get("dream_dedup"), Some(&1));
    let bad_day = (now - Duration::days(2)).date_naive();
    assert_eq!(a.unexplained.get(&bad_day), Some(&2), "{a:?}");
    assert_eq!(r.unexplained_total, 2);
    assert_eq!(r.green_days, 6);
    assert_eq!(r.clean, "6/7 clean");
    assert!(r.failed(), "an unexplained drop must exit 1");
    assert!(!r.stale);
}

/// Why: the operator reads the text form; the unexplained day must be named.
#[test]
fn report_text_names_unexplained_days() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let now = Utc::now();
    week(tmp.path(), now);
    let text = render_text(&build_report(tmp.path(), now, 7).expect("report"));
    let bad_day = (now - Duration::days(2)).date_naive();
    assert!(
        text.contains(&format!("UNEXPLAINED {bad_day}: 2")),
        "{text}"
    );
    assert!(text.contains("[10 10 9 9 7 7 7]"), "{text}");
    assert!(text.contains("6/7 clean"), "{text}");
}

/// Why (design §4): a palace missing from the newest day is a removal to
/// warn about, an unreadable count is unknown, and no history is pending.
#[test]
fn removed_unavailable_and_pending_palaces_are_not_red() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let now = Utc::now();
    let r = verdict(tmp.path(), now);
    assert_eq!(r.status, CheckStatus::Unknown, "{r:?}");
    assert!(detail(&r).contains("baseline pending"), "{r:?}");

    let t0 = now - Duration::days(1);
    write_history(
        tmp.path(),
        &[
            count("gone", t0, Some(4)),
            count("held", t0, Some(4)),
            count("held", now, None),
        ],
    );
    let r = verdict(tmp.path(), now);
    assert_eq!(r.status, CheckStatus::Warn, "{r:?}");
    assert!(detail(&r).contains("palace removed: gone"), "{r:?}");
    assert!(
        detail(&r).contains("1 palace(s) uncountable: held: reason not recorded"),
        "{r:?}"
    );
}

/// Why (#9283, Fail-Open Check): an uncountable palace can never go red, so
/// a check that passes beside one reads as healthy while it cannot see loss.
/// What: one palace compares clean, one is unavailable with a reason; the
/// check warns and names the palace and the reason.
#[test]
fn an_uncountable_palace_is_named_and_never_green() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let now = Utc::now();
    let t0 = now - Duration::days(1);
    let mut held = count("held", now, None);
    held.reason = Some("kg.redb is not readable: Database already open".into());
    write_history(
        tmp.path(),
        &[
            count("ok", t0, Some(5)),
            count("ok", now, Some(5)),
            count("held", t0, Some(4)),
            held,
        ],
    );
    let r = verdict(tmp.path(), now);
    assert_eq!(r.status, CheckStatus::Warn, "{r:?}");
    assert!(
        detail(&r).contains(
            "1 palace(s) uncountable: held: kg.redb is not readable: Database already open"
        ),
        "{r:?}"
    );
}

/// Why (#9283, Fail-Open Check): a palace whose deletion journal cannot be
/// read can never go red, so a check that passes beside it reads as healthy
/// while it cannot judge a drop.
/// What: one palace compares clean, the other's journal path is a directory;
/// the check warns and names the palace and the read error.
#[test]
fn an_unreadable_journal_is_named_and_never_green() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let now = Utc::now();
    let t0 = now - Duration::days(1);
    write_history(
        tmp.path(),
        &[
            count("ok", t0, Some(5)),
            count("ok", now, Some(5)),
            count("broken", t0, Some(4)),
            count("broken", now, Some(4)),
        ],
    );
    let journal_path = tmp
        .path()
        .join("broken")
        .join("maintenance_deletions.jsonl");
    std::fs::create_dir_all(&journal_path).expect("journal path as a directory");
    let r = verdict(tmp.path(), now);
    assert_eq!(r.status, CheckStatus::Warn, "{r:?}");
    assert!(detail(&r).contains("journal unreadable: broken: "), "{r:?}");
}

/// Why (#9283): one day of counts is the expected first-day state, not an
/// undetermined probe — reporting it Unknown made every first-day doctor run
/// exit 1 (#4001 counts Unknown as unhealthy). It is still not green.
/// What: one snapshot day, every palace counted: Warn naming the pending
/// baseline; an empty history stays Unknown.
#[test]
fn a_first_day_baseline_warns_instead_of_undetermined() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let now = Utc::now();
    write_history(
        tmp.path(),
        &[count("a", now, Some(3)), count("b", now, Some(0))],
    );
    let r = verdict(tmp.path(), now);
    assert_eq!(r.status, CheckStatus::Warn, "{r:?}");
    assert!(
        detail(&r).contains("baseline pending: 2 palace(s) counted once"),
        "{r:?}"
    );
}

#[derive(clap::Parser)]
struct Cli {
    #[command(flatten)]
    args: DrawerCountArgs,
}

/// Why: the report and ack flags are the operator surface the design names.
#[test]
fn doctor_drawer_flags_parse() {
    use clap::Parser;
    let r = Cli::try_parse_from(["doctor", "--drawer-report", "--days", "3", "--json"])
        .expect("report flags");
    assert!(r.args.drawer_report && r.args.json && r.args.is_mode());
    assert_eq!(r.args.days, 3);
    let a = Cli::try_parse_from([
        "doctor",
        "--ack-drop",
        "p",
        "--count",
        "4",
        "--reason",
        "reclaim",
    ])
    .expect("ack flags");
    assert_eq!(a.args.ack_drop.as_deref(), Some("p"));
    assert!(Cli::try_parse_from(["doctor", "--ack-drop", "p"]).is_err());
    assert!(Cli::try_parse_from(["doctor", "--json"]).is_err());
    assert!(!Cli::try_parse_from(["doctor"])
        .expect("bare")
        .args
        .is_mode());
}
