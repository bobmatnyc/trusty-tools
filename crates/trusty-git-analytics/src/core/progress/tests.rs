//! Unit coverage for the #5197 progress bus and its aggregate fold.

use super::*;

fn ev(stage: Stage, target: &str) -> ProgressEvent {
    ProgressEvent::started(stage, target, None)
}

// ---------------------------------------------------------------- events

#[test]
fn stage_label_is_stable() {
    assert_eq!(Stage::Collect.label(), "Collect");
    assert_eq!(Stage::Correlate.label(), "Correlate");
    assert_eq!(Stage::Classify.label(), "Classify");
    assert_eq!(Stage::Audit.label(), "Audit");
    // #5361: `all()` is the pipeline skeleton, so the audit wrapper stays out
    // of it — a `tga tui` run would otherwise show eight rows that never fill.
    assert_eq!(Stage::all().len(), 3);
    assert!(!Stage::all().contains(&Stage::Audit));
}

#[test]
fn outcome_label_is_stable() {
    assert_eq!(Outcome::Completed.label(), "ok");
    assert_eq!(Outcome::Completed.reason(), None);
    let f = Outcome::Failed {
        reason: "boom".into(),
    };
    assert_eq!(f.label(), "failed");
    assert_eq!(f.reason(), Some("boom"));
    let s = Outcome::Skipped {
        reason: "cached".into(),
    };
    assert_eq!(s.label(), "skipped");
    assert_eq!(s.reason(), Some("cached"));
}

#[test]
fn event_constructors_set_expected_fields() {
    let started = ProgressEvent::started(Stage::Collect, "api", Some(10));
    assert_eq!(started.done, 0);
    assert_eq!(started.total, Some(10));
    assert!(started.outcome.is_none());

    let advanced = ProgressEvent::advanced(Stage::Collect, "api", 4, Some(10));
    assert_eq!(advanced.done, 4);
    assert!(advanced.outcome.is_none());

    let completed = ProgressEvent::completed(Stage::Collect, "api", 10);
    assert_eq!(completed.done, 10);
    assert_eq!(completed.outcome, Some(Outcome::Completed));

    let failed = ProgressEvent::failed(Stage::Collect, "api", "no remote");
    assert_eq!(
        failed.outcome,
        Some(Outcome::Failed {
            reason: "no remote".into()
        })
    );
    assert_eq!(failed.detail.as_deref(), Some("no remote"));

    let skipped = ProgressEvent::skipped(Stage::Correlate, "api", "already linked");
    assert!(matches!(skipped.outcome, Some(Outcome::Skipped { .. })));

    let detailed = ProgressEvent::advanced(Stage::Classify, "batch", 1, None).with_detail("tier 2");
    assert_eq!(detailed.detail.as_deref(), Some("tier 2"));
}

#[test]
fn event_is_terminal_only_when_outcome_present() {
    assert!(!ProgressEvent::started(Stage::Collect, "a", None).is_terminal());
    assert!(!ProgressEvent::advanced(Stage::Collect, "a", 1, None).is_terminal());
    assert!(ProgressEvent::completed(Stage::Collect, "a", 1).is_terminal());
    assert!(ProgressEvent::failed(Stage::Collect, "a", "x").is_terminal());
    assert!(ProgressEvent::skipped(Stage::Collect, "a", "x").is_terminal());
}

// ------------------------------------------------------------------- bus

/// The no-subscriber contract: emitting on a disabled bus must be inert.
///
/// This is the regression guard for "existing CLI behavior is byte-identical
/// with no subscriber attached" — every non-TUI call site passes
/// `ProgressBus::disabled()`.
#[test]
fn disabled_bus_swallows_every_emit() {
    let bus = ProgressBus::disabled();
    assert!(!bus.is_active());
    for i in 0..10_000u64 {
        bus.emit(ProgressEvent::advanced(Stage::Collect, "api", i, None));
    }
    assert!(bus.drain().is_empty());
    assert_eq!(bus.queued(), 0);
    assert_eq!(bus.dropped(), 0);
    // Default is the disabled bus.
    assert!(!ProgressBus::default().is_active());
}

#[test]
fn default_capacity_is_used_by_new() {
    let bus = ProgressBus::new();
    assert!(bus.is_active());
    for i in 0..DEFAULT_CAPACITY {
        bus.emit(ev(Stage::Collect, &format!("r{i}")));
    }
    assert_eq!(bus.dropped(), 0, "no drop before capacity is exceeded");
    assert_eq!(bus.queued(), DEFAULT_CAPACITY);
}

/// Overflow policy: a full ring evicts the OLDEST event, and counts it.
#[test]
fn overflow_drops_oldest_and_counts() {
    let bus = ProgressBus::bounded(3);
    for i in 0..5u64 {
        bus.emit(ProgressEvent::advanced(Stage::Collect, "api", i, None));
    }
    assert_eq!(bus.dropped(), 2, "two oldest evicted");
    let events = bus.drain();
    assert_eq!(events.len(), 3);
    // The three most recent survived, in order.
    assert_eq!(
        events.iter().map(|e| e.done).collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
}

/// A producer must not stall when the consumer never drains. 100k emits into a
/// 16-slot ring must complete and leave exactly 16 events queued.
#[test]
fn absent_consumer_never_stalls_producer() {
    let bus = ProgressBus::bounded(16);
    for i in 0..100_000u64 {
        bus.emit(ProgressEvent::advanced(Stage::Collect, "api", i, None));
    }
    assert_eq!(bus.queued(), 16);
    assert_eq!(bus.dropped(), 100_000 - 16);
}

#[test]
fn zero_capacity_is_raised_to_one() {
    let bus = ProgressBus::bounded(0);
    bus.emit(ProgressEvent::completed(Stage::Collect, "a", 1));
    bus.emit(ProgressEvent::completed(Stage::Collect, "b", 1));
    let events = bus.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].target, "b", "newest wins");
}

#[test]
fn drain_returns_fifo_and_empties() {
    let bus = ProgressBus::bounded(8);
    bus.emit(ev(Stage::Collect, "a"));
    bus.emit(ev(Stage::Collect, "b"));
    let first = bus.drain();
    assert_eq!(
        first.iter().map(|e| e.target.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(bus.queued(), 0);
    assert!(bus.drain().is_empty(), "second drain sees nothing");
}

#[test]
fn clone_shares_one_queue() {
    let producer = ProgressBus::bounded(8);
    let consumer = producer.clone();
    producer.emit(ev(Stage::Collect, "a"));
    assert_eq!(consumer.drain().len(), 1);
}

#[test]
fn concurrent_producers_lose_nothing_within_capacity() {
    let bus = ProgressBus::bounded(512);
    std::thread::scope(|s| {
        for t in 0..4 {
            let b = bus.clone();
            s.spawn(move || {
                for i in 0..100u64 {
                    b.emit(ProgressEvent::advanced(
                        Stage::Collect,
                        format!("t{t}"),
                        i,
                        None,
                    ));
                }
            });
        }
    });
    assert_eq!(bus.dropped(), 0);
    assert_eq!(bus.drain().len(), 400);
}

// ------------------------------------------------------------- aggregate

#[test]
fn aggregate_starts_empty() {
    let agg = ProgressAggregate::new();
    assert!(agg.is_empty());
    assert!(!agg.is_settled());
    assert!(agg.rows(Stage::Collect).is_empty());
    assert_eq!(agg.summary(Stage::Collect), StageSummary::default());
    assert!(!agg.summary(Stage::Collect).is_started());
}

/// #5361: a renderer driven by `Stage::all()` can never show audit-sweep rows,
/// so the aggregate must be able to report what actually emitted.
#[test]
fn aggregate_lists_only_stages_that_produced_rows() {
    let mut agg = ProgressAggregate::new();
    assert_eq!(agg.stages().count(), 0);

    agg.apply(ProgressEvent::started(Stage::Audit, "jira sync", Some(1)));
    agg.apply(ProgressEvent::completed(Stage::Collect, "api", 1));

    // Ascending `Stage` order, and nothing for the stage that never emitted.
    assert_eq!(
        agg.stages().collect::<Vec<_>>(),
        vec![Stage::Collect, Stage::Audit]
    );
    assert!(!agg.stages().any(|s| s == Stage::Classify));
}

#[test]
fn aggregate_tracks_per_target_rows() {
    let mut agg = ProgressAggregate::new();
    agg.apply(ProgressEvent::started(Stage::Collect, "api", Some(4)));
    agg.apply(ProgressEvent::started(Stage::Collect, "web", Some(2)));
    agg.apply(ProgressEvent::advanced(Stage::Collect, "api", 3, Some(4)));

    let rows = agg.rows(Stage::Collect);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].target, "api", "first-seen order preserved");
    assert_eq!(rows[0].done, 3);
    assert!(rows[0].is_running());

    agg.apply(ProgressEvent::completed(Stage::Collect, "api", 4));
    assert!(!agg.rows(Stage::Collect)[0].is_running());
    assert_eq!(
        agg.rows(Stage::Collect)[0].outcome,
        Some(Outcome::Completed)
    );
}

#[test]
fn aggregate_advance_keeps_known_total() {
    let mut agg = ProgressAggregate::new();
    agg.apply(ProgressEvent::started(Stage::Collect, "api", Some(9)));
    // A later event with no total must not erase the one already learned.
    agg.apply(ProgressEvent::advanced(Stage::Collect, "api", 5, None));
    assert_eq!(agg.rows(Stage::Collect)[0].total, Some(9));
    assert_eq!(agg.rows(Stage::Collect)[0].done, 5);
}

#[test]
fn aggregate_done_never_goes_backwards() {
    let mut agg = ProgressAggregate::new();
    agg.apply(ProgressEvent::advanced(Stage::Collect, "api", 7, Some(9)));
    // Out-of-order delivery must not rewind the bar.
    agg.apply(ProgressEvent::advanced(Stage::Collect, "api", 2, Some(9)));
    assert_eq!(agg.rows(Stage::Collect)[0].done, 7);
}

#[test]
fn aggregate_rolls_up_per_stage() {
    let mut agg = ProgressAggregate::new();
    agg.apply(ProgressEvent::completed(Stage::Collect, "a", 1));
    agg.apply(ProgressEvent::failed(Stage::Collect, "b", "no remote"));
    agg.apply(ProgressEvent::skipped(Stage::Collect, "c", "cached"));
    agg.apply(ProgressEvent::started(Stage::Collect, "d", None));

    let s = agg.summary(Stage::Collect);
    assert_eq!(s.completed, 1);
    assert_eq!(s.failed, 1);
    assert_eq!(s.skipped, 1);
    assert_eq!(s.running, 1);
    assert_eq!(s.total(), 4);
    assert!(s.is_started());
    // Stages are independent.
    assert_eq!(agg.summary(Stage::Classify).total(), 0);
}

#[test]
fn aggregate_is_settled() {
    let mut agg = ProgressAggregate::new();
    agg.apply(ProgressEvent::started(Stage::Collect, "a", None));
    assert!(!agg.is_settled());
    agg.apply(ProgressEvent::completed(Stage::Collect, "a", 1));
    assert!(agg.is_settled());
    agg.apply(ProgressEvent::started(Stage::Correlate, "link", None));
    assert!(!agg.is_settled(), "a new running stage un-settles it");
}

#[test]
fn aggregate_log_is_bounded() {
    let mut agg = ProgressAggregate::new();
    for i in 0..(LOG_CAPACITY + 50) {
        agg.apply(ProgressEvent::completed(Stage::Collect, format!("r{i}"), 1));
    }
    assert_eq!(agg.log().count(), LOG_CAPACITY);
    let last = agg.log().next_back().cloned().unwrap_or_default();
    assert!(last.contains(&format!("r{}", LOG_CAPACITY + 49)));
}

/// #5197: the TUI pushes its diverted `tracing` lines onto the same log, and
/// they obey the same bound as the event-derived ones.
#[test]
fn aggregate_accepts_external_activity_lines() {
    let mut agg = ProgressAggregate::new();
    agg.push_activity("2026-08-08T16:53:27Z  WARN fetch failed".to_string());
    assert_eq!(
        agg.log().next_back().map(String::as_str),
        Some("2026-08-08T16:53:27Z  WARN fetch failed")
    );
    for i in 0..LOG_CAPACITY {
        agg.push_activity(format!("line {i}"));
    }
    assert_eq!(agg.log().count(), LOG_CAPACITY);
}

#[test]
fn activity_line_only_for_notable_events() {
    let mut agg = ProgressAggregate::new();
    // Bare counter ticks do not earn a log line.
    agg.apply(ProgressEvent::advanced(Stage::Collect, "api", 1, Some(9)));
    assert_eq!(agg.log().count(), 0);
    // A detail does.
    agg.apply(ProgressEvent::advanced(Stage::Collect, "api", 2, Some(9)).with_detail("week 3"));
    assert_eq!(agg.log().count(), 1);
    // So does a terminal event.
    agg.apply(ProgressEvent::failed(Stage::Collect, "api", "auth"));
    assert_eq!(agg.log().count(), 2);
    let lines: Vec<&String> = agg.log().collect();
    assert!(lines[1].contains("failed"));
    assert!(lines[1].contains("auth"));
}

#[test]
fn aggregate_records_dropped() {
    let mut agg = ProgressAggregate::new();
    assert_eq!(agg.dropped(), 0);
    agg.set_dropped(17);
    assert_eq!(agg.dropped(), 17);
}

#[test]
fn target_row_fraction() {
    let mut agg = ProgressAggregate::new();
    agg.apply(ProgressEvent::advanced(Stage::Collect, "api", 5, Some(10)));
    assert_eq!(agg.rows(Stage::Collect)[0].fraction(), Some(0.5));

    // Unknown total while running → no fraction (spinner, not a bar).
    agg.apply(ProgressEvent::started(Stage::Collect, "web", None));
    assert_eq!(agg.rows(Stage::Collect)[1].fraction(), None);

    // Unknown total but finished → full.
    agg.apply(ProgressEvent::failed(Stage::Collect, "web", "x"));
    assert_eq!(agg.rows(Stage::Collect)[1].fraction(), Some(1.0));

    // Overshoot clamps.
    agg.apply(ProgressEvent::advanced(Stage::Collect, "api", 99, Some(10)));
    assert_eq!(agg.rows(Stage::Collect)[0].fraction(), Some(1.0));
}

/// End-to-end: bus → drain → aggregate, the exact loop the TUI runs per tick.
#[test]
fn bus_drain_folds_into_aggregate() {
    let bus = ProgressBus::bounded(64);
    bus.emit(ProgressEvent::started(Stage::Collect, "api", Some(2)));
    bus.emit(ProgressEvent::advanced(Stage::Collect, "api", 1, Some(2)));
    bus.emit(ProgressEvent::completed(Stage::Collect, "api", 2));

    let mut agg = ProgressAggregate::new();
    for e in bus.drain() {
        agg.apply(e);
    }
    agg.set_dropped(bus.dropped());

    assert!(agg.is_settled());
    assert_eq!(agg.summary(Stage::Collect).completed, 1);
    assert_eq!(agg.rows(Stage::Collect)[0].fraction(), Some(1.0));
}
