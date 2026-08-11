//! `tm doctor` pinned-search-index resolution probe (issue #5045).
//!
//! Why: session launch pins a trusty-search index id into the project's
//! `.mcp.json` (`trusty-search serve --index <id>`) and registers that index
//! best-effort, swallowing every failure at `tracing::warn!`. Until #5091 the
//! shared helper handed the derived id back regardless, so the pin advanced
//! whether or not the index was ever created, while the existing `search`
//! probe — which asks the daemon whether it is healthy and whether the
//! *derived* id appears in `/indexes` — kept reporting fine. Measured on
//! 2026-08-07: 4 of 75 live worktrees had a registered index, and
//! `POST /indexes/<pinned-id>/search` returned
//! `404 Not Found: {"error":"unknown index"}` in a worktree whose `search`
//! check was green.
//!
//! #5091 stops NEW pins from advancing on an unconfirmed create, and this check
//! stays: a pin already written by an older `tm` is still on disk, and an index
//! that existed when the pin was written can be deleted, GC'd, or lost when the
//! daemon's registry is rebuilt. A pin is a claim about the daemon's state at
//! one past moment; only resolving it says whether it still holds.
//!
//! What: [`check_search_index_pin`] reads the id the session is ACTUALLY
//! pinned to out of `.mcp.json` and resolves it against the daemon with
//! `GET /indexes/{id}/status`. A 404 is `Fail` — the pin names an index that
//! does not exist, so every `search` call in that session 404s. This is
//! deliberately not a health question: asking the daemon "are you well?"
//! reproduces the blind spot rather than closing it.
//!
//! Test: the `tests` module below covers every [`PinState`] / [`PinProbe`]
//! verdict, the `.mcp.json` reader against real files, and the 404 → `Fail`
//! path over a real HTTP socket.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};

use crate::daemon::discover::{TRUSTY_SEARCH_DEFAULT_ADDR, discover_addr};

use super::PROBE_TIMEOUT;

/// Stable name of this check, as it appears in the report and in
/// `generate::doctor::DOCTOR_CHECKS`.
const CHECK: &str = "search_index_pin";

/// What the project's `.mcp.json` says the session's search index is.
///
/// Why: the four cases carry different verdicts and each is reachable in the
/// field — an unmanaged project has no `.mcp.json` at all, a pre-#1373 stub is
/// present but unpinned, and only the last case gives the probe an id to
/// resolve. Naming them keeps [`build_pin_check`] a pure, exhaustive match.
/// What: the parse outcome of `<project>/.mcp.json`'s
/// `mcpServers["trusty-search"].args`.
/// Test: `read_pin_*` in the `tests` module below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PinState {
    /// No `.mcp.json` in the project — nothing was pinned, nothing to verify.
    NoMcpJson,
    /// `.mcp.json` exists but could not be read or parsed as JSON.
    Unreadable(String),
    /// `.mcp.json` parses but carries no `trusty-search` MCP server.
    NoSearchServer,
    /// A `trusty-search` server is registered without an `--index <id>` pin.
    Unpinned,
    /// The session is pinned to this index id.
    Pinned(String),
}

/// What `GET /indexes/{id}/status` reported for the pinned id.
///
/// Why: `UnknownIndex` is the whole point of this check (issue #5045) and must
/// never be collapsed into the generic error case — a 404 is a definite,
/// actionable "this pin resolves to nothing", whereas a transport failure is
/// an absence of information.
/// What: the classified outcome of one bounded status request.
/// Test: `probe_reports_unknown_index_on_404`, `probe_reports_resolved_on_200`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PinProbe {
    /// The daemon knows the index; `chunk_count` is its reported corpus size
    /// (`None` when the body carried no count).
    Resolved { chunk_count: Option<u64> },
    /// 404 — the daemon has no index under that id.
    UnknownIndex,
    /// The daemon answered with some other non-2xx status.
    HttpStatus(u16),
    /// No trusty-search daemon address could be discovered, or the request
    /// failed at the transport layer.
    Unreachable(String),
}

/// Resolve the session's PINNED trusty-search index and report whether it
/// actually exists (issue #5045).
///
/// Why: the index-registration path is fail-open end to end, so the pin in
/// `.mcp.json` advances even when index creation failed, and the pre-existing
/// `search` probe stays green because it asks about daemon health and the
/// *derived* id rather than resolving the id the session will really use. This
/// probe exists because "health reports ok" is exactly the symptom — see the
/// module doc.
/// What: reads the pin via [`read_pinned_index_id`], resolves the daemon
/// address the same way [`super::doctor`]'s `search` probe does, issues one
/// [`PROBE_TIMEOUT`]-bounded `GET /indexes/{id}/status`, and folds the two
/// results with [`build_pin_check`]. Read-only: it never creates, reindexes,
/// or repairs anything.
/// Test: `pinned_but_missing_index_is_fail` (the full path, over a real
/// socket), plus the `build_pin_check_*` verdict tests.
pub(super) async fn check_search_index_pin(home: &Path, project_dir: Option<&Path>) -> DoctorCheck {
    let Some(project) = project_dir else {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Warn,
            "no project directory supplied — cannot resolve a pinned trusty-search index",
        );
    };

    let pin = read_pinned_index_id(project);
    let probe = match &pin {
        PinState::Pinned(id) => {
            let dir = home.join(".trusty-search");
            let default = TRUSTY_SEARCH_DEFAULT_ADDR
                .parse()
                .expect("static default is valid");
            let env = std::env::var("TRUSTY_SEARCH_ADDR").ok();
            let addr = discover_addr(&dir, default, env.as_deref()).await;
            Some(probe_pinned_index(&format!("http://{addr}"), id).await)
        }
        _ => None,
    };

    build_pin_check(&pin, probe.as_ref())
}

/// Read the `--index <id>` pin out of `<project>/.mcp.json`.
///
/// Why: the pin is the id the session's MCP client will send on every `search`
/// call, so it — not the id doctor would derive today — is the thing whose
/// existence has to be proven. The two can differ whenever the workspace was
/// pinned under a different root, which is the ordinary case for a worktree.
/// What: parses `mcpServers["trusty-search"].args` and returns the element
/// following `--index`. Distinguishes an absent file, an unparseable one, a
/// missing server entry, and an unpinned stub so the caller can report each
/// honestly rather than folding them into one verdict.
/// Test: `read_pin_finds_the_pinned_id`, `read_pin_reports_unpinned_stub`,
/// `read_pin_reports_missing_file`, `read_pin_reports_missing_server`,
/// `read_pin_reports_unreadable_json`.
pub(super) fn read_pinned_index_id(project_dir: &Path) -> PinState {
    let path = project_dir.join(".mcp.json");
    if !path.exists() {
        return PinState::NoMcpJson;
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => return PinState::Unreadable(e.to_string()),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return PinState::Unreadable(e.to_string()),
    };

    let Some(args) = value
        .get("mcpServers")
        .and_then(|m| m.get("trusty-search"))
        .and_then(|s| s.get("args"))
        .and_then(serde_json::Value::as_array)
    else {
        return PinState::NoSearchServer;
    };

    let pinned = args
        .iter()
        .position(|a| a.as_str() == Some("--index"))
        .and_then(|i| args.get(i + 1))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty());

    match pinned {
        Some(id) => PinState::Pinned(id.to_string()),
        None => PinState::Unpinned,
    }
}

/// Ask the daemon whether `index_id` resolves.
///
/// Why: `GET /indexes/{id}/status` is the cheapest endpoint that answers the
/// exact question — it 404s for an id the daemon does not hold, which is the
/// signature issue #5045 reproduced. `/health` and `/indexes` do not: the
/// former reports the daemon, the latter is where the fail-open pin was
/// already passing unnoticed.
/// What: one `GET {base}/indexes/{index_id}/status` bounded by
/// [`PROBE_TIMEOUT`], classified into [`PinProbe`]. A 404 maps to
/// `UnknownIndex`; a 2xx body's `chunk_count` is carried through so the caller
/// can distinguish "resolves and holds a corpus" from "resolves but is empty".
/// Test: `probe_reports_unknown_index_on_404`, `probe_reports_resolved_on_200`,
/// `probe_reports_unreachable_when_nothing_listens`.
pub(super) async fn probe_pinned_index(base: &str, index_id: &str) -> PinProbe {
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => return PinProbe::Unreachable(e.to_string()),
    };
    let url = format!("{base}/indexes/{index_id}/status");
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let chunk_count = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|b| b.get("chunk_count").and_then(serde_json::Value::as_u64));
            PinProbe::Resolved { chunk_count }
        }
        Ok(resp) if resp.status().as_u16() == 404 => PinProbe::UnknownIndex,
        Ok(resp) => PinProbe::HttpStatus(resp.status().as_u16()),
        Err(e) => PinProbe::Unreachable(e.to_string()),
    }
}

/// Fold the pin and its probe into a verdict (pure).
///
/// Why: keeping the verdict logic pure makes every branch — including the one
/// that matters, a live pin the daemon 404s — assertable without a daemon,
/// a filesystem, or a network.
/// What: `Fail` when a pinned id 404s (issue #5045's defect) or the daemon
/// answers another non-2xx; `Warn` for an unpinned stub, an unreadable
/// `.mcp.json`, or a pin that resolves to an EMPTY index (registered but never
/// populated — every query still returns nothing, issue #1908's shape);
/// `Unknown` when the daemon could not be reached, because an unanswered probe
/// has established nothing about the pin; `Ok` only when the pinned id
/// resolves to a non-empty index, or when the project pins nothing at all.
/// Test: `build_pin_check_fails_on_unknown_index`,
/// `build_pin_check_warns_on_empty_index`,
/// `build_pin_check_unreachable_is_unknown_not_ok`,
/// `build_pin_check_ok_when_index_resolves`,
/// `build_pin_check_warns_on_unpinned_stub`.
pub(super) fn build_pin_check(pin: &PinState, probe: Option<&PinProbe>) -> DoctorCheck {
    let id = match pin {
        PinState::NoMcpJson => {
            return DoctorCheck::new(
                CHECK,
                CheckStatus::Ok,
                "no .mcp.json in this project — no trusty-search index pin to resolve",
            );
        }
        PinState::Unreadable(e) => {
            return DoctorCheck::new(
                CHECK,
                CheckStatus::Warn,
                format!(
                    ".mcp.json is present but unreadable ({e}) — whether this session pins a \
                     trusty-search index, and whether that index exists, is UNVERIFIABLE"
                ),
            );
        }
        PinState::NoSearchServer => {
            return DoctorCheck::new(
                CHECK,
                CheckStatus::Ok,
                ".mcp.json registers no trusty-search server — no index pin to resolve",
            );
        }
        PinState::Unpinned => {
            return DoctorCheck::new(
                CHECK,
                CheckStatus::Warn,
                "the trusty-search MCP stub carries no `--index` pin — a bare `search` falls back \
                 to whichever index the daemon guesses for this path, which is the wrong-index \
                 bug issue #1373 fixed by pinning. Relaunch the session to rewrite .mcp.json",
            );
        }
        PinState::Pinned(id) => id,
    };

    // #5045: this is the branch the check exists for. The pin advanced because
    // index registration is fail-open; nothing else in the report notices.
    match probe {
        Some(PinProbe::UnknownIndex) => DoctorCheck::new(
            CHECK,
            CheckStatus::Fail,
            format!(
                "this session is pinned to trusty-search index `{id}` but the daemon has NO such \
                 index — every `search`/`grep` call in this session returns 404 \"unknown index\", \
                 and the `search` health check stays green because it reports the daemon, not the \
                 pin (issue #5045). Either the index was deleted since the pin was written, or the \
                 pin predates #5091. Run `trusty-search index create` for this project, or \
                 relaunch the session"
            ),
        ),
        Some(PinProbe::HttpStatus(code)) => DoctorCheck::new(
            CHECK,
            CheckStatus::Fail,
            format!("status of pinned trusty-search index `{id}` returned HTTP {code}"),
        ),
        Some(PinProbe::Resolved { chunk_count }) => match chunk_count {
            Some(0) => DoctorCheck::new(
                CHECK,
                CheckStatus::Warn,
                format!(
                    "pinned trusty-search index `{id}` exists but holds 0 chunks — it was \
                     registered and never populated, so every query returns nothing. Trigger a \
                     reindex for this project"
                ),
            ),
            Some(n) => DoctorCheck::new(
                CHECK,
                CheckStatus::Ok,
                format!("pinned trusty-search index `{id}` resolves ({n} chunks)"),
            ),
            None => DoctorCheck::new(
                CHECK,
                CheckStatus::Ok,
                format!("pinned trusty-search index `{id}` resolves"),
            ),
        },
        Some(PinProbe::Unreachable(e)) => DoctorCheck::new(
            CHECK,
            CheckStatus::Unknown,
            format!(
                "this session is pinned to trusty-search index `{id}` but the daemon did not \
                 answer ({e}) — whether that index exists is UNKNOWN. The `search` check above \
                 reports the daemon itself"
            ),
        ),
        None => DoctorCheck::new(
            CHECK,
            CheckStatus::Unknown,
            format!("pinned trusty-search index `{id}` was not probed"),
        ),
    }
}

#[cfg(test)]
#[path = "doctor_search_pin_tests.rs"]
mod tests;
