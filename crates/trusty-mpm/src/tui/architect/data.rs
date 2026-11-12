//! The Architect dashboard lists, read over the mpm daemon socket.
//!
//! Why (#10966, goal ar-G01): the Architect view replicates the dashboard's
//! lists and badges. `tm tui` is UDS-only (#10899), and the dashboard store
//! is served as `mpm.architect.dashboard.*` on the daemon socket.
//! What: [`fetch_dashboard`] sends `list` for `items`, `goals`, `sessions`
//! and `questions` plus `tally`. Those two verbs are the only methods this
//! module names, which is the read-only guarantee (Dc4). [`snapshot_from`]
//! (pure) turns the documents into one [`DashRow`] list per tab. The store
//! root is passed in (`tm tui --architect-store`) until the daemon resolves
//! it itself (#10944).
//! Test: `snapshot_splits_items_by_kind_and_drops_closed_rows`.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use trusty_architect::dashboard::{Tally, tally_line};

use super::Tab;
use crate::tui::theme::Tone;

/// What the dashboard tabs say when no store root was given (#10944).
pub const STORE_NOT_SET: &str = "Architect store not set: start with `tm tui --architect-store <dir>` \
     or TM_ARCHITECT_STORE (the daemon resolves it itself once #10944 lands)";
/// The deadline on one dashboard call.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest detail value shown before it is cut.
const DETAIL_MAX: usize = 160;

/// One list row on a dashboard tab.
#[derive(Debug, Clone, PartialEq)]
pub struct DashRow {
    /// The document id (`D3a`, `G01`, …).
    pub id: String,
    /// Title, question or session name.
    pub title: String,
    /// The status word the badge shows.
    pub status: String,
    /// The badge tone.
    pub tone: Tone,
    /// One short context column: goal, session, summary.
    pub note: String,
    /// Scalar fields for the detail pane, in document order.
    pub fields: Vec<(String, String)>,
}

/// Every dashboard tab's rows plus the tally line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DashSnapshot {
    /// Open and answered items of kind `question` (decisions).
    pub decisions: Vec<DashRow>,
    /// Open and answered items of kind `action`.
    pub actions: Vec<DashRow>,
    /// Active and parked goals.
    pub goals: Vec<DashRow>,
    /// Every session document.
    pub sessions: Vec<DashRow>,
    /// Open and answered items of kind `task`.
    pub tasks: Vec<DashRow>,
    /// Q&A documents not yet cleared.
    pub qa: Vec<DashRow>,
    /// `tally_line` of the store's `tally`, the count path the footer uses.
    pub tally: String,
}

impl DashSnapshot {
    /// The rows `tab` lists; the Live tab reads the tracker, not the store.
    // #11115: crate-internal, as `Tab` is
    pub(crate) fn rows(&self, tab: Tab) -> &[DashRow] {
        match tab {
            Tab::Decisions => &self.decisions,
            Tab::Actions => &self.actions,
            Tab::Goals => &self.goals,
            Tab::Sessions => &self.sessions,
            Tab::Tasks => &self.tasks,
            Tab::Qa => &self.qa,
            Tab::Live => &[],
        }
    }
}

fn s<'a>(data: &'a Value, key: &str) -> &'a str {
    data.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn status_tone(status: &str) -> Tone {
    match status {
        "open" | "pending" | "parked" => Tone::Warn,
        "answered" | "active" | "working" => Tone::Ok,
        "idle" => Tone::Info,
        "blocked" | "failed" => Tone::Err,
        _ => Tone::Muted,
    }
}

/// Scalar top-level fields, `key: value`, long values cut.
fn fields(data: &Value) -> Vec<(String, String)> {
    let Some(map) = data.as_object() else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(k, v)| {
            let text = match v {
                Value::String(t) if !t.is_empty() => t.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => return None,
            };
            let mut text: String = text.replace('\n', " ");
            if text.chars().count() > DETAIL_MAX {
                text = text.chars().take(DETAIL_MAX).collect::<String>() + "…";
            }
            Some((k.clone(), text))
        })
        .collect()
}

fn row(doc: &Value, title: &str, status: &str, note: String) -> DashRow {
    let data = doc.get("data").unwrap_or(&Value::Null);
    DashRow {
        id: s(doc, "id").to_string(),
        title: title.to_string(),
        status: status.to_string(),
        tone: status_tone(status),
        note,
        fields: fields(data),
    }
}

/// Turn the four `list` replies and the `tally` reply into tab rows.
///
/// Why: the network half is glue; this is the part with rules to test.
/// What: items split by `kind` (question → Decisions, action → Actions,
/// task → Tasks) keeping `open` and `answered`; goals keep `active` and
/// `parked`; questions drop `cleared`; sessions are all kept.
/// Test: `snapshot_splits_items_by_kind_and_drops_closed_rows`.
pub fn snapshot_from(
    items: &[Value],
    goals: &[Value],
    sessions: &[Value],
    questions: &[Value],
    tally: &Value,
) -> DashSnapshot {
    let mut snap = DashSnapshot::default();
    for doc in items {
        let d = doc.get("data").unwrap_or(&Value::Null);
        let status = s(d, "status");
        if !matches!(status, "open" | "answered") {
            continue;
        }
        let note = [s(d, "goal"), s(d, "session")]
            .into_iter()
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let r = row(doc, s(d, "title"), status, note);
        match s(d, "kind") {
            "question" => snap.decisions.push(r),
            "action" => snap.actions.push(r),
            "task" => snap.tasks.push(r),
            _ => {}
        }
    }
    for doc in goals {
        let d = doc.get("data").unwrap_or(&Value::Null);
        let life = s(d, "lifecycle");
        if matches!(life, "active" | "parked") {
            snap.goals
                .push(row(doc, s(d, "title"), life, s(d, "status_line").into()));
        }
    }
    for doc in sessions {
        let d = doc.get("data").unwrap_or(&Value::Null);
        snap.sessions.push(row(
            doc,
            s(d, "name"),
            s(d, "state"),
            s(d, "summary").into(),
        ));
    }
    for doc in questions {
        let d = doc.get("data").unwrap_or(&Value::Null);
        let status = s(d, "status");
        if status != "cleared" {
            snap.qa
                .push(row(doc, s(d, "asked"), status, s(d, "session").into()));
        }
    }
    snap.tally = tally_line(&tally_of(tally));
    snap
}

fn tally_of(v: &Value) -> Tally {
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
    Tally {
        open: n("open"),
        questions: n("questions"),
        actions: n("actions"),
        tasks: n("tasks"),
        answered_unrelayed: n("answered_unrelayed"),
        parked: n("parked"),
        running: n("running"),
        answers_waiting: v
            .get("answers_waiting")
            .and_then(Value::as_u64)
            .map(|w| w as usize),
    }
}

/// One JSON-RPC call over the daemon socket; `result` or the error text.
async fn call(socket: &Path, method: &str, params: Value) -> Result<Value, String> {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let reply: Value = trusty_common::uds::send_framed_request(socket, &frame, CALL_TIMEOUT)
        .await
        .map_err(|e| format!("{method}: {e}"))?;
    if let Some(result) = reply.get("result") {
        return Ok(result.clone());
    }
    let message = reply
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("no result and no error");
    Err(format!("{method}: {message}"))
}

/// Read every tab's rows and the tally, or say why it cannot.
///
/// Why: the home loop hands over what it has; the reasons for no data
/// (no store root, no daemon socket) belong with the fetch.
/// What: no `root` → [`STORE_NOT_SET`]; no `socket` → UDS-only refusal
/// (#10899); else four `list` calls and one `tally`.
/// Test: `fetch_dashboard_says_why_without_store_or_socket`; the socket
/// round trip runs live (`tm tui --architect-store`).
pub async fn fetch_dashboard(
    socket: Option<&Path>,
    root: Option<&Path>,
) -> Result<DashSnapshot, String> {
    let Some(root) = root else {
        return Err(STORE_NOT_SET.to_string());
    };
    let Some(socket) = socket else {
        return Err("the Architect view reads the daemon socket only (#10899)".into());
    };
    let store = json!({"root": root, "format": "dir"});
    let list = |collection: &str| {
        let params = json!({"store": store, "collection": collection, "order_by": "rank"});
        async move {
            let v = call(socket, "mpm.architect.dashboard.list", params).await?;
            Ok::<_, String>(v.as_array().cloned().unwrap_or_default())
        }
    };
    let items = list("items").await?;
    let goals = list("goals").await?;
    let sessions = list("sessions").await?;
    let questions = list("questions").await?;
    let tally = call(
        socket,
        "mpm.architect.dashboard.tally",
        json!({"store": store}),
    )
    .await?;
    Ok(snapshot_from(&items, &goals, &sessions, &questions, &tally))
}
