//! `POST /api/v1/sessions/{id}/delegations/builder-slot` — claim one of this
//! machine's builder slots, answered while claiming it (#6892).
//!
//! Why: `tm hook --pm-guard` decides, per `Agent` dispatch, whether the machine
//! has room for another builder. Only the daemon can answer: it holds every
//! session's delegations, and the cap the answer is measured against is a
//! property of the host, not of the asking session. A hook-side ledger would be
//! a second implementation of delegation tracking and would drift.
//!
//! Why POST and not GET: a read-only query cannot close the window it opens.
//! Asking "is a slot free?" and acting on the answer are two steps, and two
//! dispatches issued in ONE PM turn can both ask before either is recorded. So
//! the answer and the record are one operation — see
//! [`DaemonState::claim_builder_slot`].
//!
//! **The daemon resolves the cap, never the caller.**
//! [`resolve_max_concurrent`](crate::core::builders::resolve_max_concurrent)
//! reads `~/.trusty-mpm/config.toml` here, in the process that does the
//! counting. A `tm` older or newer than the daemon would otherwise argue for a
//! number the live leases were not admitted under, and the guard's whole value
//! is that one authority counts.
//!
//! **Eligibility is re-derived here too, never taken on trust.** The caller says
//! which agent it is dispatching; whether that agent claims a builder slot is
//! this daemon's policy call, shared with the guard through the one
//! [`agent_is_builder`](crate::core::dispatch_isolation::agent_is_builder)
//! classifier. A non-builder payload therefore claims nothing even if a caller
//! posts it to this route.
//!
//! It lives in its own module, merged as a sub-router, for the same reason
//! [`delegation_routes`](crate::daemon::delegation_routes) does: `api.rs` is
//! grandfathered at a frozen line-cap budget.
//! Test: the `#[cfg(test)]` suite below.

use std::sync::Arc;

use axum::{Json, Router, extract::Path, extract::State, routing::get, routing::post};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::agent::is_subagent_dispatch_tool;
use crate::core::builder_capacity::Capacity;
#[cfg(test)]
use crate::core::builder_capacity::CapacityReason;
use crate::core::builder_slot_pool::SlotPool;
use crate::core::builders::resolve_max_concurrent;
use crate::core::config::MpmConfig;
use crate::core::dispatch_isolation::{agent_is_builder, dispatch_agent};
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::daemon::error::DaemonError;
use crate::daemon::state::{BuilderHolder, BuilderSlotCensus, DaemonState};

/// Request body of [`builder_slot_route`].
///
/// Why: the route both answers and records, and the recording is done by the
/// delegation tracker's own observer — so the body is simply the payload that
/// observer already consumes, built by `tm hook`'s single `build_hook_payload`.
/// Re-describing the dispatch in a bespoke schema here would be a second
/// construction of one record.
/// What: `payload` carries `cwd`, `tool`, `input` (with `subagent_type`), and
/// `tool_use_id`.
/// Test: `builder_slot_route_claims_a_free_slot`.
#[derive(Debug, Deserialize)]
pub struct BuilderSlotRequest {
    /// The `PreToolUse` hook payload, in the daemon's own forwarded shape.
    pub payload: Value,
}

/// Response of [`builder_slot_route`].
///
/// Why: the guard needs a verdict to act on and names to explain it — a deny
/// that cannot say which builders are running reads as arbitrary and gets
/// retried identically.
/// What: `holders` is one entry per live builder lease, longest-running first;
/// `cap` is the machine's effective `builders.max_concurrent`; `claimed` says
/// whether THIS call took a slot.
///
/// **`ineligible` exists because `claimed: false` had two meanings (#6892 critic
/// round).** It meant both "the machine is full" and "this payload could never
/// have claimed anything", and the hook read the second as the first — denying
/// an idle machine with a message naming zero holders. The two are now separate
/// fields, so a caller cannot conflate them by reading only one.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BuilderSlotResponse {
    /// Builders already holding a slot, excluding this dispatch's own record.
    pub holders: Vec<BuilderHolder>,
    /// The machine's effective builder cap.
    pub cap: u32,
    /// Whether this call claimed a slot.
    #[serde(default)]
    pub claimed: bool,
    /// Whether the daemon judged this payload unable to claim at all — a
    /// non-builder agent, a non-dispatch tool, or no `tool_use_id`. Never a
    /// statement about the machine, and never `true` alongside `claimed`.
    #[serde(default)]
    pub ineligible: bool,
    /// The operator's hard ceiling, which `cap` may now sit below (#8261).
    ///
    /// Why: since #8261 `cap` is the MEASURED slot count, not the configured
    /// one. A refusal that showed only the measured number would read as a
    /// config the operator does not recognise. Additive with `serde(default)`
    /// so a `tm` older than the daemon still parses the answer.
    #[serde(default)]
    pub ceiling: u32,
    /// Why `cap` is what it is, rendered (#8261).
    ///
    /// Why: the closure condition — the refusal must name which condition
    /// failed, show the measured reading AND the configured limit, and
    /// distinguish a reading that could not be taken from one that was
    /// exceeded. Rendered daemon-side so one authority words it.
    #[serde(default)]
    pub capacity_reason: String,
    /// The `builder-cap-*-read-failure` surface, when a reading could not be
    /// taken at all and the count fell back to the fixed ceiling (#8261).
    #[serde(default)]
    pub fail_closed_surface: Option<String>,
    /// The private `CARGO_TARGET_DIR` this claim was granted (#8261).
    ///
    /// Why: the whole point of the slot pool is that the admitted builder builds
    /// somewhere of its own. The guard cannot derive this path — it depends on
    /// `builders.slot_pool_root` and the repo identity, both resolved daemon-side
    /// — so the answer carries it and the guard puts it in the dispatch brief.
    /// `serde(default)` keeps a `tm` older than the daemon parsing the answer.
    #[serde(default)]
    pub slot_path: Option<String>,
    /// How [`Self::slot_path`] came to exist, rendered (#8261).
    #[serde(default)]
    pub slot_seed: Option<String>,
    /// Why an ADMITTED builder got no [`Self::slot_path`] (#8261 critic round).
    ///
    /// Why: three arms admit without a private directory — no repo identity, no
    /// assignable index, and a slot whose seed has still to run — and an
    /// admission that carries no directory and no explanation is
    /// indistinguishable to the engineer from one that was never given a slot.
    #[serde(default)]
    pub slot_notice: Option<String>,
    /// Why the pool REFUSED this claim, when it did (#8261 critic round).
    ///
    /// Why: a pool refusal answers `claimed: false, ineligible: false`, which
    /// the guard read as a full machine and denied with "raise
    /// `builders.max_concurrent`" — the wrong repair for an unwritable pool
    /// root. This field is what tells the two apart.
    #[serde(default)]
    pub slot_refused: Option<String>,
}

/// One claim, plus the seeding the daemon still owes it (#8261 critic round).
///
/// Why: the answer must leave the daemon before the slot is seeded — a 207 GB
/// `cp -c` cannot run inside the hook's 2-second claim budget — so the claim
/// cannot simply return a response. It returns the response AND the work.
/// What: `seed_index` is `Some` only when a slot was reserved but not yet
/// seeded; the route runs [`SlotPool::seed`] for it on a blocking task.
/// Test: `an_unseeded_slot_admits_without_copying_anything_on_the_claim_path`.
pub struct SlotClaimOutcome {
    /// What the caller is answered.
    pub response: BuilderSlotResponse,
    /// The slot index still needing a seed, off the answering path.
    pub seed_index: Option<u32>,
}

/// The builder-slot sub-router (#6892).
///
/// Why: see the module doc — `api.rs` is grandfathered at a frozen line-cap
/// budget, so a new route is registered here and merged rather than appended
/// there.
/// What: the claiming POST under `/sessions/{id}`, and the read-only census GET
/// `tm doctor` reads.
/// Test: `builder_slot_route_claims_a_free_slot`,
/// `builder_slot_census_route_reports_holders_and_the_cap`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new()
        .route(
            "/api/v1/sessions/{id}/delegations/builder-slot",
            post(builder_slot_route),
        )
        .route("/api/v1/builder-slots", get(builder_slot_census_route))
        // #8261: the build-lease decision log rides this router; `api.rs` is at
        // its frozen line-cap budget.
        .merge(super::build_lease_routes::router())
}

/// `POST /api/v1/sessions/{id}/delegations/builder-slot` (#6892).
///
/// Why: see the module doc.
/// What: parses the session id, resolves the machine's cap, and hands the
/// scan-and-claim to [`builder_slot_op`]. A malformed session id is a 400; an
/// unknown session is not an error — a session the daemon has no record of has
/// no delegations, and a 404 would read to the guard as "the daemon could not
/// answer", which this guard denies on.
/// Test: `builder_slot_route_claims_a_free_slot`,
/// `builder_slot_route_rejects_a_malformed_session_id`.
pub async fn builder_slot_route(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(req): Json<BuilderSlotRequest>,
) -> Result<Json<BuilderSlotResponse>, DaemonError> {
    // The count is resolved HERE, in the counting process — see the module doc.
    // #8261: it is now MEASURED per decision rather than read once from the
    // tier table, with `builders.max_concurrent` as the hard ceiling.
    let config = MpmConfig::load_default().builders;
    let capacity = capacity_for(&state, &config, resolve_max_concurrent(), &req.payload);
    // #8261: the pool is resolved HERE, in the daemon, for the same reason the
    // cap is — `builders.slot_pool_root` and the repo identity are the daemon's
    // to read, and a hook deriving them itself would be a second authority.
    let project_dir = str_field(&req.payload, "cwd").map(std::path::PathBuf::from);
    let resolved = resolve_slot_pool(&config, dirs::home_dir(), project_dir.as_deref());
    let outcome = builder_slot_op_with_pool(
        &state,
        &id,
        req,
        &capacity,
        resolved.pool.as_ref(),
        resolved.notice.as_deref(),
    )?;
    // #8261 critic round: the seed is the unbounded half of the pool and runs
    // AFTER the answer, never under the claim mutex the hook is waiting on.
    if let (Some(index), Some(pool)) = (outcome.seed_index, resolved.pool) {
        spawn_seed(Arc::clone(&state), pool, resolved.clone_from, index);
    }
    Ok(Json(outcome.response))
}

/// Run one slot's seed off the answering path, then release its claim (#8261).
///
/// Why: named rather than inlined so the release is impossible to lose. The
/// claim that `DaemonState::reserve_slot_dir` took on this index suppresses
/// every later spawn for it, so a task that returned without releasing would
/// strand the slot unseedable forever — and the error arm is the one most
/// likely to be written without it.
/// What: `spawn_blocking`, because `SlotPool::seed` shells `cp -c -R` over a
/// directory measured at 207 GB and would otherwise block a runtime worker. The
/// release runs from [`SeedGuard`]'s `Drop` rather than as the closure's last
/// statement, so a panic inside `seed` releases the index too — a trailing call
/// is skipped by an unwind, and the index would then be unseedable for the
/// daemon's whole life (#8261 critic round 3).
/// Test: `the_route_sequence_seeds_once_then_grants_the_slot_ready`,
/// `a_panicking_seed_still_releases_its_index`.
fn spawn_seed(
    state: Arc<DaemonState>,
    pool: SlotPool,
    clone_from: Option<std::path::PathBuf>,
    index: u32,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let _guard = SeedGuard { state, index };
        if let Err(err) = pool.seed(index, clone_from.as_deref()) {
            tracing::warn!("builder slot {index} could not be seeded: {err}");
        }
    })
}

/// Holds one slot index's seed claim for as long as the seed runs (#8261).
///
/// Why: see [`spawn_seed`] — the release must survive a panic, and only `Drop`
/// runs during an unwind.
/// Test: `a_panicking_seed_still_releases_its_index`.
struct SeedGuard {
    state: Arc<DaemonState>,
    index: u32,
}

impl Drop for SeedGuard {
    fn drop(&mut self) {
        self.state.finish_builder_seed(self.index);
    }
}

/// The pool for this dispatch's repo, or why there is none.
///
/// Why: [`resolve_slot_pool`] has three ways to answer "no pool", and #8261's
/// critic round found all three admitting silently. Carrying the reason beside
/// the pool is what lets the answer explain itself.
/// What: `pool`/`clone_from` are `Some` together; `notice` is `Some` exactly
/// when `pool` is `None`.
/// Test: `a_dispatch_with_no_repo_identity_admits_with_a_notice`.
struct ResolvedPool {
    pool: Option<SlotPool>,
    clone_from: Option<std::path::PathBuf>,
    notice: Option<String>,
}

/// The capacity THIS dispatch is measured against (#8261 critic round).
///
/// Why: the throttle floors N at the current holder count, so counting the
/// caller's own in-flight record raises that floor by one and the machine always
/// looks as though it has room for exactly one more — the dispatch asking. The
/// route passed `None` here, which made the load and memory throttle admit every
/// builder it was supposed to refuse. The exclusion is the same
/// `tool_use_id` the claim itself excludes, read from the same payload, so the
/// number that admits and the number that counts cannot disagree.
/// What: [`DaemonState::builder_capacity`] with `tool_use_id` excluded.
/// Test: `the_throttle_excludes_the_claimants_own_record`.
fn capacity_for(
    state: &DaemonState,
    config: &crate::core::builders::BuildersConfig,
    ceiling: u32,
    payload: &Value,
) -> Capacity {
    state.builder_capacity(config, ceiling, str_field(payload, "tool_use_id"))
}

/// The slot pool for this dispatch's repo, and the directory to seed from.
///
/// Why: identity and clone source resolve exactly as `doctor_rust_build_env`'s
/// `gather` does, so the pool a builder is given and the row `tm doctor` prints
/// can never disagree about which repo this is or where its warm cache lives.
/// What: no home directory, no `cwd`, or no git identity each yield no pool and
/// a notice naming which — rather than guessing a pool location, and rather than
/// the `"."` home fallback that would have rooted every operator's pool in the
/// daemon's working directory (#8261 critic round).
/// Test: `the_route_grants_a_private_target_dir_from_the_pool`,
/// `a_dispatch_with_no_repo_identity_admits_with_a_notice`.
fn resolve_slot_pool(
    config: &crate::core::builders::BuildersConfig,
    home: Option<std::path::PathBuf>,
    project_dir: Option<&std::path::Path>,
) -> ResolvedPool {
    let no_pool = |why: &str| {
        tracing::warn!("no builder slot pool for this dispatch: {why}");
        ResolvedPool {
            pool: None,
            clone_from: None,
            notice: Some(format!(
                "no private cargo target directory was reserved for this dispatch: {why}. It \
                 builds in the shared target directory and contends on its lock."
            )),
        }
    };
    let Some(home) = home else {
        return no_pool("this daemon cannot resolve a home directory, so the pool root is unknown");
    };
    let Some(project_dir) = project_dir else {
        return no_pool("the dispatch payload named no `cwd`");
    };
    let Some(identity) = trusty_common::github_path::derive_github_path(project_dir) else {
        return no_pool("the checkout has no git origin identity to key a pool by");
    };
    let build = trusty_common::crate_config::load_at::<
        crate::core::trusty_tools_config::TrustyToolsConfig,
    >(&trusty_common::crate_config::crate_config_path_at(
        &home,
        crate::core::trusty_tools_config::CRATE_NAME,
    ))
    .ok()
    .flatten();
    let clone_from = crate::core::build_env::resolve_build_env(
        build.as_ref().and_then(|c| c.build.as_ref()),
        &home,
        Some(&identity),
        crate::core::build_env::host_cores(),
    )
    .ok()
    .map(|env| env.cargo_target_dir);
    ResolvedPool {
        pool: Some(SlotPool::new(
            config.effective_slot_pool_root(&home),
            identity,
        )),
        clone_from,
        notice: None,
    }
}

/// [`builder_slot_route`]'s body, with the cap supplied and no transport in it.
///
/// Why: the cap is the one input that would otherwise make this untestable —
/// [`resolve_max_concurrent`] reads the operator's real `~/.trusty-mpm`, so a
/// test driving the route would depend on the machine it runs on. Taking it as
/// a parameter keeps the route's own logic hermetic and leaves exactly one line
/// (the caller above) asserting which loader answers.
///
/// # Errors
///
/// [`DaemonError::InvalidRequest`] when `id` is not a UUID.
///
/// What: re-derives whether this dispatch claims a slot from the payload —
/// a dispatch tool, a `subagent_type`, and [`agent_is_builder`] — then runs the
/// atomic claim. A payload with no `tool_use_id` is still ANSWERED but claims
/// nothing: without that key the record could not be excluded from its own
/// count, and a dispatch that denied itself would be worse than one that went
/// uncounted. That answer reports `ineligible: true` rather than leaving the
/// caller to read `claimed: false` as a full machine (#6892 critic round).
///
/// A refusal that IS eligible releases the record the guard's preceding
/// shared-tree or worktree-grant claim wrote for this same dispatch — see
/// [`DaemonState::claim_builder_slot`]. #8012: that release goes through
/// [`crate::daemon::services::delegation_tracker::release_denied_dispatch`], so
/// a refusal whose record has not arrived yet leaves the `Cancelled` tombstone
/// the late writer then finds, instead of writing nothing.
/// Test: `builder_slot_route_claims_a_free_slot`,
/// `builder_slot_route_denies_over_the_cap_and_names_the_holders`,
/// `builder_slot_route_claims_nothing_for_a_non_builder`,
/// `builder_slot_route_claims_nothing_without_a_tool_use_id`,
/// `a_denied_builder_releases_the_record_the_dispatch_just_claimed`,
/// `a_denied_builder_tombstones_a_record_that_lands_after_the_deny_8012`,
/// `a_payload_with_no_tool_use_id_is_ineligible_not_full`.
pub fn builder_slot_op(
    state: &Arc<DaemonState>,
    id: &str,
    req: BuilderSlotRequest,
    cap: u32,
) -> Result<BuilderSlotResponse, DaemonError> {
    let session = uuid::Uuid::parse_str(id)
        .map(SessionId)
        .map_err(|_| DaemonError::InvalidRequest(format!("malformed session id: {id}")))?;

    let payload = &req.payload;
    let exclude = str_field(payload, "tool_use_id");
    let input = payload.get("input");
    let eligible = exclude.is_some()
        && str_field(payload, "tool").is_some_and(is_subagent_dispatch_tool)
        && dispatch_agent(input).is_some_and(agent_is_builder);

    let (holders, claimed) = state.claim_builder_slot(
        cap,
        exclude,
        eligible,
        |s| {
            crate::daemon::services::delegation_tracker::observe(
                s,
                session,
                HookEvent::PreToolUse,
                payload,
            );
        },
        // #6892 critic round: a refusal is a DENY, and the guard's preceding
        // shared-tree or worktree-grant claim already recorded this dispatch as
        // Running. Nothing downstream will ever close that record, because a
        // `PreToolUse` deny means the tool never runs.
        //
        // #8012: through the tracker's `release_denied_dispatch`, the same call
        // both sibling deny paths make — it TOMBSTONES a record that has not
        // landed yet, where `DaemonState::release_denied_builder_dispatch` can
        // only close one that already exists.
        |s| {
            crate::daemon::services::delegation_tracker::release_denied_dispatch(
                s, session, payload,
            );
        },
    );
    Ok(BuilderSlotResponse {
        holders,
        cap,
        claimed,
        ineligible: !eligible,
        ceiling: cap,
        capacity_reason: String::new(),
        fail_closed_surface: None,
        slot_path: None,
        slot_seed: None,
        slot_notice: None,
        slot_refused: None,
    })
}

/// [`builder_slot_op_with_capacity`], granting the slot's directory too (#8261).
///
/// Why: the production route's entry point since #8261 — admission alone leaves
/// every admitted builder pointed at the one shared target directory, which is
/// the lock contention this issue exists to end. Kept as a `_with_pool` sibling
/// rather than a parameter on [`builder_slot_op`] so that function, and the
/// eight tests driving it, stay on the index-only contract.
/// What: as [`builder_slot_op_with_capacity`], but the claim runs through
/// [`DaemonState::claim_builder_slot_with_pool`], so a directory the pool cannot
/// RESERVE refuses the claim rather than admitting a builder that would fall
/// back to the shared directory. A slot that reserves but has not been seeded
/// admits with a notice and hands the seed back through
/// [`SlotClaimOutcome::seed_index`] — nothing that can outrun the hook's
/// 2-second claim budget runs on this path (#8261 critic round).
///
/// `no_pool_notice` is what [`resolve_slot_pool`] says when there is no pool at
/// all, carried through so one field explains every directory-less admission.
///
/// # Errors
///
/// As [`builder_slot_op`].
///
/// Test: `the_route_grants_a_private_target_dir_from_the_pool`,
/// `a_pool_that_cannot_provide_a_directory_refuses_the_claim`,
/// `an_unseeded_slot_admits_without_copying_anything_on_the_claim_path`,
/// `a_dispatch_with_no_repo_identity_admits_with_a_notice`.
pub fn builder_slot_op_with_pool(
    state: &Arc<DaemonState>,
    id: &str,
    req: BuilderSlotRequest,
    capacity: &Capacity,
    pool: Option<&SlotPool>,
    no_pool_notice: Option<&str>,
) -> Result<SlotClaimOutcome, DaemonError> {
    let session = uuid::Uuid::parse_str(id)
        .map(SessionId)
        .map_err(|_| DaemonError::InvalidRequest(format!("malformed session id: {id}")))?;

    let payload = &req.payload;
    let exclude = str_field(payload, "tool_use_id");
    let input = payload.get("input");
    let eligible = exclude.is_some()
        && str_field(payload, "tool").is_some_and(is_subagent_dispatch_tool)
        && dispatch_agent(input).is_some_and(agent_is_builder);

    let grant = state.claim_builder_slot_with_pool(
        capacity.n_effective,
        exclude,
        eligible,
        pool,
        |s| {
            crate::daemon::services::delegation_tracker::observe(
                s,
                session,
                HookEvent::PreToolUse,
                payload,
            );
        },
        |s| {
            crate::daemon::services::delegation_tracker::release_denied_dispatch(
                s, session, payload,
            );
        },
    );

    // The pool's own notice wins over the "there is no pool" one: when a pool
    // exists, only it knows why the slot was withheld. A REFUSAL carries no
    // notice at all — `slot_refused` is that answer's explanation.
    let claimed = grant.claimed;
    let slot_notice = grant.slot_notice.or_else(|| {
        claimed
            .then(|| no_pool_notice.map(str::to_string))
            .flatten()
    });
    Ok(SlotClaimOutcome {
        response: BuilderSlotResponse {
            holders: grant.holders,
            cap: capacity.n_effective,
            claimed: grant.claimed,
            ineligible: !eligible,
            ceiling: capacity.ceiling,
            capacity_reason: capacity.reason.to_string(),
            fail_closed_surface: capacity
                .reason
                .fail_closed_surface()
                .map(|surface| surface.name().to_string()),
            slot_path: grant.slot_dir.map(|dir| dir.to_string_lossy().into_owned()),
            slot_seed: grant.slot_seed,
            slot_notice,
            slot_refused: grant.slot_refused,
        },
        seed_index: grant.seed_index,
    })
}

/// [`builder_slot_op`] against a measured capacity rather than a fixed cap (#8261).
///
/// Why: a `_with_capacity` wrapper rather than a fifth parameter on
/// [`builder_slot_op`], because every existing caller and test supplies a plain
/// number and the capacity is only ever built by the two production sites.
/// What: admits against [`Capacity::n_effective`] and carries the ceiling, the
/// rendered reason and any fail-closed surface into the answer, so the guard's
/// refusal can name which condition failed with its reading and its limit
/// without re-deriving any of it.
///
/// # Errors
///
/// As [`builder_slot_op`].
///
/// Test: `the_route_admits_against_the_measured_count_and_reports_the_ceiling`,
/// `a_fail_closed_reading_is_named_in_the_answer`.
pub fn builder_slot_op_with_capacity(
    state: &Arc<DaemonState>,
    id: &str,
    req: BuilderSlotRequest,
    capacity: &Capacity,
) -> Result<BuilderSlotResponse, DaemonError> {
    let mut response = builder_slot_op(state, id, req, capacity.n_effective)?;
    response.ceiling = capacity.ceiling;
    response.capacity_reason = capacity.reason.to_string();
    response.fail_closed_surface = capacity
        .reason
        .fail_closed_surface()
        .map(|surface| surface.name().to_string());
    Ok(response)
}

/// `GET /api/v1/builder-slots` — the read-only census `tm doctor` renders.
///
/// Why: the deny message names holders at the moment of a dispatch; an operator
/// asking "what is holding my machine" has no dispatch to hang that on. This is
/// that question, and it takes no slot: a diagnostic that claimed one would
/// change the thing it reports.
/// What: [`DaemonState::builder_slot_census`] under the machine's resolved cap.
/// Session-free by construction — the cap is machine-wide, so there is no id in
/// the path to scope it by.
/// Test: `builder_slot_census_route_reports_holders_and_the_cap`.
pub async fn builder_slot_census_route(
    State(state): State<Arc<DaemonState>>,
) -> Json<BuilderSlotCensus> {
    Json(state.builder_slot_census(resolve_max_concurrent()))
}

/// Read a non-empty string field from the forwarded hook payload.
fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::{Delegation, DelegationStatus, ModelTier};
    use crate::core::paths::FrameworkPaths;

    /// A hermetic state plus one session id, mirroring `delegation_routes_tests`.
    fn hermetic() -> (Arc<DaemonState>, tempfile::TempDir, SessionId) {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = FrameworkPaths::under(dir.path());
        let state = Arc::new(DaemonState::with_paths(&paths));
        (state, dir, SessionId(uuid::Uuid::new_v4()))
    }

    /// One dispatch in the daemon's forwarded hook shape — what the guard POSTs.
    fn dispatch(agent: &str, tool_use_id: Option<&str>) -> BuilderSlotRequest {
        let mut payload = serde_json::json!({
            "cwd": "/repo",
            "tool": "Agent",
            "input": {"subagent_type": agent, "description": "go"},
        });
        if let Some(id) = tool_use_id {
            payload["tool_use_id"] = Value::String(id.to_string());
        }
        BuilderSlotRequest { payload }
    }

    /// Insert one running builder owned by `session`.
    fn insert_builder(state: &DaemonState, session: SessionId, agent: &str) {
        let mut d = Delegation::new(session, None, agent, ModelTier::Sonnet, "build");
        d.status = DelegationStatus::Running;
        d.started_at = Some(chrono::Utc::now());
        state.upsert_delegation(d);
    }

    /// A pool rooted at `root`, for a fixed test identity.
    fn test_pool(root: std::path::PathBuf) -> SlotPool {
        SlotPool::new(
            root,
            trusty_common::github_path::GithubPath {
                owner: "acme".to_string(),
                repo: "widgets".to_string(),
            },
        )
    }

    /// #8261: the answer must carry the DIRECTORY, not only the verdict — the
    /// guard cannot derive it, so a claim that omits it leaves the engineer
    /// building in the shared target directory.
    #[test]
    fn the_route_grants_a_private_target_dir_from_the_pool() {
        let (state, dir, session) = hermetic();
        let pool = test_pool(dir.path().join("pool"));
        // Seeding never runs on the claim path since #8261's critic round, so a
        // GRANTED directory is by definition one already seeded.
        pool.seed(0, None).expect("a seeded slot 0");
        let body = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_A")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            Some(&pool),
            None,
        )
        .expect("a well-formed session id")
        .response;

        assert!(body.claimed, "a quiet machine admits the first builder");
        let path = body
            .slot_path
            .expect("an admitted builder gets a directory");
        assert!(path.ends_with("slot-0"), "{path}");
        assert!(
            std::path::Path::new(&path).is_dir(),
            "the directory must exist: {path}"
        );
        assert!(
            body.slot_seed.is_some(),
            "the answer says how it was seeded"
        );
    }

    /// #8261 Fail-Open Check, at the route: a pool that cannot provide a
    /// directory must REFUSE, never admit a builder that would then fall back to
    /// the shared directory. Fails before `builder_slot_op_with_pool` existed —
    /// the route ignored the pool and always answered `claimed: true`.
    #[test]
    fn a_pool_that_cannot_provide_a_directory_refuses_the_claim() {
        let (state, dir, session) = hermetic();
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"#8261").expect("write blocker");
        let body = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_A")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            Some(&test_pool(blocker)),
            None,
        )
        .expect("a well-formed session id")
        .response;

        assert!(
            !body.claimed,
            "an unprovidable slot must refuse, not admit unthrottled"
        );
        assert!(body.slot_path.is_none(), "a refusal carries no directory");
        assert!(
            !body.ineligible,
            "this payload WAS eligible; the pool is what failed"
        );
        // #8261 critic round: without this field the guard reads the answer as a
        // full machine and tells the reader to raise `builders.max_concurrent`.
        assert!(
            body.slot_refused
                .as_deref()
                .is_some_and(|d| d.contains("not-a-directory")),
            "a pool refusal must name itself: {:?}",
            body.slot_refused
        );
    }

    /// #8261 critic round, CRITICAL: the claim path clones nothing.
    ///
    /// `SlotPool::acquire_path` ran inside the claim mutex, and `cp -c -R` of a
    /// 207 GB shared directory cannot finish inside the hook's 2-second claim
    /// budget. The sentinel below is what a real clone would have copied.
    #[test]
    fn an_unseeded_slot_admits_without_copying_anything_on_the_claim_path() {
        let (state, dir, session) = hermetic();
        let shared = dir.path().join("shared");
        std::fs::create_dir_all(&shared).expect("a warm shared dir");
        std::fs::write(shared.join("sentinel.rlib"), b"warm").expect("an artifact");
        let pool = test_pool(dir.path().join("pool"));

        let outcome = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_A")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            Some(&pool),
            None,
        )
        .expect("a well-formed session id");

        assert!(
            outcome.response.claimed,
            "an unseeded slot admits: it is the SEED that is deferred"
        );
        assert!(
            outcome.response.slot_path.is_none(),
            "an unseeded slot is not handed out: {:?}",
            outcome.response.slot_path
        );
        assert_eq!(
            outcome.seed_index,
            Some(0),
            "the seed is handed back for the route to run after answering"
        );
        assert!(
            !pool.slot_path(0).join("sentinel.rlib").exists(),
            "nothing may be cloned while the hook waits on the claim"
        );
        assert!(
            outcome.response.slot_notice.is_some(),
            "admitting with no private directory must say so"
        );
    }

    /// #8261 critic round 2, HIGH: at most one seed per index is ever in flight.
    ///
    /// A `Seeding` admission holds its index with no directory, so the dispatch
    /// can END inside the multi-minute clone; `assign_builder_slot` then frees
    /// the index, and the next claim's `reserve_path` still finds no marker.
    /// Without a registry that second claim spawned a second `seed(N)`, whose
    /// first act is `remove_dir_all` of the ONE staging name — deleting the
    /// first run's tree mid-copy, after which whichever survived renamed over
    /// `dst` and wrote `SEED_MARKER` over a directory assembled from two
    /// interleaved runs. Fails without `DaemonState::builder_seeding`.
    #[test]
    fn a_second_reservation_does_not_spawn_a_second_seed() {
        let (state, dir, session) = hermetic();
        let pool = test_pool(dir.path().join("pool"));

        let first = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_A")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            Some(&pool),
            None,
        )
        .expect("a well-formed session id");
        assert_eq!(
            first.seed_index,
            Some(0),
            "the first reservation wins the seed"
        );

        // The first dispatch ends while its seed is still cloning — its lease
        // stops being live, which is what frees slot 0 for the next claim.
        assert!(
            state.release_denied_builder_dispatch(session, Some("toolu_A")),
            "the first dispatch's record must exist to be ended"
        );

        let second = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_B")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            Some(&pool),
            None,
        )
        .expect("a well-formed session id");

        assert!(
            second.response.claimed,
            "the second dispatch is still admitted — only the SEED is suppressed"
        );
        assert_eq!(
            second.seed_index, None,
            "a seed already in flight for this index must not be spawned twice"
        );
        assert!(
            second.response.slot_notice.is_some(),
            "and the admission still says it carries no private directory"
        );

        // The seed task's own release is what makes the index seedable again —
        // without it slot 0 could never be warmed by anyone.
        state.release_denied_builder_dispatch(session, Some("toolu_B"));
        state.finish_builder_seed(0);
        let third = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_C")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            Some(&pool),
            None,
        )
        .expect("a well-formed session id");
        assert_eq!(
            third.seed_index,
            Some(0),
            "a released index is seedable again by the next claim to hold it"
        );
    }

    /// #8261 critic round 2: the spawn arm itself, in the route's own order.
    ///
    /// `builder_slot_route` cannot be driven here — it reads the operator's real
    /// `~/.trusty-mpm` and home directory, and a test that let it resolve the
    /// pool root would write under the real `~/.trusty-tools` (#8311's 42,000
    /// leaked directories). This drives the same two calls the route makes, in
    /// the same order, against a temp pool root.
    #[tokio::test]
    async fn the_route_sequence_seeds_once_then_grants_the_slot_ready() {
        let (state, dir, session) = hermetic();
        let shared = dir.path().join("shared");
        std::fs::create_dir_all(&shared).expect("a warm shared dir");
        std::fs::write(shared.join("sentinel.rlib"), b"warm").expect("an artifact");
        let pool = test_pool(dir.path().join("pool"));

        let first = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_A")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            Some(&pool),
            None,
        )
        .expect("a well-formed session id");
        assert!(
            first.response.slot_path.is_none(),
            "the first claim is cold"
        );

        let index = first.seed_index.expect("the first claim owes a seed");
        spawn_seed(
            Arc::clone(&state),
            pool.clone(),
            Some(shared.clone()),
            index,
        )
        .await
        .expect("the seed task runs to completion");
        // The cold dispatch finishes, freeing slot 0 for the next builder.
        state.release_denied_builder_dispatch(session, Some("toolu_A"));

        let second = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_B")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            Some(&pool),
            None,
        )
        .expect("a well-formed session id")
        .response;

        let path = second
            .slot_path
            .expect("the seeded slot is granted to the next builder");
        assert!(path.ends_with("slot-0"), "{path}");
        assert_eq!(
            second.slot_seed.as_deref(),
            Some("AlreadySeeded"),
            "a seeded slot is granted as warm, not re-seeded"
        );
        assert!(
            second.slot_notice.is_none(),
            "a granted slot owes no explanation: {:?}",
            second.slot_notice
        );
    }

    /// #8261 critic round 3, LOW: the seed's release must survive a panic.
    ///
    /// A trailing `finish_builder_seed(index)` is skipped by an unwind, so a
    /// `cp` that panicked would strand the index in `builder_seeding` for the
    /// daemon's whole life — nobody could ever seed that slot again, and every
    /// builder on it would build in the shared directory. Fails with
    /// `SeedGuard`'s `Drop` body emptied.
    #[test]
    fn a_panicking_seed_still_releases_its_index() {
        let (state, dir, session) = hermetic();
        let pool = test_pool(dir.path().join("pool"));

        let first = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_A")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            Some(&pool),
            None,
        )
        .expect("a well-formed session id");
        assert_eq!(first.seed_index, Some(0), "the claim holds index 0's seed");

        // What the blocking task does when `seed` panics partway.
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = SeedGuard {
                state: Arc::clone(&state),
                index: 0,
            };
            panic!("the clone died mid-copy");
        }));
        std::panic::set_hook(hook);
        assert!(outcome.is_err(), "the seed panicked, as this test intends");

        state.release_denied_builder_dispatch(session, Some("toolu_A"));
        let second = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_B")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            Some(&pool),
            None,
        )
        .expect("a well-formed session id");
        assert_eq!(
            second.seed_index,
            Some(0),
            "a panicked seed must leave the index seedable, not held forever"
        );
    }

    /// #8261 critic round: a checkout with no git identity admits, and the
    /// silence that used to accompany it is what made the engineer build in the
    /// shared directory believing it held a slot.
    #[test]
    fn a_dispatch_with_no_repo_identity_admits_with_a_notice() {
        let (state, _dir, session) = hermetic();
        let body = builder_slot_op_with_pool(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_A")),
            &capacity(
                4,
                4,
                CapacityReason::WaitingOutQuietWindow { remaining_secs: 0 },
            ),
            None,
            Some("the checkout has no git origin identity to key a pool by"),
        )
        .expect("a well-formed session id")
        .response;

        assert!(body.claimed, "no pool is not a refusal");
        assert!(body.slot_path.is_none());
        assert!(
            body.slot_notice
                .as_deref()
                .is_some_and(|n| n.contains("no git origin identity")),
            "the admission must say why it carries no directory: {:?}",
            body.slot_notice
        );
    }

    /// #8261 critic round, HIGH: the load/memory throttle must exclude THIS
    /// dispatch's own record.
    ///
    /// The route passed `None`, so a throttled machine holding two builders
    /// counted three — the two holders plus the claimant's own in-flight record
    /// — floored N at three, and admitted the very dispatch it was refusing.
    /// Fails before `capacity_for` existed.
    #[test]
    fn the_throttle_excludes_the_claimants_own_record() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        insert_builder(&state, session, "python-engineer");
        // What the guard's preceding shared-tree claim already recorded for THIS
        // dispatch, and what the daemon's own `matcher: "*"` hook races to write.
        let mut mine = Delegation::new(session, None, "rust-engineer", ModelTier::Sonnet, "build");
        mine.status = DelegationStatus::Running;
        mine.started_at = Some(chrono::Utc::now());
        mine.tool_use_id = Some("toolu_SELF".to_string());
        state.upsert_delegation(mine);

        // A 1 TiB free-memory floor is the highest `validate` accepts and no
        // host in this fleet can meet it, so the memory throttle fires
        // regardless of what the machine running this test is doing.
        let config = crate::core::builders::BuildersConfig {
            free_memory_floor_mb: Some(1024 * 1024),
            ..crate::core::builders::BuildersConfig::default()
        };
        let request = dispatch("rust-engineer", Some("toolu_SELF"));
        let measured = capacity_for(&state, &config, 4, &request.payload);

        assert_eq!(
            measured.n_effective, 2,
            "a throttled machine floors N at its REAL holders, not at three: {:?}",
            measured.reason
        );
        let body =
            builder_slot_op_with_capacity(&state, &session.0.to_string(), request, &measured)
                .expect("a well-formed session id");
        assert!(
            !body.claimed,
            "the third builder on a throttled two-holder machine must be refused"
        );
    }

    /// #8261: a `Capacity` the test scripts outright, so no reading of the real
    /// machine enters the assertion.
    fn capacity(n_effective: u32, ceiling: u32, reason: CapacityReason) -> Capacity {
        Capacity {
            n_effective,
            ceiling,
            reason,
        }
    }

    /// #8261 closure condition: the route admits against the MEASURED count, and
    /// the answer carries the configured ceiling so the refusal can show both.
    #[test]
    fn the_route_admits_against_the_measured_count_and_reports_the_ceiling() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        // Ceiling 4, but a loaded machine measures room for only the 1 holder.
        let measured = capacity(
            1,
            4,
            CapacityReason::LoadAboveThreshold {
                load: 40.0,
                threshold: 32.0,
            },
        );

        let body = builder_slot_op_with_capacity(
            &state,
            &session.0.to_string(),
            dispatch("python-engineer", Some("toolu_M")),
            &measured,
        )
        .expect("route succeeds");

        assert!(
            !body.claimed,
            "a second builder is refused at a measured count of 1, though the ceiling is 4"
        );
        assert_eq!(body.cap, 1, "the refusal reports the MEASURED count");
        assert_eq!(body.ceiling, 4, "and the configured ceiling beside it");
        assert!(
            body.capacity_reason.contains("40.00"),
            "{}",
            body.capacity_reason
        );
        assert!(
            body.capacity_reason.contains("32.00"),
            "{}",
            body.capacity_reason
        );
        assert_eq!(
            body.fail_closed_surface, None,
            "an EXCEEDED limit is not an UNREADABLE one"
        );
    }

    /// #8261 Fail-Open Check: an unreadable reading fails CLOSED to the fixed
    /// ceiling — the pre-#8261 behaviour — and is NAMED in the answer.
    #[test]
    fn a_fail_closed_reading_is_named_in_the_answer() {
        let (state, _dir, session) = hermetic();
        let failed = capacity(
            4,
            4,
            CapacityReason::FailedClosedToCeiling(crate::core::builder_capacity::ReadFailure {
                surface: crate::core::builder_capacity::FailClosedSurface::Load,
                detail: "operation not permitted".to_string(),
                errno: Some(1),
            }),
        );

        let body = builder_slot_op_with_capacity(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_F")),
            &failed,
        )
        .expect("route succeeds");

        assert!(
            body.claimed,
            "fail CLOSED is the fixed ceiling, not zero slots"
        );
        assert_eq!(body.cap, 4);
        assert_eq!(
            body.fail_closed_surface.as_deref(),
            Some("builder-cap-load-read-failure"),
        );
        assert!(
            body.capacity_reason.contains("errno 1"),
            "{}",
            body.capacity_reason
        );
    }

    /// #8261: the daemon derives N itself, against its OWN quiet window, so two
    /// decisions a moment apart cannot disagree about whether the window closed.
    #[test]
    fn the_daemon_resolves_capacity_against_its_own_quiet_window() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        let config = crate::core::builders::BuildersConfig {
            // A ceiling the live readings cannot lower it below: this asserts
            // the plumbing, not the machine's current load.
            max_concurrent: Some(4),
            ..crate::core::builders::BuildersConfig::default()
        };

        let capacity = state.builder_capacity(&config, 4, None);

        assert_eq!(capacity.ceiling, 4);
        assert!(
            capacity.n_effective >= 1 && capacity.n_effective <= 4,
            "N is clamped into floor..=ceiling whatever this host reads: {capacity:?}"
        );
        assert!(
            !capacity.reason.to_string().is_empty(),
            "the number never travels without its reason"
        );
    }

    #[test]
    fn builder_slot_route_claims_a_free_slot() {
        let (state, _dir, session) = hermetic();
        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_A")),
            2,
        )
        .expect("route succeeds");
        assert!(body.claimed, "an idle machine admits the first builder");
        assert_eq!(body.cap, 2);
        assert!(body.holders.is_empty());
        // The claim IS a delegation record, so the next caller can see it.
        assert_eq!(state.builder_slot_holders(None).len(), 1);
    }

    /// Criterion 1 through the wire shape: the deny names the actual holders,
    /// with agent, session and elapsed time — not a generic string.
    #[test]
    fn builder_slot_route_denies_over_the_cap_and_names_the_holders() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        insert_builder(&state, session, "local-ops");

        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("python-engineer", Some("toolu_C")),
            2,
        )
        .expect("route succeeds");
        assert!(!body.claimed);
        assert_eq!(body.holders.len(), 2);
        let names: Vec<&str> = body.holders.iter().map(|h| h.agent.as_str()).collect();
        assert!(names.contains(&"rust-engineer"), "{names:?}");
        assert!(names.contains(&"local-ops"), "{names:?}");
        assert!(body.holders.iter().all(|h| h.session == session));
    }

    /// Criterion 7 at the route: the daemon re-derives eligibility, so a
    /// research dispatch posted here claims nothing even on an idle machine.
    #[test]
    fn builder_slot_route_claims_nothing_for_a_non_builder() {
        let (state, _dir, session) = hermetic();
        for agent in ["research", "ticketing", "documentation", "version-control"] {
            let body = builder_slot_op(
                &state,
                &session.0.to_string(),
                dispatch(agent, Some("toolu_X")),
                4,
            )
            .expect("route succeeds");
            assert!(!body.claimed, "{agent} must not take a builder slot");
        }
        assert!(state.builder_slot_holders(None).is_empty());
    }

    #[test]
    fn builder_slot_route_claims_nothing_without_a_tool_use_id() {
        let (state, _dir, session) = hermetic();
        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", None),
            4,
        )
        .expect("route succeeds");
        assert!(!body.claimed);
        assert!(state.builder_slot_holders(None).is_empty());
    }

    #[test]
    fn builder_slot_route_rejects_a_malformed_session_id() {
        let (state, _dir, _session) = hermetic();
        let err = builder_slot_op(
            &state,
            "not-a-uuid",
            dispatch("rust-engineer", Some("toolu_A")),
            2,
        )
        .expect_err("a malformed id is a 400");
        assert!(matches!(err, DaemonError::InvalidRequest(_)));
    }

    /// What the shared-tree claim or the worktree grant wrote for THIS dispatch
    /// before the cap was asked.
    ///
    /// Why both shapes: the cap is asked at three ALLOW exits and each is
    /// preceded by a different recorder — the grant's `record_granted_isolation`
    /// upsert on the `Rewrite` arm, and `observe` on the `InPlace` arm and the
    /// fall-through. Both key the record by `tool_use_id`, so the release is
    /// exit-independent, and driving both is what proves that rather than
    /// assuming it.
    fn record_the_preceding_claim_wrote(
        state: &DaemonState,
        session: SessionId,
        payload: &Value,
        granted: bool,
    ) {
        if granted {
            let mut isolated = payload.clone();
            isolated["input"]["isolation"] = Value::String("worktree".to_string());
            crate::daemon::services::delegation_tracker::record_granted_isolation(
                state, session, &isolated,
            );
        } else {
            crate::daemon::services::delegation_tracker::observe(
                state,
                session,
                HookEvent::PreToolUse,
                payload,
            );
        }
    }

    /// The #6892 critic round, HIGH. The call that runs BEFORE the cap at every
    /// one of the guard's three ALLOW exits records a `Running` delegation for
    /// this dispatch on its empty answer — that is how both the #4480 claim and
    /// the ADR-0048 grant work. The cap then denies, and nothing sends the
    /// `SubagentStop` that would close the record, because a `PreToolUse` deny
    /// means the tool never runs. Without a compensating release the record is
    /// live for the six hours of `RUNNING_STALE_AFTER_SECS`, so the "queue and
    /// re-issue" the deny message offers is itself refused — by #4480, with a
    /// message that never mentions the cap.
    ///
    /// Fails before this round: the record stays `Running`, `shared_tree_occupants`
    /// names it, and the retry's claim is refused.
    #[test]
    fn a_denied_builder_releases_the_record_the_dispatch_just_claimed() {
        for granted in [false, true] {
            let (state, _dir, session) = hermetic();
            // The machine's two slots, held by agents with no cwd of their own —
            // so the only thing that can occupy `/repo` is the denied dispatch.
            insert_builder(&state, session, "rust-engineer");
            insert_builder(&state, session, "local-ops");

            let req = dispatch("python-engineer", Some("toolu_DENIED"));
            record_the_preceding_claim_wrote(&state, session, &req.payload, granted);
            let recorded = state.find_delegation(session, |d| {
                d.tool_use_id.as_deref() == Some("toolu_DENIED")
            });
            assert!(
                recorded.is_some(),
                "premise (granted={granted}): the preceding claim records this dispatch"
            );

            let body =
                builder_slot_op(&state, &session.0.to_string(), req, 2).expect("route succeeds");
            assert!(!body.claimed, "granted={granted}: the machine is full");

            // The record must be terminal, not deleted: keeping it is what makes
            // the tracker's own `matcher: "*"` hook a no-op if it lands after the
            // deny (`on_dispatch_locked` returns early on a known `tool_use_id`).
            let id = state
                .find_delegation(session, |d| {
                    d.tool_use_id.as_deref() == Some("toolu_DENIED")
                })
                .expect("the record is retained, not removed");
            let released = state
                .all_delegations()
                .into_iter()
                .find(|d| d.id == id)
                .expect("delegation");
            assert!(
                !released.status.is_live(),
                "granted={granted}: a denied dispatch must not keep a live record: {:?}",
                released.status
            );
            assert!(released.ended_at.is_some(), "granted={granted}");

            // And the remedy the deny offers must actually work: nothing occupies
            // the checkout, so the retry is ADMITTED.
            assert!(
                state
                    .shared_tree_occupants(std::path::Path::new("/repo"), Some("toolu_RETRY"))
                    .is_empty(),
                "granted={granted}: the denied dispatch still occupies the checkout"
            );
            let (occupants, claimed) = state.claim_shared_tree_dispatch(
                std::path::Path::new("/repo"),
                Some("toolu_RETRY"),
                true,
                crate::daemon::state::sessions::SharedTreeQuestion::Dispatch,
                |_| {},
            );
            assert!(
                claimed,
                "granted={granted}: the re-issued dispatch must be admitted, \
                 but #4480 named {occupants:?}"
            );
        }
    }

    /// #8012, and the half the round above did not close. The refusal can only
    /// release a record that EXISTS, and at both of the guard's builder-cap deny
    /// exits it can be absent: the shared-tree claim declines to record a
    /// dispatch that already declares its own isolation (`blocked_by_shared_tree`
    /// is false for it, which is every ADR-0048 worktree dispatch), a grant
    /// against an older daemon 404s and records nothing, and the tracker's own
    /// `matcher: "*"` hook is an independent process that can land after this
    /// one. A record arriving then is `Running` for a dispatch that never ran: it
    /// holds one of the machine's slots for the 45 minutes of
    /// `BUILDER_LEASE_TTL_SECS`, and an unisolated one occupies the checkout for
    /// the six hours of `RUNNING_STALE_AFTER_SECS` as well.
    ///
    /// Both sibling deny paths already tombstone through
    /// `delegation_tracker::release_denied_dispatch` (#7487); this one did not.
    ///
    /// Fails before #8012: the late record is live, the machine counts three
    /// builders against a cap of two, and the unisolated arm's retry is refused
    /// by #4480.
    #[test]
    fn a_denied_builder_tombstones_a_record_that_lands_after_the_deny_8012() {
        // The two exits' dispatch shapes: the grant/`isolation` arm is the
        // Rewrite exit, the bare one is the InPlace exit and the fall-through.
        for isolation in [None, Some("worktree")] {
            let (state, _dir, session) = hermetic();
            insert_builder(&state, session, "rust-engineer");
            insert_builder(&state, session, "local-ops");

            let mut req = dispatch("python-engineer", Some("toolu_LATE"));
            if let Some(mode) = isolation {
                req.payload["input"]["isolation"] = Value::String(mode.to_string());
            }
            let payload = req.payload.clone();
            assert!(
                state
                    .find_delegation(session, |d| d.tool_use_id.as_deref() == Some("toolu_LATE"))
                    .is_none(),
                "premise ({isolation:?}): nothing has recorded this dispatch yet"
            );

            let body =
                builder_slot_op(&state, &session.0.to_string(), req, 2).expect("route succeeds");
            assert!(!body.claimed, "{isolation:?}: the machine is full");

            // The tracker's own `PreToolUse` observation, losing the race.
            crate::daemon::services::delegation_tracker::observe(
                &state,
                session,
                HookEvent::PreToolUse,
                &payload,
            );

            let id = state
                .find_delegation(session, |d| d.tool_use_id.as_deref() == Some("toolu_LATE"))
                .expect("the tombstone or the late record carries the id");
            let record = state
                .all_delegations()
                .into_iter()
                .find(|d| d.id == id)
                .expect("delegation");
            assert!(
                !record.status.is_live(),
                "{isolation:?}: a denied dispatch must not end up live: {:?}",
                record.status
            );
            assert_eq!(
                state.builder_slot_holders(None).len(),
                2,
                "{isolation:?}: the denied dispatch must hold no builder slot"
            );
            assert!(
                state
                    .shared_tree_occupants(std::path::Path::new("/repo"), Some("toolu_RETRY"))
                    .is_empty(),
                "{isolation:?}: the denied dispatch still occupies the checkout"
            );
        }
    }

    /// The release is keyed to the DENIED dispatch and nothing else. A sibling
    /// builder already holding a slot must survive another dispatch's refusal.
    #[test]
    fn a_denied_builder_releases_only_its_own_record() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        insert_builder(&state, session, "local-ops");

        let req = dispatch("python-engineer", Some("toolu_DENIED"));
        record_the_preceding_claim_wrote(&state, session, &req.payload, false);
        let body = builder_slot_op(&state, &session.0.to_string(), req, 2).expect("route succeeds");
        assert!(!body.claimed);

        assert_eq!(
            state.builder_slot_holders(None).len(),
            2,
            "the two holders keep their slots"
        );
    }

    /// An ADMITTED claim releases nothing — the record it just wrote IS the
    /// lease.
    #[test]
    fn an_admitted_builder_keeps_its_record() {
        let (state, _dir, session) = hermetic();
        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", Some("toolu_OK")),
            2,
        )
        .expect("route succeeds");
        assert!(body.claimed);
        assert_eq!(state.builder_slot_holders(None).len(), 1);
    }

    /// The #6892 critic round, MEDIUM. A payload with no `tool_use_id` cannot be
    /// claimed OR excluded from its own count, and answering it `claimed: false`
    /// made the hook read an idle machine as full and deny naming zero holders.
    /// The route now says so in its own field rather than overloading `claimed`.
    #[test]
    fn a_payload_with_no_tool_use_id_is_ineligible_not_full() {
        let (state, _dir, session) = hermetic();
        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("rust-engineer", None),
            4,
        )
        .expect("route succeeds");
        assert!(!body.claimed);
        assert!(
            body.ineligible,
            "an unclaimable payload must not read as a full machine"
        );
        assert!(state.builder_slot_holders(None).is_empty());
    }

    /// The counterpart: a real refusal is NOT ineligible, or the hook would read
    /// a full machine as a payload defect and allow the build.
    #[test]
    fn a_full_machine_is_not_reported_ineligible() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        let body = builder_slot_op(
            &state,
            &session.0.to_string(),
            dispatch("python-engineer", Some("toolu_C")),
            1,
        )
        .expect("route succeeds");
        assert!(!body.claimed);
        assert!(!body.ineligible);
    }

    #[tokio::test]
    async fn builder_slot_census_route_reports_holders_and_the_cap() {
        let (state, _dir, session) = hermetic();
        insert_builder(&state, session, "rust-engineer");
        let Json(census) = builder_slot_census_route(State(Arc::clone(&state))).await;
        assert_eq!(census.holders.len(), 1);
        assert_eq!(census.holders[0].agent, "rust-engineer");
        assert!(census.expired.is_empty());
        // The cap comes from the host config; only its presence is assertable
        // here, since the number depends on the machine running the test.
        assert!(census.cap <= 64);
    }
}
