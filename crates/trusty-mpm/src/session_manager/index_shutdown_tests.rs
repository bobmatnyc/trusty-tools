//! Tests for [`IndexShutdown`] (#10989): the shutdown parks and unloads over
//! the real socket clients, never sends a delete, and reports every failure
//! arm as an item that may still be loaded.
//!
//! Hermetic: both daemons are `uds_mock` stand-ins on temp sockets that
//! record every method they are asked.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{
    IndexShutdown, IndexShutdownReport, METHOD_PALACE_UNLOAD, METHOD_PARK, METHOD_RESOLVE,
    ShutdownState, ShutdownTarget,
};
use crate::uds_mock::{self, MockUdsDaemon, RpcError};

/// trusty-search's "not found" code (`search_rpc::CODE_NOT_FOUND`).
const NOT_FOUND: i64 = -32004;
/// trusty-search's conflict code (`-32009`).
const CONFLICT: i64 = -32009;

/// One mock daemon's answer to `(method, params)`.
pub(crate) type Answer = Arc<dyn Fn(&str, &Value) -> Result<Value, RpcError> + Send + Sync>;

/// Every `(daemon, method, params)` the rig's daemons were asked, in order.
pub(crate) type CallLog = Arc<Mutex<Vec<(String, String, Value)>>>;

/// Two recording mock daemons: trusty-search and trusty-memory.
pub(crate) struct Rig {
    search: MockUdsDaemon,
    memory: MockUdsDaemon,
    /// Every call either daemon was asked.
    pub(crate) calls: CallLog,
}

impl Rig {
    /// The capability against this rig's sockets.
    pub(crate) fn capability(&self) -> IndexShutdown {
        IndexShutdown::at(
            Some(self.search.socket().to_path_buf()),
            Some(self.memory.socket().to_path_buf()),
        )
    }

    /// The trusty-search stand-in's socket (#10989 restore tests).
    pub(crate) fn search_socket(&self) -> PathBuf {
        self.search.socket().to_path_buf()
    }

    /// The methods asked, as `daemon:method`.
    pub(crate) fn methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .map(|(d, m, _)| format!("{d}:{m}"))
            .collect()
    }

    /// The params of the first call to `method`.
    pub(crate) fn params_of(&self, method: &str) -> Option<Value> {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .find(|(_, m, _)| m == method)
            .map(|(_, _, p)| p.clone())
    }
}

async fn recording(daemon: &'static str, calls: &CallLog, answer: Answer) -> MockUdsDaemon {
    let calls = Arc::clone(calls);
    uds_mock::spawn(move |method, params| {
        calls
            .lock()
            .expect("calls")
            .push((daemon.to_string(), method.to_string(), params.clone()));
        let out = answer(method, &params);
        Box::pin(async move { out })
    })
    .await
}

/// A rig whose daemons answer through `search` and `memory`.
pub(crate) async fn rig(search: Answer, memory: Answer) -> Rig {
    let calls = Arc::new(Mutex::new(Vec::new()));
    Rig {
        search: recording("search", &calls, search).await,
        memory: recording("memory", &calls, memory).await,
        calls,
    }
}

/// A trusty-search that resolves to `widget-idx` and parks it unloaded.
pub(crate) fn healthy_search() -> Answer {
    Arc::new(|method, _| match method {
        METHOD_RESOLVE => Ok(json!({ "index_id": "widget-idx" })),
        METHOD_PARK => Ok(json!({ "index_id": "widget-idx", "parked": true, "unloaded": true })),
        other => Err(RpcError::new(-32601, format!("unexpected {other}"))),
    })
}

/// A trusty-memory that unloads any palace.
pub(crate) fn healthy_memory() -> Answer {
    Arc::new(|method, params| match method {
        METHOD_PALACE_UNLOAD => Ok(json!({
            "palace_id": params["palace_id"], "unloaded": true, "exists": true,
            "was_resident": true, "data_kept": true,
        })),
        other => Err(RpcError::new(-32601, format!("unexpected {other}"))),
    })
}

/// A trusty-search whose park is refused because a reindex runs.
pub(crate) fn busy_search() -> Answer {
    Arc::new(|method, _| match method {
        METHOD_RESOLVE => Ok(json!({ "index_id": "widget-idx" })),
        _ => Err(RpcError::new(
            CONFLICT,
            "widget-idx was not parked: a reindex is running",
        )),
    })
}

/// A trusty-memory whose unload is refused: the palace is busy.
pub(crate) fn busy_memory() -> Answer {
    Arc::new(|_, _| Err(RpcError::new(-32006, "palace \"acme-widget\" is busy")))
}

fn target() -> ShutdownTarget {
    ShutdownTarget {
        search_query: Some("acme/widget".into()),
        palace: Ok("acme-widget".into()),
    }
}

/// Why: acceptance 3 of #10989 — the shutdown parks with `decommission:
/// true` and unloads the palace, and no daemon is ever asked to delete.
/// Test: this test. Fails if the shutdown sends a delete, skips the park or
/// the unload, or parks without the decommission flag.
#[tokio::test]
async fn shutdown_parks_and_unloads_and_never_deletes() {
    let rig = rig(healthy_search(), healthy_memory()).await;
    let report = rig.capability().shut_down(&target()).await;

    assert_eq!(
        report.search,
        ShutdownState::Unloaded {
            id: "widget-idx".into()
        }
    );
    assert_eq!(
        report.palace,
        ShutdownState::Unloaded {
            id: "acme-widget".into()
        }
    );
    assert!(report.still_loaded().is_empty());
    let methods = rig.methods();
    assert_eq!(
        methods,
        vec![
            format!("search:{METHOD_RESOLVE}"),
            format!("search:{METHOD_PARK}"),
            format!("memory:{METHOD_PALACE_UNLOAD}"),
        ],
        "only resolve, park and unload are sent"
    );
    assert!(
        !methods.iter().any(|m| m.contains("delete")),
        "a delete was sent: {methods:?}"
    );
    assert_eq!(
        rig.params_of(METHOD_PARK),
        Some(json!({ "index_id": "widget-idx", "decommission": true }))
    );
    assert_eq!(
        rig.params_of(METHOD_PALACE_UNLOAD),
        Some(json!({ "palace_id": "acme-widget" }))
    );
}

/// One failure arm: the daemons' answers and the expected per-item verdict.
struct Arm {
    name: &'static str,
    search: Answer,
    memory: Answer,
    search_failed: bool,
    palace_failed: bool,
}

/// Why: Fail-Open Check (#10989) — every answer that does not confirm an
/// unload is a failed item listed by `still_loaded`; the answers that mean
/// "nothing was loaded" are not.
/// Test: this test, one row per arm of `park_search` and `unload_palace`.
/// Removing an arm (for instance treating `unloaded: false` as success)
/// fails its row.
#[tokio::test]
async fn every_unconfirmed_answer_is_still_loaded_and_a_miss_is_not() {
    let arms = vec![
        Arm {
            name: "park refused (reindex running)",
            search: busy_search(),
            memory: healthy_memory(),
            search_failed: true,
            palace_failed: false,
        },
        Arm {
            name: "park answered parked but not unloaded",
            search: Arc::new(|method, _| match method {
                METHOD_RESOLVE => Ok(json!({ "index_id": "widget-idx" })),
                _ => Ok(json!({ "parked": true, "unloaded": false })),
            }),
            memory: healthy_memory(),
            search_failed: true,
            palace_failed: false,
        },
        Arm {
            name: "resolve answered no index_id",
            search: Arc::new(|_, _| Ok(json!({}))),
            memory: healthy_memory(),
            search_failed: true,
            palace_failed: false,
        },
        Arm {
            name: "resolve failed with another error",
            search: Arc::new(|_, _| Err(RpcError::new(-32603, "registry unreadable"))),
            memory: healthy_memory(),
            search_failed: true,
            palace_failed: false,
        },
        Arm {
            name: "resolve miss: no live index",
            search: Arc::new(|_, _| Err(RpcError::new(NOT_FOUND, "no live index"))),
            memory: healthy_memory(),
            search_failed: false,
            palace_failed: false,
        },
        Arm {
            name: "unload refused (busy palace)",
            search: healthy_search(),
            memory: busy_memory(),
            search_failed: false,
            palace_failed: true,
        },
        Arm {
            name: "memory daemon predates palace_unload",
            search: healthy_search(),
            memory: Arc::new(|m, _| Err(RpcError::new(-32601, format!("unknown method {m}")))),
            search_failed: false,
            palace_failed: true,
        },
        Arm {
            name: "unload answered not unloaded",
            search: healthy_search(),
            memory: Arc::new(|_, _| Ok(json!({ "unloaded": false }))),
            search_failed: false,
            palace_failed: true,
        },
    ];
    for arm in arms {
        let rig = rig(arm.search, arm.memory).await;
        let report = rig.capability().shut_down(&target()).await;
        let failed = |s: &ShutdownState| matches!(s, ShutdownState::Failed { .. });
        assert_eq!(
            failed(&report.search),
            arm.search_failed,
            "{}: {report:?}",
            arm.name
        );
        assert_eq!(
            failed(&report.palace),
            arm.palace_failed,
            "{}: {report:?}",
            arm.name
        );
        let expected = usize::from(arm.search_failed) + usize::from(arm.palace_failed);
        assert_eq!(
            report.still_loaded().len(),
            expected,
            "{}: {report:?}",
            arm.name
        );
    }
}

/// Why: a daemon that is not running holds nothing, so it is not a failure;
/// an unresolvable target with a daemon up is one.
/// Test: this test.
#[tokio::test]
async fn a_daemon_not_running_holds_nothing_and_an_unresolved_target_fails() {
    let gone = IndexShutdown::at(Some(PathBuf::from("/nonexistent/trusty-search.sock")), None);
    let report = gone.shut_down(&target()).await;
    assert!(
        matches!(report.search, ShutdownState::NotLoaded { .. }),
        "{report:?}"
    );
    assert!(
        matches!(report.palace, ShutdownState::NotLoaded { .. }),
        "{report:?}"
    );

    let rig = rig(healthy_search(), healthy_memory()).await;
    let unresolved = ShutdownTarget {
        search_query: None,
        palace: Err("could not resolve the palace: bad pin".into()),
    };
    let report = rig.capability().shut_down(&unresolved).await;
    assert_eq!(report.still_loaded().len(), 2, "{report:?}");
    assert!(
        rig.methods().is_empty(),
        "nothing is asked without a target"
    );
}

/// Why: the decommission error names each item that may still be loaded.
/// Test: this test.
#[test]
fn a_failed_park_is_listed_as_still_loaded() {
    let report = IndexShutdownReport {
        search: ShutdownState::Failed {
            id: Some("widget-idx".into()),
            error: "a reindex is running".into(),
        },
        palace: ShutdownState::Unloaded {
            id: "acme-widget".into(),
        },
    };
    assert_eq!(
        report.still_loaded(),
        vec!["search index widget-idx: a reindex is running".to_string()]
    );
    assert!(
        IndexShutdownReport::skipped("test")
            .still_loaded()
            .is_empty()
    );
}

/// Why: the search query is the repo identity tm registered, so the resolve
/// reaches the project's own group; a path is the last resort.
/// Test: this test.
#[test]
fn target_prefers_the_project_key_then_the_remote_then_the_path() {
    let root = std::path::Path::new("/nonexistent/widget");
    let by_key =
        ShutdownTarget::for_project(Some("acme/widget"), "https://github.com/other/name", None);
    assert_eq!(by_key.search_query.as_deref(), Some("acme/widget"));
    let by_remote = ShutdownTarget::for_project(
        Some("local:1234"),
        "https://github.com/Acme/Widget.git",
        None,
    );
    assert_eq!(by_remote.search_query.as_deref(), Some("acme/widget"));
    let by_path = ShutdownTarget::for_project(None, "", Some(root));
    assert_eq!(by_path.search_query.as_deref(), Some("/nonexistent/widget"));
    assert!(by_key.palace.is_err(), "no checkout, no palace");
}
