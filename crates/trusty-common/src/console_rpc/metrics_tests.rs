//! Serde tests for the console socket's metrics vocabulary (#10942, #10943).
//!
//! Why: the TUI and the console decode these payloads from two crates; a
//! renamed key or a changed spelling must fail here, not as a blank card.
//! What: golden JSON for the not-ready and history answers, round-trips, the
//! request types' accept/refuse rules, and forward-compatible decoding.
//! Test: this module IS the suite for `console_rpc/metrics.rs`.

use serde_json::json;

use super::*;
use crate::console_metrics::make_report;
use crate::host_metrics::HostSampler;

fn sample(id: &str, cpu: Option<f32>, rss: Option<u64>) -> ServiceSample {
    ServiceSample {
        id: id.to_string(),
        status: ServiceStatus::Running,
        cpu_pct: cpu,
        rss_bytes: rss,
    }
}

/// Why: a client may send no params, `null`, `{}` or `[]`; all mean "none".
/// What: each request type encodes as `{}` and decodes every accepted form.
/// Test: this test itself.
#[test]
fn request_types_accept_absent_null_or_an_object() {
    assert_eq!(
        serde_json::to_value(MachineStatusRequest::default()).expect("encode"),
        json!({})
    );
    assert_eq!(
        serde_json::to_value(HistoryRequest::default()).expect("encode"),
        json!({})
    );
    for params in [json!(null), json!({}), json!({"later": 1}), json!([])] {
        let m: MachineStatusRequest = serde_json::from_value(params.clone()).expect("decode m");
        assert_eq!(m, MachineStatusRequest {});
        let h: HistoryRequest = serde_json::from_value(params).expect("decode h");
        assert_eq!(h, HistoryRequest {});
    }
}

/// Why: a malformed request must reach the router as a decode failure, which
/// it answers with `invalid_params`, instead of being silently accepted.
/// What: a string, a number, a bool and a non-empty array are all refused.
/// Test: this test itself.
#[test]
fn request_types_refuse_a_scalar_or_a_non_empty_array() {
    for params in [json!("x"), json!(3), json!(true), json!([1])] {
        assert!(
            serde_json::from_value::<MachineStatusRequest>(params.clone()).is_err(),
            "machine_status params {params} must be refused"
        );
        assert!(
            serde_json::from_value::<HistoryRequest>(params.clone()).is_err(),
            "history params {params} must be refused"
        );
    }
}

/// Why: "no samples yet" is the cold-start answer every TUI sees first.
/// What: the default answer encodes as `{"ready":false,"machine":null}`, and
/// an empty object decodes to it.
/// Test: this test itself.
#[test]
fn machine_status_not_ready_golden_json() {
    let value = serde_json::to_value(ConsoleMachineStatus::default()).expect("encode");
    assert_eq!(value, json!({"ready": false, "machine": null}));
    let back: ConsoleMachineStatus = serde_json::from_value(json!({})).expect("decode");
    assert!(!back.ready && back.machine.is_none());
}

/// Why: the ready answer embeds the whole `MachineStatus`; it must survive the
/// wire unchanged.
/// What: assembles a status from a real host sample and one report, encodes
/// it, decodes it, and compares the two encodings.
/// Test: this test itself.
#[test]
fn machine_status_ready_round_trips() {
    let report = make_report(
        "trusty-search",
        "Search",
        "1.0.0",
        ServiceHealth::Ok,
        json!({}),
        1,
    );
    let answer = ConsoleMachineStatus {
        ready: true,
        machine: Some(MachineStatus::assemble(
            HostSampler::new().sample(),
            &[report],
        )),
    };
    let value = serde_json::to_value(&answer).expect("encode");
    let back: ConsoleMachineStatus = serde_json::from_value(value.clone()).expect("decode");
    assert!(back.ready);
    assert_eq!(back.machine.as_ref().expect("machine").services.total, 1);
    assert_eq!(serde_json::to_value(&back).expect("re-encode"), value);
}

/// Why: the TUI draws from these exact keys; #10943's per-service rings ride
/// on `service_samples`.
/// What: pins the JSON of a snapshot with one service sample and one
/// transition, then round-trips it.
/// Test: this test itself.
#[test]
fn history_snapshot_golden_json() {
    let mut service_samples = BTreeMap::new();
    service_samples.insert(
        "trusty-search".to_string(),
        vec![sample("trusty-search", Some(12.5), Some(4096))],
    );
    let snapshot = HistorySnapshot {
        samples: Vec::new(),
        transitions: vec![ServiceTransition {
            service_id: "trusty-search".into(),
            display_name: "Trusty Search".into(),
            from: ServiceState::Up,
            to: ServiceState::Down,
            at_unix: 1_700_000_000,
        }],
        service_samples,
        sample_capacity: 600,
        service_sample_capacity: 600,
        transition_capacity: 256,
        sample_interval_secs: 1,
        sse_client_count: 0,
        schema_version: MACHINE_HISTORY_SCHEMA_VERSION,
    };
    let value = serde_json::to_value(&snapshot).expect("encode");
    assert_eq!(
        value,
        json!({
            "samples": [],
            "transitions": [{
                "service_id": "trusty-search",
                "display_name": "Trusty Search",
                "from": "up",
                "to": "down",
                "at_unix": 1_700_000_000u64,
            }],
            "service_samples": {"trusty-search": [{
                "id": "trusty-search",
                "status": "running",
                "cpu_pct": 12.5,
                "rss_bytes": 4096,
            }]},
            "sample_capacity": 600,
            "service_sample_capacity": 600,
            "transition_capacity": 256,
            "sample_interval_secs": 1,
            "sse_client_count": 0,
            "schema_version": 4,
        })
    );
    let back: HistorySnapshot = serde_json::from_value(value.clone()).expect("decode");
    assert_eq!(serde_json::to_value(&back).expect("re-encode"), value);
}

/// Why: a newer TUI must read an older console, and an older TUI a newer one.
/// What: `{}` plus an unknown key decodes to an empty window.
/// Test: this test itself.
#[test]
fn history_snapshot_decodes_a_payload_missing_fields() {
    let back: HistorySnapshot =
        serde_json::from_value(json!({"added_later": true})).expect("decode");
    assert!(back.samples.is_empty() && back.service_samples.is_empty());
    assert_eq!(back.schema_version, 0);
}

/// Why: `null` means "not measured"; `0` would draw an idle service.
/// What: both figure keys are present and `null` for an unmeasured sample, and
/// a pre-#6773 row without `rss_bytes` still decodes.
/// Test: this test itself.
#[test]
fn service_sample_serialises_absent_figures_as_null() {
    let value = serde_json::to_value(sample("trusty-review", None, None)).expect("encode");
    assert!(value.get("cpu_pct").is_some_and(|v| v.is_null()));
    assert!(value.get("rss_bytes").is_some_and(|v| v.is_null()));
    let old: ServiceSample =
        serde_json::from_value(json!({"id": "a", "status": "absent", "cpu_pct": 1.0}))
            .expect("decode a pre-#6773 row");
    assert_eq!(old.rss_bytes, None);
}

/// Why: the transition log's states derive from self-reported health.
/// What: checks each mapping and the lowercase spelling.
/// Test: this test itself.
#[test]
fn service_state_maps_from_self_reported_health() {
    assert_eq!(ServiceState::from(&ServiceHealth::Ok), ServiceState::Up);
    assert_eq!(
        ServiceState::from(&ServiceHealth::Degraded),
        ServiceState::Degraded
    );
    assert_eq!(
        ServiceState::from(&ServiceHealth::Error),
        ServiceState::Down
    );
    assert_eq!(
        serde_json::to_value(ServiceState::Unknown).expect("encode"),
        json!("unknown")
    );
}
