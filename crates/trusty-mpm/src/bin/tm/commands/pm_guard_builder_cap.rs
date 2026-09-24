//! `tm hook --pm-guard` — machine-wide concurrent-builder denial (#6892).
//!
//! Why: "at most 2 concurrent builders" lived in PM memory, per session. On
//! 2026-08-08 several independent `tm` sessions each honouring their own "2"
//! produced six concurrent `cargo` builds and crashed the host. A per-session
//! rule cannot prevent that — the RAM and CPU being overcommitted belong to the
//! MACHINE, and no session can see another session's builds. This guard moves
//! the rule into the harness, where the daemon counts once for everyone.
//!
//! What: [`evaluate`] denies an `Agent`/`Task` dispatch when, and only when, the
//! named agent [`agent_is_builder`] and the daemon reports the machine's builder
//! slots full — or cannot report at all.
//!
//! **This guard fails CLOSED, and that is a deliberate inversion of the #4480
//! shared-tree guard beside it.** There, an absent daemon allows and warns: a
//! false DENY halts every dispatch in the system, and a false ALLOW merely
//! reproduces pre-#4480 behaviour. Here the two costs are not symmetric. A false
//! ALLOW is an uncounted builder on a machine sized for N, which is precisely
//! the class of event that crashed the host — and once the machine is down there
//! is no session left to retry in. A false DENY costs one dispatch that the
//! operator can unblock by starting the daemon. So every failure arm denies:
//! nothing listening, a route that does not exist, a timeout, a 5xx, a body that
//! does not parse. See [`unverifiable_deny_reason`].
//!
//! **The blast radius of that inversion is bounded to builders, by ordering.**
//! [`dispatch_claims_a_builder_slot`] is local — a bundle scan, no I/O — and it
//! runs BEFORE any daemon round trip. A research, ticketing, qa, documentation
//! or version-control dispatch therefore never reaches the network at all, so a
//! daemon outage cannot deny it. "The daemon is down" degrades builder dispatch
//! only, never every dispatch on the machine.
//!
//! **A DENY undoes the record the guard's previous call just wrote (#6892
//! critic round).** This check is asked LAST, after the shared-tree and
//! worktree-grant rules, and both of those claim BY recording a `Running`
//! delegation on an empty answer. A `PreToolUse` deny means the tool never runs,
//! so nothing downstream would ever close that record: it would hold the
//! checkout and a builder slot for the six hours of `RUNNING_STALE_AFTER_SECS`,
//! and the re-issue this guard's own deny message recommends would then be
//! refused by #4480 with a message that never mentions the cap. The daemon
//! releases it inside the same critical section as the refusal — see
//! `DaemonState::claim_builder_slot`.
//!
//! Test: the `#[cfg(test)]` suite below covers the pure classification and every
//! failure arm; the daemon-side counting, atomicity, and the deny's release are
//! covered by `daemon::state::builder_slots` and `daemon::builder_slot_routes`.

use serde_json::Value;
use std::path::Path;
use trusty_mpm::core::agent::is_subagent_dispatch_tool;
use trusty_mpm::core::dispatch_isolation::{agent_is_builder, dispatch_agent};

use crate::commands::pm_guard_dispatch::{SharedTreeReply, post_shared_tree};

/// The route that answers and claims a builder slot (#6892).
const BUILDER_SLOT_ROUTE: &str = "builder-slot";

/// Would this tool call put another builder on this machine?
///
/// Why: the cheap predicate that gates the daemon call, so every non-builder
/// dispatch — and every ordinary tool call — costs nothing and, more
/// importantly, cannot be denied by a daemon that is not answering. See the
/// module doc's second bolded paragraph: this ordering is what bounds the
/// fail-closed policy to builders.
/// What: `true` when `tool_name` is a subagent-dispatch tool AND the named
/// agent [`agent_is_builder`]. An untyped dispatch — no `subagent_type` — is
/// `false`; it is a separate defect, not this guard's to block.
///
/// Isolation is deliberately NOT consulted. `isolation: "worktree"` gives a
/// builder its own directory, not its own RAM: two isolated `cargo` builds
/// contend for the machine exactly as two unisolated ones do. The shared-tree
/// guard reads isolation because its hazard is one git HEAD; this one's hazard
/// is the host.
/// Test: `a_builder_dispatch_is_gated`, `an_isolated_builder_still_counts`,
/// `non_builder_dispatches_are_not_gated`,
/// `every_non_dispatch_tool_is_not_gated`.
pub(crate) fn dispatch_claims_a_builder_slot(tool_name: &str, tool_input: Option<&Value>) -> bool {
    is_subagent_dispatch_tool(tool_name) && dispatch_agent(tool_input).is_some_and(agent_is_builder)
}

/// Build the deny message for a dispatch the machine has no slot for.
///
/// Why: a bare "denied" leaves the model guessing and it retries the identical
/// call. The text names every current holder — agent and elapsed running time,
/// never its session's UUID (#8257 owner ruling) — so the reader can see whether a slot is about to free or whether one
/// is wedged, states the cap and the key that sets it, and offers remedies that
/// need nothing from the agents already running.
/// What: a single-paragraph `permissionDecisionReason`. Built per call rather
/// than kept as a constant because naming the actual holders is most of its
/// value.
///
/// The remedies are queue-and-retry and raise-the-cap. Waiting is deliberately
/// not offered as an instruction to block on: a lease can run to
/// `BUILDER_LEASE_TTL_SECS`, and telling the PM to wait for something that may
/// take 45 minutes is advice to stall the session.
/// Test: `deny_reason_names_every_holder_the_cap_and_the_config_key`.
pub(crate) fn deny_reason(
    agent: &str,
    cap: u32,
    holders: &[HolderLine],
    note: &CapacityNote,
) -> String {
    let running = holders
        .iter()
        .map(HolderLine::render)
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Machine-wide builder cap reached (#6892): this {agent} dispatch would be builder \
         {next} on a machine capped at {cap}.{measured} Already running: {running}. The cap counts \
         BUILDERS across every session on this host, not per session — on 2026-08-08 several \
         sessions each honouring their own limit produced six concurrent `cargo` builds and \
         crashed the machine, which is why no session can see or raise its own share. \
         `isolation: \"worktree\"` does not exempt a builder: it buys a separate directory, \
         not separate RAM. Queue this dispatch and re-issue it when one of the agents above \
         reports back, or ask the operator to raise `builders.max_concurrent` in \
         `~/.trusty-mpm/config.toml` — a project's `.trusty-mpm.toml` cannot set it, by \
         design. `tm doctor` lists the current holders.",
        next = holders.len() + 1,
        measured = note.render(cap),
    )
}

/// What the daemon measured to arrive at this slot count (#8261).
///
/// Why: since #8261 the cap in a refusal is MEASURED, not configured, and a
/// message showing only the measured number reads as a config the operator does
/// not recognise. The closure condition is explicit: the refusal names which
/// condition failed, shows the reading AND the limit, and distinguishes a
/// reading that could not be TAKEN from one that was EXCEEDED.
/// What: the daemon's own rendering, carried verbatim so one authority words it.
/// A daemon too old to send these fields leaves them empty and
/// [`Self::render`] adds nothing — the pre-#8261 message, unchanged.
/// Test: `deny_reason_names_the_measured_capacity_and_its_ceiling`,
/// `deny_reason_names_an_unreadable_metric_as_such`,
/// `an_old_daemons_answer_renders_the_pre_8261_message`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CapacityNote {
    /// The operator's configured hard ceiling.
    pub(crate) ceiling: u32,
    /// Why the measured count is what it is, as the daemon rendered it.
    pub(crate) reason: String,
    /// The `builder-cap-*-read-failure` surface, when a reading could not be
    /// taken at all.
    pub(crate) fail_closed_surface: Option<String>,
}

impl CapacityNote {
    /// The sentence [`deny_reason`] inserts after the cap, or nothing.
    ///
    /// Test: `deny_reason_names_the_measured_capacity_and_its_ceiling`,
    /// `an_old_daemons_answer_renders_the_pre_8261_message`.
    fn render(&self, cap: u32) -> String {
        if self.reason.is_empty() {
            return String::new();
        }
        let ceiling = if self.ceiling > cap {
            format!(
                " That {cap} is MEASURED capacity, below the configured ceiling of {} \
                 (`builders.max_concurrent`).",
                self.ceiling
            )
        } else {
            String::new()
        };
        let unreadable = match &self.fail_closed_surface {
            Some(surface) => format!(
                " This is a FAIL-CLOSED fallback to the fixed ceiling, not a measured \
                 refusal — the {surface} surface fired."
            ),
            None => String::new(),
        };
        format!("{ceiling} Capacity: {}.{unreadable}", self.reason)
    }
}

/// Read the #8261 capacity fields out of the daemon's answer.
///
/// Why: same reason [`holders_in`] exists — the guard renders what the daemon
/// reports rather than re-deriving it, so the number that refused and the number
/// the message names cannot disagree.
/// What: every field is optional; a daemon predating #8261 yields
/// [`CapacityNote::default`], which renders as the pre-#8261 message.
/// Test: `an_old_daemons_answer_renders_the_pre_8261_message`,
/// `the_capacity_note_is_read_out_of_the_daemons_answer`.
fn capacity_note_in(body: &Value) -> CapacityNote {
    CapacityNote {
        ceiling: body
            .get("ceiling")
            .and_then(Value::as_u64)
            .and_then(|c| u32::try_from(c).ok())
            .unwrap_or_default(),
        reason: body
            .get("capacity_reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        fail_closed_surface: body
            .get("fail_closed_surface")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    }
}

/// One current holder, as the deny message renders it.
///
/// Why: the deny is built in the `tm` binary and the holders arrive as JSON, so
/// the shape the message needs is not the daemon's struct. Keeping it here means
/// the rendering is assertable without a daemon.
/// What: the agent and elapsed time the daemon reports. `elapsed_secs` is
/// rendered as whole minutes, because a builder's age is only ever read at that
/// resolution. The holder's session is not read: its UUID is what a denied
/// caller could replay as `CLAUDE_CODE_SESSION_ID` (#8257 owner ruling).
/// Test: `deny_reason_names_every_holder_the_cap_and_the_config_key`,
/// `holders_are_read_out_of_the_daemons_answer`,
/// `denies_a_builder_when_the_machine_is_full`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HolderLine {
    /// The holding agent's name.
    pub(crate) agent: String,
    /// How long it has been running, in seconds.
    pub(crate) elapsed_secs: i64,
}

impl HolderLine {
    /// `agent (running 12m)`.
    fn render(&self) -> String {
        format!("{} (running {}m)", self.agent, self.elapsed_secs / 60)
    }
}

/// Build the deny message for a claim the daemon could not answer (#6892).
///
/// Why: [`deny_reason`] names the builders already running, and here there is no
/// name to give — the point is precisely that nobody could count. A reader
/// handed that message would go looking for agents that may not exist. The text
/// therefore says what failed, why an unverifiable cap denies rather than
/// allows, and how to restore the count.
/// What: names the failure and the two remedies that need no daemon answer —
/// start or repair the daemon, or serialize the builds by hand until it is back.
/// Test: `unverifiable_reason_names_the_failure_and_why_it_denies`.
pub(crate) fn unverifiable_deny_reason(agent: &str, detail: &str) -> String {
    format!(
        "Builder cap unverifiable (#6892): the daemon did not answer this guard's builder-slot \
         claim — {detail}. That answer is the only thing that can say how many builders are \
         already running on this machine, across every session, so admitting this {agent} \
         dispatch would start a build with the count that exists to bound it never having run. \
         This denies rather than allowing, which is the OPPOSITE of the shared-worktree guard's \
         policy on the same failure and is deliberate: a false allow here overcommits the host, \
         and a machine that goes down takes every session with it, while a false deny costs one \
         dispatch. Start the daemon (`tm start`) or check it (`tm doctor`), then re-issue. \
         Non-builder dispatches — research, ticketing, documentation, qa, version-control — are \
         unaffected and still run: they never reach this check."
    )
}

/// The holders named in a builder-slot answer.
///
/// Why: the guard renders what the daemon reports rather than re-deriving it, so
/// a holder the daemon counted is a holder the deny names.
/// What: `holders[]` rows with an `agent`; a row missing one is skipped rather
/// than rendered as an empty name. `elapsed_secs` defaults to `0` — a daemon
/// too old to send it still produces a usable message.
/// Test: `holders_are_read_out_of_the_daemons_answer`.
fn holders_in(body: &Value) -> Vec<HolderLine> {
    body.get("holders")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(HolderLine {
                        agent: row.get("agent").and_then(Value::as_str)?.to_string(),
                        elapsed_secs: row
                            .get("elapsed_secs")
                            .and_then(Value::as_i64)
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What one builder-slot claim resolved to.
///
/// Why: three outcomes, and collapsing "the daemon did not answer" into "no slot
/// was claimed" would lose the distinction the two deny messages are built on.
/// What: [`Self::Admitted`] took a slot; [`Self::Full`] carries the holders to
/// name; [`Self::Unverifiable`] carries why no answer arrived.
/// Test: `claim_is_unverifiable_when_the_daemon_is_unreachable`,
/// `claim_is_unverifiable_when_the_daemon_answers_500`.
pub(crate) enum BuilderSlotClaim {
    /// A slot was claimed; the dispatch proceeds. Carries the private
    /// `CARGO_TARGET_DIR` the daemon granted it (#8261), or — when it was
    /// admitted without one — the daemon's own notice saying why.
    Admitted {
        /// The granted private `CARGO_TARGET_DIR`, when the pool had one ready.
        slot_path: Option<String>,
        /// Why this admission carries no directory (#8261 critic round).
        notice: Option<String>,
    },
    /// The machine is at its cap. Carries the cap, its current holders, and
    /// (#8261) what the daemon measured to arrive at that cap.
    Full(u32, Vec<HolderLine>, CapacityNote),
    /// The slot POOL refused, which is not a full machine (#8261 critic round).
    /// Carries the daemon's detail, which names the path and the errno.
    PoolRefused(String),
    /// The daemon answered, reported the machine's state, and declined to count
    /// this dispatch at all — it classified the agent as a non-builder where
    /// this binary classified it as one.
    NotCounted,
    /// No usable answer, so the count is unknown. Carries the failure detail.
    Unverifiable(String),
}

/// Claim a builder slot for this dispatch, and learn who already holds one.
///
/// Why: the count and the claim must be indivisible or two dispatches in one PM
/// turn both see a free slot — see [`trusty_mpm::daemon`]'s builder-slot route.
/// The hook cannot make them so; the daemon can, and this is the call that asks
/// it to.
/// What: POSTs through the shared delegation-guard wire contract
/// ([`post_shared_tree`], which takes the route) so the endpoint, the payload
/// projection and the 500 ms / 2 s bounds are the same ones every other
/// `PreToolUse` guard call uses. Every failure arm — including
/// [`SharedTreeReply::Unavailable`], which the shared-tree claim ALLOWS on —
/// becomes [`BuilderSlotClaim::Unverifiable`] here. See the module doc.
/// Test: `claim_is_unverifiable_when_the_daemon_is_unreachable`,
/// `claim_is_unverifiable_when_the_daemon_answers_500`,
/// `claim_is_unverifiable_when_the_body_does_not_parse`,
/// `claim_is_admitted_when_the_daemon_says_so`.
pub(crate) async fn claim_builder_slot(
    url: &str,
    session_id: &str,
    cwd: &Path,
    payload: &Value,
) -> BuilderSlotClaim {
    match post_shared_tree(url, session_id, cwd, payload, BUILDER_SLOT_ROUTE).await {
        SharedTreeReply::Answered(body) => {
            // A body carrying no `claimed` verdict is not an answer to this
            // question — reading its absence as "no slot" would render a deny
            // naming a cap of zero and no holders, which is nonsense the reader
            // cannot act on. It is version skew or a wrong route, and both leave
            // the count unknown.
            let Some(claimed) = body.get("claimed").and_then(Value::as_bool) else {
                return BuilderSlotClaim::Unverifiable(
                    "the daemon's answer carries no `claimed` verdict, so it is not this route's"
                        .to_string(),
                );
            };
            if claimed {
                return BuilderSlotClaim::Admitted {
                    slot_path: str_body_field(&body, "slot_path"),
                    notice: str_body_field(&body, "slot_notice"),
                };
            }
            // #8261 critic round: a pool refusal answers `claimed: false,
            // ineligible: false`, exactly as a full machine does. Read as a full
            // machine it produced a deny telling the operator to raise
            // `builders.max_concurrent`, which does nothing for an unwritable
            // pool root. This field is checked BEFORE the cap for that reason.
            if let Some(detail) = str_body_field(&body, "slot_refused") {
                return BuilderSlotClaim::PoolRefused(detail);
            }
            // #6892 critic round: `claimed: false` had two meanings and this is
            // the second — the daemon answered with the machine's real state and
            // declined to count this dispatch. Reading it as a full machine
            // denied on an idle host, naming zero holders.
            if body.get("ineligible").and_then(Value::as_bool) == Some(true) {
                return BuilderSlotClaim::NotCounted;
            }
            let cap = body
                .get("cap")
                .and_then(Value::as_u64)
                .and_then(|c| u32::try_from(c).ok())
                .unwrap_or_default();
            BuilderSlotClaim::Full(cap, holders_in(&body), capacity_note_in(&body))
        }
        // #6892: unlike the #4480 guard, an absent daemon is NOT a degraded mode
        // this path accepts. See the module doc for why the costs are not
        // symmetric between the two guards.
        SharedTreeReply::Unavailable(detail) | SharedTreeReply::Unanswered(detail) => {
            BuilderSlotClaim::Unverifiable(detail)
        }
    }
}

/// Read a non-empty string field out of the daemon's answer.
///
/// Test: `a_pool_refusal_denies_naming_the_path_rather_than_the_cap`.
fn str_body_field(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Build the deny message for a slot the POOL refused (#8261 critic round).
///
/// Why: this refusal has nothing to do with the cap, and [`deny_reason`] was
/// rendering it as one — "cap reached … raise `builders.max_concurrent`" for a
/// pool root that is a file, or a disk with no space. The reader followed that
/// advice and the next dispatch was refused identically.
/// What: names the daemon's own detail, which carries the path and the errno,
/// and gives the two repairs that actually apply.
/// Test: `a_pool_refusal_denies_naming_the_path_rather_than_the_cap`.
pub(crate) fn pool_refused_deny_reason(agent: &str, detail: &str) -> String {
    format!(
        "Builder slot directory refused (#8261): the daemon admitted this {agent} dispatch \
         against the machine's builder capacity, then could not give it a private cargo target \
         directory — {detail}. This is NOT a full machine and raising \
         `builders.max_concurrent` will not clear it. A builder with no private directory would \
         build in the shared `CARGO_TARGET_DIR` and contend on its lock with every other builder \
         on this host, which is the failure #8261 exists to end, so the dispatch is refused \
         instead. Fix the path named above — check that `builders.slot_pool_root` in \
         `~/.trusty-mpm/config.toml` names a writable directory and that its volume has space — \
         then re-issue. `tm doctor` reports the pool root in its build-environment row."
    )
}

/// The field this payload is missing that makes the claim impossible, if any.
///
/// Why: two keys are load-bearing for a claim and NEITHER is this guard's to
/// supply. `session_id` addresses the route; `tool_use_id` is what lets the
/// daemon exclude this dispatch's own record from its own count, and without it
/// a claim would be refused by the record the guard itself just wrote. Checking
/// both HERE — locally, before the POST — is what makes them one class of
/// outcome rather than two: the round trip is skipped, and the answer cannot be
/// confused with a full machine (#6892 critic round). The daemon re-derives the
/// same rule and reports `ineligible` for a caller that skips this check.
/// What: `Some("session id")` or `Some("tool_use_id")` — the first missing key,
/// for the warning — else `None`.
/// Test: `a_payload_with_no_session_id_allows_and_warns`,
/// `a_payload_with_no_tool_use_id_allows_and_warns`.
fn unclaimable_field(payload: &Value, session_id: &str) -> Option<&'static str> {
    if session_id.is_empty() {
        return Some("session id");
    }
    let has_tool_use_id = payload
        .get("tool_use_id")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    (!has_tool_use_id).then_some("tool_use_id")
}

/// Warn that a payload missing `field` cannot claim a slot.
///
/// Why: this is the ONE class of input failure that allows rather than denies,
/// and the asymmetry needs stating. Every other failure this guard meets is the
/// daemon declining to answer, which is correlated with load and therefore with
/// a busy machine — exactly when a false allow costs the most. A payload missing
/// one of the two keys a claim needs is a different thing: there is nothing to
/// POST to, or nothing to key the claim by, and the failure is in the caller's
/// own input rather than in the machine's state. Claude Code stamps both
/// `session_id` and `tool_use_id` on every `PreToolUse`, so reaching this line
/// means the payload did not come from it — and a caller that can edit the
/// payload can already set `TRUSTY_MPM_DISABLE_HOOKS`, so denying buys nothing
/// while breaking every harness whose payload shape this binary does not
/// control.
/// What: one stderr line, the same channel and reasoning as
/// `pm_guard_dispatch::warn_guard_unavailable`.
/// Test: `a_payload_with_no_session_id_allows_and_warns`,
/// `a_payload_with_no_tool_use_id_allows_and_warns`.
fn warn_unaddressable(field: &str) {
    eprintln!(
        "tm hook --pm-guard: this dispatch's payload carries no {field}, so the machine-wide \
         builder cap (#6892) could not be claimed for it. The cap is NOT being enforced for this \
         dispatch — an uncounted builder may now be running. Every payload Claude Code emits \
         carries both `session_id` and `tool_use_id`; a payload without one did not come from it. \
         Allowing the dispatch."
    );
}

/// Resolve the builder-cap verdict for one `PreToolUse` call: `Some` denies.
///
/// Why: the one entry point `pm_guard` calls, so the ordering that keeps the
/// daemon off the hot path — classify locally first, ask second — lives here
/// rather than being re-derived at the call site. That ordering is load-bearing:
/// it is what stops a daemon outage from denying non-builder dispatches.
/// What: `None` immediately unless [`dispatch_claims_a_builder_slot`]; then one
/// atomic claim, denying when the machine is full or the count is unverifiable.
/// Test: `claim_is_admitted_when_the_daemon_says_so`,
/// `denies_a_builder_when_the_daemon_is_down`,
/// `allows_a_research_dispatch_when_the_daemon_is_down`,
/// `denies_a_builder_when_the_machine_is_full`,
/// `a_payload_with_no_session_id_allows_and_warns`.
pub(crate) async fn evaluate(
    url: &str,
    payload: &Value,
    tool_name: &str,
    tool_input: Option<&Value>,
    session_id: &str,
    cwd: &Path,
) -> BuilderCapVerdict {
    if !dispatch_claims_a_builder_slot(tool_name, tool_input) {
        return BuilderCapVerdict::Allow(None);
    }
    if let Some(missing) = unclaimable_field(payload, session_id) {
        warn_unaddressable(missing);
        return BuilderCapVerdict::Allow(None);
    }
    let agent = dispatch_agent(tool_input).unwrap_or("this");
    match claim_builder_slot(url, session_id, cwd, payload).await {
        // #8261 critic round: an admission with no directory carries the
        // daemon's notice instead, so the engineer is never left to infer from
        // silence that it holds a private target directory.
        BuilderSlotClaim::Admitted { slot_path, notice } => {
            BuilderCapVerdict::Allow(slot_path.map(|dir| slot_notice(&dir)).or(notice))
        }
        BuilderSlotClaim::Full(cap, holders, note) => {
            BuilderCapVerdict::Deny(deny_reason(agent, cap, &holders, &note))
        }
        BuilderSlotClaim::PoolRefused(detail) => {
            BuilderCapVerdict::Deny(pool_refused_deny_reason(agent, &detail))
        }
        // ALLOW, and say so on stderr. The daemon ANSWERED here — the machine's
        // count is known, this dispatch simply was not added to it — so unlike
        // the arm below there is no open question to fail closed on. The usual
        // cause is a daemon older than this `tm`, and denying on version skew
        // would halt every build until a restart, which is the same trade #5324
        // already settled the same way for the shared-tree guard.
        BuilderSlotClaim::NotCounted => {
            warn_not_counted(agent);
            BuilderCapVerdict::Allow(None)
        }
        BuilderSlotClaim::Unverifiable(detail) => {
            BuilderCapVerdict::Deny(unverifiable_deny_reason(agent, &detail))
        }
    }
}

/// What the builder cap decided, and what the engineer must be told (#8261).
///
/// Why: an allow now carries information — the private `CARGO_TARGET_DIR` the
/// daemon granted — so `Option<String>` could no longer express the answer: its
/// `Some` already meant "deny". Two named arms make the allow-with-a-notice case
/// unmissable at the one call site.
/// What: [`Self::Deny`] carries the refusal; [`Self::Allow`] carries an optional
/// notice to merge into the hook's single output object.
/// Test: `an_admitted_builder_allows_with_its_target_dir_notice`.
pub(crate) enum BuilderCapVerdict {
    /// The dispatch is refused, for this reason.
    Deny(String),
    /// The dispatch proceeds, optionally carrying a notice for the engineer.
    Allow(Option<String>),
}

/// Print the builder-cap deny, or `allowed` when the machine has room (#6892).
///
/// Why: the worktree grant has two ALLOW exits and both print a rewrite and
/// return, so the machine cap has to be asked at each of them or a granted
/// dispatch escapes it entirely. Folded into one helper rather than written
/// twice because the two arms differ only in which rewrite they emit, and a
/// `PreToolUse` hook's stdout may carry exactly one object — duplicating the
/// print/deny pair is how a second one gets emitted.
///
/// It lives HERE rather than in `pm_guard.rs` because that file sits at the
/// 500-SLOC cap, and because every line of it is about this module's verdict.
/// What: runs [`evaluate`]; on a deny it audits and prints the deny, on an allow
/// it prints `allowed` with any slot notice merged into that same single
/// `hookSpecificOutput`. Exactly one object reaches stdout either way.
/// Test: `pm_guard_grants_a_worktree_to_a_writer_in_a_main_checkout` and
/// `pm_guard_denies_the_second_of_two_simultaneous_dispatches` in
/// `tests/tm_hook_pm_guard.rs` cover the allow exits;
/// `a_slot_notice_is_merged_into_the_one_hook_output_object` covers the merge.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn emit_builder_cap_or(
    url: &str,
    payload: &Value,
    tool_name: &str,
    tool_input: Option<&Value>,
    session_id: &str,
    hook_cwd: &Path,
    allowed: &str,
) {
    match evaluate(url, payload, tool_name, tool_input, session_id, hook_cwd).await {
        BuilderCapVerdict::Deny(reason) => {
            super::pm_guard::audit_denied_tool(url, session_id, tool_name, &reason).await;
            println!(
                "{}",
                super::pm_guard_response::build_pretooluse_deny_response(&reason)
            );
        }
        // #8261: the slot notice merges INTO the grant's own object — a second
        // printed object would be a second `hookSpecificOutput`.
        BuilderCapVerdict::Allow(notice) => println!(
            "{}",
            super::pm_guard_worktree_grant::with_additional_context(allowed, notice.as_deref())
        ),
    }
}

/// The line an admitted builder is told to prefix on every cargo invocation.
///
/// Why (#6868 closure condition 3): an agent's shell environment does not
/// persist between tool calls, so a slot directory it is not told to prefix
/// INLINE is a slot directory it will not use — and it would then build in the
/// shared one, which is the contention #8261 exists to end.
/// Test: `an_admitted_builder_allows_with_its_target_dir_notice`.
fn slot_notice(dir: &str) -> String {
    format!(
        "Builder slot (#8261): this dispatch holds a private cargo target \
         directory. Prefix `CARGO_TARGET_DIR={dir}` INLINE on every cargo \
         command — an exported variable does not survive between tool calls, and \
         building in the shared directory contends on its lock with every other \
         builder on this machine."
    )
}

/// Warn that the daemon declined to count a dispatch this binary calls a builder.
///
/// Why: the divergence is always a disagreement, never a normal outcome — the
/// guard does not reach the daemon at all unless it has already classified the
/// dispatch as a builder. Discarding the signal would leave the cap silently
/// under-counting, which is exactly the invisible degradation #5324's own
/// warning exists to prevent for the shared-tree guard.
/// What: one stderr line, naming the likely cause and the repair.
/// Test: `an_ineligible_answer_allows_rather_than_denying`.
fn warn_not_counted(agent: &str) {
    eprintln!(
        "tm hook --pm-guard: the daemon does not classify {agent} as a builder, so this dispatch \
         was NOT counted against the machine-wide cap (#6892) — the machine may now be running \
         one more builder than it is sized for. The usual cause is a running daemon older than \
         the `tm` on PATH, built before this agent joined its bundled table; `tm restart` clears \
         it. Allowing the dispatch: the daemon answered, so the count is known and this is a \
         classification disagreement rather than an unverifiable cap."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::agent::SUBAGENT_DISPATCH_TOOLS;

    fn input(agent: &str, isolation: Option<&str>) -> Value {
        match isolation {
            Some(i) => serde_json::json!({"subagent_type": agent, "isolation": i}),
            None => serde_json::json!({"subagent_type": agent}),
        }
    }

    fn holders() -> Vec<HolderLine> {
        vec![
            HolderLine {
                agent: "rust-engineer".to_string(),
                elapsed_secs: 754,
            },
            HolderLine {
                agent: "local-ops".to_string(),
                elapsed_secs: 61,
            },
        ]
    }

    // ---- the local classification ---------------------------------------

    #[test]
    fn a_builder_dispatch_is_gated() {
        for tool in SUBAGENT_DISPATCH_TOOLS {
            for agent in ["rust-engineer", "engineer", "local-ops"] {
                assert!(
                    dispatch_claims_a_builder_slot(tool, Some(&input(agent, None))),
                    "{tool}/{agent}"
                );
            }
        }
    }

    /// A worktree buys a separate directory, not separate RAM — two isolated
    /// `cargo` builds contend for the machine exactly as two unisolated ones do.
    #[test]
    fn an_isolated_builder_still_counts() {
        assert!(dispatch_claims_a_builder_slot(
            "Agent",
            Some(&input("rust-engineer", Some("worktree")))
        ));
        assert!(dispatch_claims_a_builder_slot(
            "Agent",
            Some(&input("rust-engineer", Some("remote")))
        ));
    }

    /// Criterion 7's guard-side half: a non-builder dispatch is not classified,
    /// so it never reaches the daemon call at all.
    #[test]
    fn non_builder_dispatches_are_not_gated() {
        for agent in [
            "research",
            "ticketing",
            "qa",
            "documentation",
            "version-control",
            "code-critic",
        ] {
            assert!(!dispatch_claims_a_builder_slot(
                "Agent",
                Some(&input(agent, None))
            ));
        }
        // An untyped dispatch and a nameless one are both ungated.
        assert!(!dispatch_claims_a_builder_slot("Agent", None));
        assert!(!dispatch_claims_a_builder_slot(
            "Agent",
            Some(&serde_json::json!({"description": "go"}))
        ));
    }

    #[test]
    fn every_non_dispatch_tool_is_not_gated() {
        for tool in ["Bash", "Edit", "Write", "Read", "Grep"] {
            assert!(!dispatch_claims_a_builder_slot(
                tool,
                Some(&input("rust-engineer", None))
            ));
        }
    }

    // ---- the two deny messages ------------------------------------------

    #[test]
    fn deny_reason_names_every_holder_the_cap_and_the_config_key() {
        let reason = deny_reason("python-engineer", 2, &holders(), &CapacityNote::default());
        // Every holder, with agent and elapsed time.
        assert!(reason.contains("rust-engineer (running 12m)"), "{reason}");
        assert!(reason.contains("local-ops (running 1m)"), "{reason}");
        // The cap and the key that sets it.
        assert!(reason.contains("capped at 2"), "{reason}");
        assert!(reason.contains("builders.max_concurrent"), "{reason}");
        assert!(reason.contains("~/.trusty-mpm/config.toml"), "{reason}");
        // And a remedy that needs nothing from the agents already running.
        assert!(reason.contains("Queue this dispatch"), "{reason}");
    }

    /// #8261: an old daemon sends no capacity fields, and the message it
    /// produces must be exactly the pre-#8261 one — no dangling "Capacity:".
    #[test]
    fn an_old_daemons_answer_renders_the_pre_8261_message() {
        let note = capacity_note_in(&serde_json::json!({"cap": 2, "claimed": false}));
        assert_eq!(note, CapacityNote::default());
        let reason = deny_reason("python-engineer", 2, &holders(), &note);
        assert!(!reason.contains("Capacity:"), "{reason}");
        assert!(!reason.contains("MEASURED"), "{reason}");
    }

    #[test]
    fn the_capacity_note_is_read_out_of_the_daemons_answer() {
        let note = capacity_note_in(&serde_json::json!({
            "ceiling": 4,
            "capacity_reason": "1-minute load average 40.00 is above the threshold 32.00",
            "fail_closed_surface": serde_json::Value::Null,
        }));
        assert_eq!(note.ceiling, 4);
        assert!(note.reason.contains("40.00"));
        assert_eq!(note.fail_closed_surface, None);
    }

    /// #8261 closure condition: the refusal shows the MEASURED count against the
    /// configured ceiling, plus the reading and the limit behind it.
    #[test]
    fn deny_reason_names_the_measured_capacity_and_its_ceiling() {
        let note = CapacityNote {
            ceiling: 4,
            reason: "1-minute load average 40.00 is above the threshold 32.00 \
                     (logical cores x builders.load_factor)"
                .to_string(),
            fail_closed_surface: None,
        };
        let reason = deny_reason("rust-engineer", 2, &holders(), &note);
        assert!(reason.contains("MEASURED capacity"), "{reason}");
        assert!(reason.contains("ceiling of 4"), "{reason}");
        assert!(
            reason.contains("40.00") && reason.contains("32.00"),
            "the reading AND the limit must both appear: {reason}"
        );
        assert!(
            !reason.contains("FAIL-CLOSED"),
            "an EXCEEDED limit is not an UNREADABLE one: {reason}"
        );
    }

    /// #8261 closure condition: an unreadable metric must be distinguishable
    /// from an exceeded one, by name.
    #[test]
    fn deny_reason_names_an_unreadable_metric_as_such() {
        let note = CapacityNote {
            ceiling: 4,
            reason: "builder-cap-load-read-failure — the Load reading could not be taken \
                     (operation not permitted, errno 1)"
                .to_string(),
            fail_closed_surface: Some("builder-cap-load-read-failure".to_string()),
        };
        let reason = deny_reason("rust-engineer", 4, &holders(), &note);
        assert!(reason.contains("FAIL-CLOSED"), "{reason}");
        assert!(reason.contains("builder-cap-load-read-failure"), "{reason}");
        assert!(reason.contains("errno 1"), "{reason}");
        // cap == ceiling here, so there is no "below the configured ceiling".
        assert!(!reason.contains("MEASURED capacity"), "{reason}");
    }

    #[test]
    fn unverifiable_reason_names_the_failure_and_why_it_denies() {
        let reason = unverifiable_deny_reason("rust-engineer", "nothing is listening at :4317");
        assert!(reason.contains("cap unverifiable"), "{reason}");
        assert!(reason.contains("nothing is listening at :4317"), "{reason}");
        assert!(reason.contains("tm start"), "{reason}");
        // It must say the non-builder dispatches are unaffected, or the reader
        // concludes the whole harness is down.
        assert!(reason.contains("still run"), "{reason}");
    }

    #[test]
    fn holders_are_read_out_of_the_daemons_answer() {
        let body = serde_json::json!({
            "cap": 2,
            "claimed": false,
            "holders": [
                {"agent": "rust-engineer", "session": "sess-a", "elapsed_secs": 90},
                {"session": "sess-b", "elapsed_secs": 10},
            ],
        });
        let rows = holders_in(&body);
        assert_eq!(rows.len(), 1, "a row with no agent name is skipped");
        assert_eq!(rows[0].agent, "rust-engineer");
        assert_eq!(rows[0].elapsed_secs, 90);
    }

    // ---- the network half ------------------------------------------------

    /// A single-shot HTTP server answering one canned response.
    fn spawn_mock_answering(status_line: &'static str, body: &'static str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes());
            }
        });
        url
    }

    /// The refusal a verdict carries, or `None` when it allows.
    ///
    /// #8261: `evaluate` returns a two-armed verdict rather than
    /// `Option<String>`, because an ALLOW now carries a notice too. These tests
    /// ask only "was it denied, and why", so they fold the allow arm away here.
    fn deny(verdict: BuilderCapVerdict) -> Option<String> {
        match verdict {
            BuilderCapVerdict::Deny(reason) => Some(reason),
            BuilderCapVerdict::Allow(_) => None,
        }
    }

    /// Evaluate a `rust-engineer` dispatch against `url`.
    async fn evaluate_builder_against(url: &str) -> Option<String> {
        deny(
            evaluate(
                url,
                &serde_json::json!({"tool_use_id": "toolu_X"}),
                "Agent",
                Some(&input("rust-engineer", None)),
                "11111111-1111-1111-1111-111111111111",
                Path::new("/repo"),
            )
            .await,
        )
    }

    /// #8261: an admitted builder is ALLOWED and told where to build. The notice
    /// must name the directory, or the engineer builds in the shared one.
    #[tokio::test]
    async fn an_admitted_builder_allows_with_its_target_dir_notice() {
        let url = spawn_mock_answering(
            "200 OK",
            r#"{"claimed":true,"cap":4,"holders":[],"slot_path":"/pool/acme/widgets/slot-0"}"#,
        );
        let verdict = evaluate(
            &url,
            &serde_json::json!({"tool_use_id": "toolu_X"}),
            "Agent",
            Some(&input("rust-engineer", None)),
            "11111111-1111-1111-1111-111111111111",
            Path::new("/repo"),
        )
        .await;
        match verdict {
            BuilderCapVerdict::Allow(Some(notice)) => {
                assert!(
                    notice.contains("/pool/acme/widgets/slot-0"),
                    "the notice must name the directory: {notice}"
                );
                assert!(
                    notice.contains("CARGO_TARGET_DIR"),
                    "the notice must name the variable: {notice}"
                );
            }
            BuilderCapVerdict::Allow(None) => {
                panic!("an admitted builder with a slot path must carry a notice")
            }
            BuilderCapVerdict::Deny(reason) => panic!("must not deny: {reason}"),
        }
    }

    /// #8261 critic round: a POOL refusal answers `claimed: false,
    /// ineligible: false`, the same shape a full machine answers with. Read as
    /// a full machine it denied with "cap reached … raise `max_concurrent`",
    /// which does nothing for a pool root that is a file. Fails before
    /// `BuilderSlotClaim::PoolRefused` existed.
    #[tokio::test]
    async fn a_pool_refusal_denies_naming_the_path_rather_than_the_cap() {
        let url = spawn_mock_answering(
            "200 OK",
            r#"{"claimed":false,"ineligible":false,"cap":4,"ceiling":4,"holders":[],
                "slot_refused":"could not create builder slot directory /pool/acme/widgets: Not a directory (os error 20)"}"#,
        );
        let reason = deny(
            evaluate(
                &url,
                &serde_json::json!({"tool_use_id": "toolu_X"}),
                "Agent",
                Some(&input("rust-engineer", None)),
                "11111111-1111-1111-1111-111111111111",
                Path::new("/repo"),
            )
            .await,
        )
        .expect("a pool refusal denies");

        assert!(
            reason.contains("/pool/acme/widgets") && reason.contains("os error 20"),
            "the deny must name the path and the errno: {reason}"
        );
        assert!(
            !reason.contains("cap reached"),
            "a pool refusal is not a full machine: {reason}"
        );
        assert!(
            reason.contains("slot_pool_root"),
            "the deny must name the repair that applies: {reason}"
        );
    }

    /// #8261 critic round: an admission that carries no directory must say why,
    /// or the engineer cannot tell it from one that was given a slot.
    #[tokio::test]
    async fn an_admission_without_a_directory_carries_the_daemons_notice() {
        let url = spawn_mock_answering(
            "200 OK",
            r#"{"claimed":true,"cap":4,"holders":[],
                "slot_notice":"builder slot /pool/acme/widgets/slot-0 had not been seeded yet"}"#,
        );
        let verdict = evaluate(
            &url,
            &serde_json::json!({"tool_use_id": "toolu_X"}),
            "Agent",
            Some(&input("rust-engineer", None)),
            "11111111-1111-1111-1111-111111111111",
            Path::new("/repo"),
        )
        .await;

        match verdict {
            BuilderCapVerdict::Allow(Some(notice)) => assert!(
                notice.contains("had not been seeded yet"),
                "the daemon's notice must reach the engineer: {notice}"
            ),
            BuilderCapVerdict::Allow(None) => {
                panic!("an admission with no directory must still explain itself")
            }
            BuilderCapVerdict::Deny(reason) => panic!("must not deny: {reason}"),
        }
    }

    /// Criterion 5. Nothing is listening — the count is unknowable, so the
    /// dispatch is DENIED. A copy-paste of #4480's allow-on-unreachable policy
    /// returns `None` here and fails this test.
    #[tokio::test]
    async fn denies_a_builder_when_the_daemon_is_down() {
        let reason = evaluate_builder_against("http://127.0.0.1:1")
            .await
            .expect("an unverifiable cap must deny");
        assert!(reason.contains("cap unverifiable"), "{reason}");
    }

    /// Criterion 6. Same dead daemon, same turn — a research dispatch is
    /// ALLOWED, because the local classifier answers before any network call.
    #[tokio::test]
    async fn allows_a_research_dispatch_when_the_daemon_is_down() {
        for agent in ["research", "ticketing", "documentation", "version-control"] {
            let verdict = deny(
                evaluate(
                    "http://127.0.0.1:1",
                    &serde_json::json!({"tool_use_id": "toolu_X"}),
                    "Agent",
                    Some(&input(agent, None)),
                    "11111111-1111-1111-1111-111111111111",
                    Path::new("/repo"),
                )
                .await,
            );
            assert!(verdict.is_none(), "{agent} must not be denied: {verdict:?}");
        }
    }

    /// The one input failure that allows. It reaches no daemon at all, so the
    /// unreachable-daemon deny above cannot be what answers it — see
    /// `warn_unaddressable` for why the two are not the same failure.
    #[tokio::test]
    async fn a_payload_with_no_session_id_allows_and_warns() {
        let verdict = deny(
            evaluate(
                "http://127.0.0.1:1",
                &serde_json::json!({"tool_use_id": "toolu_X"}),
                "Agent",
                Some(&input("rust-engineer", None)),
                "",
                Path::new("/repo"),
            )
            .await,
        );
        assert!(verdict.is_none(), "{verdict:?}");
    }

    /// The #6892 critic round, MEDIUM. A payload with no `tool_use_id` cannot be
    /// claimed OR excluded from its own count, so the daemon answers it
    /// `claimed: false` — and reading that as "the machine is full" produced a
    /// deny naming zero holders on an idle machine. It is the same class of
    /// input failure as a missing session id, and it is answered the same way:
    /// decided locally, before the POST, allow and warn.
    ///
    /// Fails before this round: the guard POSTs, reads `claimed: false` as
    /// `Full`, and denies.
    #[tokio::test]
    async fn a_payload_with_no_tool_use_id_allows_and_warns() {
        let url = spawn_mock_answering("200 OK", r#"{"claimed":false,"cap":4,"holders":[]}"#);
        let verdict = deny(
            evaluate(
                &url,
                &serde_json::json!({"cwd": "/repo"}),
                "Agent",
                Some(&input("rust-engineer", None)),
                "11111111-1111-1111-1111-111111111111",
                Path::new("/repo"),
            )
            .await,
        );
        assert!(
            verdict.is_none(),
            "an unclaimable payload must not read as a full machine: {verdict:?}"
        );
    }

    /// A daemon that answered and declined to COUNT this dispatch reported the
    /// machine's real state, so there is no open question to fail closed on.
    /// Denying would halt every build on version skew.
    #[tokio::test]
    async fn an_ineligible_answer_allows_rather_than_denying() {
        let url = spawn_mock_answering(
            "200 OK",
            r#"{"claimed":false,"ineligible":true,"cap":4,"holders":[]}"#,
        );
        let verdict = evaluate_builder_against(&url).await;
        assert!(verdict.is_none(), "{verdict:?}");
    }

    /// The counterpart, so the allow above cannot widen into "any `claimed:
    /// false` allows". With a `tool_use_id` present, `claimed: false` IS a full
    /// machine and denies.
    #[tokio::test]
    async fn a_payload_with_a_tool_use_id_still_denies_when_full() {
        let url = spawn_mock_answering(
            "200 OK",
            r#"{"claimed":false,"cap":1,"holders":[{"agent":"local-ops","session":"s","elapsed_secs":30}]}"#,
        );
        let reason = evaluate_builder_against(&url)
            .await
            .expect("a full machine denies");
        assert!(
            reason.contains("Machine-wide builder cap reached"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn claim_is_unverifiable_when_the_daemon_is_unreachable() {
        assert!(matches!(
            claim_builder_slot(
                "http://127.0.0.1:1",
                "11111111-1111-1111-1111-111111111111",
                Path::new("/repo"),
                &serde_json::json!({"tool_use_id": "toolu_X"}),
            )
            .await,
            BuilderSlotClaim::Unverifiable(_)
        ));
    }

    /// A daemon that HAS the route and failed to serve it counted nothing.
    #[tokio::test]
    async fn claim_is_unverifiable_when_the_daemon_answers_500() {
        let url = spawn_mock_answering("500 Internal Server Error", r#"{"error":"boom"}"#);
        let reason = evaluate_builder_against(&url)
            .await
            .expect("a 500 leaves the count unknown");
        assert!(reason.contains("cap unverifiable"), "{reason}");
    }

    /// A daemon too OLD to have this route also cannot count — and unlike the
    /// shared-tree guard, version skew denies here rather than allowing.
    #[tokio::test]
    async fn claim_is_unverifiable_when_the_route_is_absent() {
        let url = spawn_mock_answering("404 Not Found", r#"{"error":"no route"}"#);
        let reason = evaluate_builder_against(&url)
            .await
            .expect("a daemon without the route cannot bound the machine");
        assert!(reason.contains("cap unverifiable"), "{reason}");
    }

    /// An answer that parses but carries no verdict is not an answer to this
    /// question. Reading its absence as "no slot" produced a deny naming a cap
    /// of zero and no holders — a message the reader cannot act on.
    #[tokio::test]
    async fn claim_is_unverifiable_when_the_answer_has_the_wrong_shape() {
        let url = spawn_mock_answering("200 OK", r#"{"agents":[],"total":0}"#);
        let reason = evaluate_builder_against(&url)
            .await
            .expect("a body with no `claimed` verdict leaves the count unknown");
        assert!(reason.contains("cap unverifiable"), "{reason}");
        assert!(!reason.contains("capped at 0"), "{reason}");
    }

    #[tokio::test]
    async fn claim_is_unverifiable_when_the_body_does_not_parse() {
        let url = spawn_mock_answering("200 OK", "not json at all");
        let reason = evaluate_builder_against(&url)
            .await
            .expect("an unparseable answer is not an answer");
        assert!(reason.contains("cap unverifiable"), "{reason}");
    }

    #[tokio::test]
    async fn claim_is_admitted_when_the_daemon_says_so() {
        let url = spawn_mock_answering("200 OK", r#"{"claimed":true,"cap":2,"holders":[]}"#);
        assert!(evaluate_builder_against(&url).await.is_none());
    }

    /// The full machine, end to end: the daemon says no slot and names who has
    /// them, and the guard renders that into the deny.
    #[tokio::test]
    async fn denies_a_builder_when_the_machine_is_full() {
        let url = spawn_mock_answering(
            "200 OK",
            r#"{"claimed":false,"cap":2,"holders":[
                {"agent":"rust-engineer","session":"5f0e2c1a-1111-4222-8333-944445555666","elapsed_secs":600},
                {"agent":"local-ops","session":"sess-b","elapsed_secs":120}]}"#,
        );
        let reason = evaluate_builder_against(&url)
            .await
            .expect("a full machine denies");
        assert!(reason.contains("rust-engineer"), "{reason}");
        assert!(reason.contains("local-ops"), "{reason}");
        // #8257 owner ruling: the holder's session UUID, in either form, is a
        // value the denied caller could replay as its own identity.
        for form in [
            "5f0e2c1a-1111-4222-8333-944445555666",
            "5f0e2c1a111142228333944445555666",
            "sess-b",
        ] {
            assert!(!reason.contains(form), "{form} leaked: {reason}");
        }
        assert!(reason.contains("capped at 2"), "{reason}");
        assert!(reason.contains("builders.max_concurrent"), "{reason}");
    }
}
