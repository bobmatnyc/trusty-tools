use super::*;
use crate::assistants::AssistantInstanceId;
use chrono::TimeZone;
fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, 31, 12, 0, 0).unwrap()
}
fn fixture() -> (tempfile::TempDir, KnowledgeStore) {
    let temp = tempfile::tempdir().unwrap();
    let home = AssistantHome::under(
        temp.path().canonicalize().unwrap(),
        AssistantInstanceId::new("test-assistant").unwrap(),
    );
    (temp, KnowledgeStore::new(home))
}
fn source(revision: &str) -> SourceDescriptor {
    SourceDescriptor {
        id: "slack:opaque".into(),
        revision: revision.into(),
        kind: SourceKind::Slack,
        display_name: "Work".into(),
        dependency_reasons: vec!["history_cursor_required".into()],
    }
}
#[test]
fn status_does_not_provision_and_restart_preserves_identity() {
    let (_temp, store) = fixture();
    assert!(store.status().unwrap().is_none());
    assert!(!store.home.path().exists());
    let initial = store.initialize(now(), None).unwrap();
    assert!(initial.store.protected);
    assert_eq!(initial, store.status().unwrap().unwrap());
    assert_eq!(initial, store.initialize(now(), None).unwrap());
    let other = KnowledgeStore::new(AssistantHome::under(
        store.home.path().parent().unwrap(),
        AssistantInstanceId::new("other").unwrap(),
    ));
    assert_ne!(
        initial.store.index_id,
        other.initialize(now(), None).unwrap().store.index_id
    );
}
#[test]
fn anchored_months_avoid_end_of_month_drift() {
    let (_temp, store) = fixture();
    let s = store.initialize(now(), None).unwrap();
    let s = store.reconcile(&s.revision, &[source("1")], now()).unwrap();
    assert_eq!(
        s.jobs[0].window.start,
        Utc.with_ymd_and_hms(2026, 2, 28, 12, 0, 0).unwrap()
    );
    let extended = store.extend_history(&s.revision, 1, now()).unwrap();
    assert!(
        extended
            .jobs
            .iter()
            .any(|j| j.window.start == Utc.with_ymd_and_hms(2026, 1, 31, 12, 0, 0).unwrap())
    );
    assert!(matches!(
        store.extend_history(&s.revision, 1, now()),
        Err(KnowledgeError::Conflict)
    ));
    assert!(
        extended
            .jobs
            .iter()
            .all(|j| j.status == JobStatus::BlockedOnDependency)
    );
}
#[test]
fn reconciliation_is_idempotent_and_revocation_cancels_all_stages() {
    let (_temp, store) = fixture();
    let s = store.initialize(now(), None).unwrap();
    let s = store.reconcile(&s.revision, &[source("1")], now()).unwrap();
    assert_eq!(
        s,
        store.reconcile(&s.revision, &[source("1")], now()).unwrap()
    );
    let s = store.reconcile(&s.revision, &[source("2")], now()).unwrap();
    assert_eq!(s.jobs.len(), 2);
    assert_eq!(s.jobs[0].status, JobStatus::Cancelled);
    assert_eq!(s.jobs[0].publication.status, JobStatus::Cancelled);
    assert!(
        store
            .admit_event(&s.revision, "slack:opaque", "1", "event", now(), now())
            .is_err()
    );
}
#[test]
fn late_record_is_admitted_once_without_widening_history() {
    let (_temp, store) = fixture();
    let s = store.initialize(now(), None).unwrap();
    let s = store.reconcile(&s.revision, &[source("1")], now()).unwrap();
    let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
    let s = store
        .admit_event(
            &s.revision,
            "slack:opaque",
            "1",
            "secret-event-id",
            old,
            now(),
        )
        .unwrap();
    assert_eq!(s.history_months, 1);
    assert_eq!(s.jobs.len(), 2);
    assert_eq!(s.jobs[1].record.as_ref().unwrap().record_time, old);
    assert_eq!(s.jobs[1].window.start, now());
    assert_eq!(
        s,
        store
            .admit_event(
                &s.revision,
                "slack:opaque",
                "1",
                "secret-event-id",
                old,
                now()
            )
            .unwrap()
    );
    assert!(
        !std::fs::read_to_string(store.state_path())
            .unwrap()
            .contains("secret-event-id")
    );
}
#[test]
fn paused_admission_and_atomic_attachment_update() {
    let (_temp, store) = fixture();
    let s = store.initialize(now(), None).unwrap();
    let s = store
        .update_projects(
            &s.revision,
            "chat",
            &["/registered/project".into()],
            &[source("1")],
            now(),
        )
        .unwrap();
    assert_eq!(s.projects_by_chat["chat"], vec!["/registered/project"]);
    let s = store.set_paused(&s.revision, true, now()).unwrap();
    assert_eq!(
        s,
        store
            .admit_event(&s.revision, "slack:opaque", "1", "event", now(), now())
            .unwrap()
    );
    assert!(
        store
            .update_projects(&s.revision, "chat", &[], &[source("1"), source("1")], now())
            .is_err()
    );
    assert_eq!(store.status().unwrap().unwrap(), s);
}
#[test]
fn concurrent_writers_have_exactly_one_revision_winner() {
    let (_temp, store) = fixture();
    let s = store.initialize(now(), None).unwrap();
    let home = store.home.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let b = barrier.clone();
            let h = home.clone();
            let rev = s.revision.clone();
            std::thread::spawn(move || {
                b.wait();
                KnowledgeStore::new(h).extend_history(&rev, 1, now())
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(store.status().unwrap().unwrap().history_months, 2);
}
#[test]
fn invalid_dates_limits_and_shared_store_fail_without_provisioning() {
    let (_temp, store) = fixture();
    let invalid = ProtectedStore {
        root: store.home.path().parent().unwrap().join("shared"),
        index_id: "shared".into(),
        protected: true,
    };
    assert!(store.initialize(now(), Some(invalid)).is_err());
    assert!(!store.home.path().exists());
    let s = store.initialize(now(), None).unwrap();
    assert!(store.extend_history(&s.revision, 120, now()).is_err());
    assert!(
        store
            .reconcile(&s.revision, &[], now() - chrono::Duration::days(1))
            .is_err()
    );
}
#[cfg(unix)]
#[test]
fn symlink_store_and_state_are_rejected() {
    use std::os::unix::fs::symlink;
    let (temp, store) = fixture();
    std::fs::create_dir(store.home.path()).unwrap();
    symlink(temp.path(), store.home.okg_dir()).unwrap();
    assert!(store.initialize(now(), None).is_err());
    std::fs::remove_file(store.home.okg_dir()).unwrap();
    let s = store.initialize(now(), None).unwrap();
    std::fs::remove_file(store.state_path()).unwrap();
    symlink(temp.path().join("other.json"), store.state_path()).unwrap();
    assert!(store.status().is_err());
    assert!(store.set_paused(&s.revision, true, now()).is_err());
}

#[test]
fn continuous_windows_remain_half_open_and_preserve_anchor() {
    let (_temp, store) = fixture();
    let s = store.initialize(now(), None).unwrap();
    let later = Utc.with_ymd_and_hms(2026, 5, 31, 12, 0, 0).unwrap();
    let s = store.reconcile(&s.revision, &[source("1")], later).unwrap();
    assert_eq!(s.jobs.len(), 3);
    assert_eq!(
        s.jobs[1].window.end,
        Utc.with_ymd_and_hms(2026, 4, 30, 12, 0, 0).unwrap()
    );
    assert_eq!(s.jobs[2].window.end, later);
    let s = store
        .admit_event(
            &s.revision,
            "slack:opaque",
            "1",
            "at-boundary",
            later,
            later,
        )
        .unwrap();
    assert_eq!(s.jobs.last().unwrap().window.start, later);
    assert_eq!(s.anchor_at, now());
}
#[test]
fn corrupt_or_cross_assistant_state_fails_closed() {
    let (_temp, store) = fixture();
    let mut s = store.initialize(now(), None).unwrap();
    s.assistant_id = "another-assistant".into();
    std::fs::write(store.state_path(), serde_json::to_vec(&s).unwrap()).unwrap();
    assert!(matches!(
        store.status(),
        Err(KnowledgeError::InvalidState(_))
    ));
    std::fs::write(store.state_path(), b"not-json").unwrap();
    assert!(store.status().is_err());
}
#[cfg(unix)]
#[test]
fn private_permissions_and_tightening_own_existing_store() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, store) = fixture();
    let s = store.initialize(now(), None).unwrap();
    assert_eq!(
        std::fs::metadata(&s.store.root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(store.state_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    std::fs::set_permissions(&s.store.root, std::fs::Permissions::from_mode(0o755)).unwrap();
    store.initialize(now(), None).unwrap();
    assert_eq!(
        std::fs::metadata(&s.store.root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}
#[test]
fn reinstating_source_keeps_job_ids_unique() {
    let (_temp, store) = fixture();
    let s = store.initialize(now(), None).unwrap();
    let s = store.reconcile(&s.revision, &[source("1")], now()).unwrap();
    let original = s.jobs[0].id.clone();
    let s = store.reconcile(&s.revision, &[], now()).unwrap();
    let s = store.reconcile(&s.revision, &[source("1")], now()).unwrap();
    assert_eq!(s.jobs.len(), 1);
    assert_eq!(s.jobs[0].id, original);
    assert_eq!(s.jobs[0].status, JobStatus::BlockedOnDependency);
}

#[test]
fn initialization_binding_confirmation_survives_restart() {
    let (_temp, store) = fixture();
    let s = store.initialize(now(), None).unwrap();
    assert!(!s.binding_confirmed);
    let s = store.confirm_binding(&s.revision).unwrap();
    assert!(s.binding_confirmed);
    assert!(store.status().unwrap().unwrap().binding_confirmed);
}
#[test]
fn inbox_recovers_after_state_outage_and_deduplicates_replay() {
    let (_temp, store) = fixture();
    store.initialize(now(), None).unwrap();
    let saved = std::fs::read(store.state_path()).unwrap();
    std::fs::write(store.state_path(), b"broken").unwrap();
    store
        .enqueue_event("slack:opaque", "1", "event-one", now(), now())
        .unwrap();
    assert!(
        store
            .replay_inbox(
                &store
                    .status()
                    .ok()
                    .flatten()
                    .map(|s| s.revision)
                    .unwrap_or_default(),
                &[source("1")],
                now()
            )
            .is_err()
    );
    std::fs::write(store.state_path(), saved).unwrap();
    let resumed = KnowledgeStore::new(store.home.clone());
    let s = resumed
        .replay_inbox(
            &store
                .status()
                .ok()
                .flatten()
                .map(|s| s.revision)
                .unwrap_or_default(),
            &[source("1")],
            now(),
        )
        .unwrap();
    assert_eq!(s.admitted_events.len(), 1);
    resumed
        .enqueue_event("slack:opaque", "1", "event-one", now(), now())
        .unwrap();
    assert_eq!(
        resumed
            .replay_inbox(
                &store
                    .status()
                    .ok()
                    .flatten()
                    .map(|s| s.revision)
                    .unwrap_or_default(),
                &[source("1")],
                now()
            )
            .unwrap()
            .jobs
            .len(),
        2
    );
    assert!(
        !std::fs::read_to_string(store.directory().join("inbox.json"))
            .unwrap()
            .contains("event-one")
    );
    assert!(!s.revision.is_empty());
}
#[test]
fn concurrent_inbox_admission_keeps_both_records_and_drops_revoked_binding() {
    let (_temp, store) = fixture();
    store.initialize(now(), None).unwrap();
    let handles: Vec<_> = (0..2)
        .map(|n| {
            let home = store.home.clone();
            std::thread::spawn(move || {
                let store = KnowledgeStore::new(home);
                store
                    .enqueue_event("slack:opaque", "1", &format!("event-{n}"), now(), now())
                    .unwrap();
                loop {
                    let revision = store.status().unwrap().unwrap().revision;
                    match store.replay_inbox(&revision, &[source("1")], now()) {
                        Ok(_) => break,
                        Err(KnowledgeError::Conflict) => continue,
                        Err(error) => panic!("{error}"),
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(store.status().unwrap().unwrap().admitted_events.len(), 2);
    store
        .enqueue_event("slack:opaque", "1", "revoked-event", now(), now())
        .unwrap();
    assert_eq!(
        store
            .replay_inbox(
                &store
                    .status()
                    .ok()
                    .flatten()
                    .map(|s| s.revision)
                    .unwrap_or_default(),
                &[source("2")],
                now()
            )
            .unwrap()
            .admitted_events
            .len(),
        2
    );
}
#[cfg(unix)]
#[test]
fn standard_assistant_home_can_initialize_without_moving_legacy_content() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, store) = fixture();
    store.home.ensure().unwrap();
    std::fs::set_permissions(store.home.okg_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let entity = store.home.okg_dir().join("existing.md");
    std::fs::write(&entity, "existing fixture entity").unwrap();
    let s = store.initialize(now(), None).unwrap();
    assert_eq!(
        std::fs::read_to_string(entity).unwrap(),
        "existing fixture entity"
    );
    assert_eq!(
        std::fs::metadata(s.store.root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}
#[test]
fn stale_inbox_replay_cannot_restore_detached_source() {
    let (_temp, store) = fixture();
    let s = store.initialize(now(), None).unwrap();
    let s = store.reconcile(&s.revision, &[source("1")], now()).unwrap();
    store
        .enqueue_event("slack:opaque", "1", "event", now(), now())
        .unwrap();
    let detached = store.reconcile(&s.revision, &[], now()).unwrap();
    assert!(matches!(
        store.replay_inbox(&s.revision, &[source("1")], now()),
        Err(KnowledgeError::Conflict)
    ));
    let state = store.status().unwrap().unwrap();
    assert_eq!(state, detached);
    assert!(state.sources.is_empty());
    assert!(
        store
            .directory()
            .join("inbox.json")
            .metadata()
            .unwrap()
            .len()
            > 2
    );
    assert!(
        store
            .replay_inbox(&state.revision, &[], now())
            .unwrap()
            .admitted_events
            .is_empty()
    );
}
#[test]
fn protected_entity_root_cannot_contain_control_state() {
    let (_temp, store) = fixture();
    let selected = ProtectedStore {
        root: store.home.path().join("stores"),
        index_id: "own-index".into(),
        protected: true,
    };
    assert!(store.initialize(now(), Some(selected)).is_err());
    assert!(!store.home.path().exists());
}
