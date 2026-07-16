//! Pure decision logic and per-session ledger for the idle-park auto-nudge
//! (#2621, follow-up to #2610 / PR #2620).
//!
//! Why: `core::idle_parking` detects that a delegated agent ended its turn with
//! monitor/notification "waiting" language after backgrounding a long command —
//! a stall a stopped agent can never recover from on its own. #2621 asks the
//! daemon to auto-nudge such a session instead of waiting for a human to notice.
//! Doing that safely needs three things kept OUT of the effectful daemon code so
//! they can be unit-tested without a tmux/session-manager: the canonical nudge
//! message, a per-session ledger that bounds how often any one session is
//! nudged, and a single pure predicate that folds every safety rail into one
//! [`NudgeDecision`]. This module owns exactly those.
//! What: [`DEFAULT_NUDGE_MESSAGE`] is the injected text; [`NudgeLedger`] tracks
//! `(count, last_nudge_at)` per session id and prunes stale entries;
//! [`decide_nudge`] is the pure gate — it returns [`NudgeDecision::Nudge`] only
//! when the feature is enabled, the turn genuinely parked, the session has no
//! live tracked children, the per-session nudge cap is not exhausted, and the
//! cooldown since the last nudge has elapsed; otherwise it returns
//! [`NudgeDecision::Skip`] with the specific [`SkipReason`]. Nothing here does
//! any I/O.
//! Test: the inline `tests` module below.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `[idle_nudge]` config section — opt-in auto-nudge for idle-parked sessions.
///
/// Why: delegated agents in tm-managed sessions repeatedly "idle-park" — they
/// end a turn with monitor/notification "waiting" language after backgrounding a
/// long command, then sit idle forever because a stopped agent can never be
/// re-woken by the thing it is waiting on. A human has had to notice and send a
/// manual nudge. This section lets the daemon do it automatically, bounded by
/// conservative caps. It lives HERE (not in `core::config`) alongside the
/// [`decide_nudge`] logic and [`NudgeLedger`] it configures, so the whole
/// feature — type, decision, and ledger — is one cohesive, testable unit;
/// `MpmConfig` merely embeds it as the `[idle_nudge]` field.
/// What: `enabled` (default `false`, zero behavior change) turns the feature on;
/// `max_nudges_per_session` caps re-nudges so a stuck session is never spammed
/// (`0` disables as firmly as `enabled = false`); `cooldown_secs` is the minimum
/// gap between two nudges to the SAME session; `message` is the literal text
/// injected (defaults to [`DEFAULT_NUDGE_MESSAGE`]).
/// Test: `config_idle_nudge_section_parses` in `core::config`; the field
/// semantics via every `decide_nudge` test below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleNudgeConfig {
    /// Whether the daemon auto-nudges idle-parked managed sessions.
    ///
    /// `false` (default) → feature is OFF, zero behavior change. `true` → a
    /// parking `Stop`/`SubagentStop` with no live tracked children triggers a
    /// nudge into the correlated managed session's pane.
    #[serde(default)]
    pub enabled: bool,

    /// Maximum number of auto-nudges delivered to any single session.
    ///
    /// Why: a session that stays parked despite a nudge must not be nudged in an
    /// unbounded loop. Once this many nudges have been sent to a session the
    /// daemon stops nudging it (a human must intervene). Default: 2.
    #[serde(default = "IdleNudgeConfig::default_max_nudges_per_session")]
    pub max_nudges_per_session: u32,

    /// Minimum seconds between two nudges to the SAME session.
    ///
    /// Why: several parking hooks can fire close together (e.g. a `SubagentStop`
    /// followed by the parent `Stop`); the cooldown collapses that burst into a
    /// single nudge. Default: 300 s (5 min).
    #[serde(default = "IdleNudgeConfig::default_cooldown_secs")]
    pub cooldown_secs: u64,

    /// The literal text injected into a parked pane (submitted with Enter).
    ///
    /// Defaults to [`DEFAULT_NUDGE_MESSAGE`], a short foreground-block
    /// instruction citing #2621.
    #[serde(default = "IdleNudgeConfig::default_message")]
    pub message: String,
}

impl IdleNudgeConfig {
    fn default_max_nudges_per_session() -> u32 {
        2
    }
    fn default_cooldown_secs() -> u64 {
        300
    }
    fn default_message() -> String {
        DEFAULT_NUDGE_MESSAGE.to_string()
    }
}

impl Default for IdleNudgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_nudges_per_session: Self::default_max_nudges_per_session(),
            cooldown_secs: Self::default_cooldown_secs(),
            message: Self::default_message(),
        }
    }
}

/// The default text injected into a parked pane, submitted with Enter.
///
/// Why: the nudge must do more than say "you stalled" — it must correct the
/// exact anti-pattern (#2501/#2610): the agent backgrounded a watch and ended
/// its turn. The message tells it to block in the FOREGROUND and keep its turn
/// open, which is the behavior the bundled agent guidance already prescribes.
/// What: a `'static` instruction string citing #2621 so an operator reading the
/// pane knows the message was machine-injected, not typed by a human.
/// Test: `config_idle_nudge_section_parses` (in `core::config`) asserts the
/// config default equals this constant; `default_message_mentions_foreground`
/// below.
pub const DEFAULT_NUDGE_MESSAGE: &str = "[trusty-mpm auto-nudge #2621] Your last turn ended with \
language that parks the task waiting on a background monitor/notification, but a stopped agent \
cannot be re-woken by the thing it is waiting on. Resume NOW: block in the FOREGROUND (re-run the \
gate, or `gh pr checks <n> --watch`) and do not end your turn until it actually completes. If you \
are genuinely blocked on a human decision, say so explicitly instead.";

/// Per-session record of how many nudges were sent and when the last one fired.
///
/// Why: the two spam guards — a hard per-session cap and a cooldown between
/// nudges — both need this small piece of history. Keeping it a plain `Copy`
/// value makes [`decide_nudge`] pure (it reads a snapshot) and the ledger a
/// trivial map.
/// What: `count` is the number of nudges delivered to the session so far;
/// `last_nudge_at` is the UTC instant of the most recent one (`None` before the
/// first).
/// Test: `cap_blocks_after_max`, `cooldown_blocks_within_window`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NudgeRecord {
    /// Number of nudges already delivered to this session.
    pub count: u32,
    /// When the most recent nudge was delivered, or `None` if never.
    pub last_nudge_at: Option<DateTime<Utc>>,
}

/// Why a nudge was NOT sent — the diagnostic half of [`NudgeDecision`].
///
/// Why: every skip path is a distinct, log-worthy reason; an enum keeps the
/// daemon's structured logging and the unit tests aligned on one vocabulary
/// instead of stringly-typed reasons.
/// What: one variant per safety rail, in the order [`decide_nudge`] checks them.
/// Test: `disabled_skips`, `not_parking_skips`, `human_wait_skips`,
/// `live_children_skips`, `cap_blocks_after_max`, `cooldown_blocks_within_window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The `[idle_nudge]` feature is disabled (`enabled = false`).
    Disabled,
    /// The turn-end did not actually park (no parking phrase was detected).
    NotParking,
    /// The parking phrase addresses a HUMAN ("let me know once…", "ping me
    /// when…"), not a monitor/notification — a legitimate user-wait that #2621
    /// requires the auto-nudge to never interrupt (code-critic HIGH 1). See
    /// [`crate::core::idle_parking::is_nudge_eligible`].
    HumanWait,
    /// The session still has live tracked children — it is not truly stalled.
    HasLiveChildren,
    /// The per-session nudge cap (`max_nudges_per_session`) is exhausted.
    CapExhausted,
    /// A nudge fired too recently; the cooldown window has not elapsed.
    WithinCooldown,
}

impl SkipReason {
    /// A stable lowercase tag for structured logging (`"disabled"`, …).
    ///
    /// Why: the daemon logs the skip reason as a field; a stable tag keeps log
    /// queries robust against `Debug`-format churn.
    /// What: maps each variant to a fixed `&'static str`.
    /// Test: `skip_reason_tags_are_stable`.
    pub fn tag(self) -> &'static str {
        match self {
            SkipReason::Disabled => "disabled",
            SkipReason::NotParking => "not_parking",
            SkipReason::HumanWait => "human_wait",
            SkipReason::HasLiveChildren => "has_live_children",
            SkipReason::CapExhausted => "cap_exhausted",
            SkipReason::WithinCooldown => "within_cooldown",
        }
    }
}

/// The verdict of [`decide_nudge`]: nudge the session, or skip with a reason.
///
/// Why: the caller must both act (inject) and log (why/why-not); a two-variant
/// enum carries both without a side channel.
/// What: `Nudge` means all rails passed; `Skip(reason)` names the first rail
/// that failed.
/// Test: covered by every `decide_nudge` test below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NudgeDecision {
    /// All safety rails passed — deliver the nudge.
    Nudge,
    /// Do not nudge; `.0` is the first rail that failed.
    Skip(SkipReason),
}

impl NudgeDecision {
    /// Whether this decision is [`NudgeDecision::Nudge`].
    ///
    /// Why: the common caller branch is "did we decide to nudge?"; a helper
    /// reads better than a `matches!` at each call site.
    /// What: `true` for `Nudge`, `false` for any `Skip`.
    /// Test: `enabled_and_parking_and_idle_nudges`.
    pub fn should_nudge(self) -> bool {
        matches!(self, NudgeDecision::Nudge)
    }
}

/// Decide whether to nudge a parked session, folding in every safety rail (pure).
///
/// Why: concentrating the gate in one side-effect-free function makes the
/// conservative posture the issue demands — disable-able, cap-bounded,
/// cooldown-bounded, never-nudge-a-busy-session, never-nudge-a-human-wait —
/// auditable and unit-testable without any daemon plumbing. The daemon layer
/// computes the three runtime inputs (`phrase`, `has_live_children`, and the
/// session's [`NudgeRecord`]) and defers the decision entirely to this function.
/// What: returns [`NudgeDecision::Nudge`] iff, in order: the feature is
/// `enabled`; `phrase` is `Some` (the turn actually parked) AND
/// [`crate::core::idle_parking::is_nudge_eligible`] accepts it (excludes a
/// human-addressed wait like "let me know once…" — #2621 code-critic HIGH 1);
/// the session has NO live tracked children (`!has_live_children`); the prior
/// nudge `count` is `<` `max_nudges_per_session`; and either no nudge has fired
/// yet OR at least `cooldown_secs` have elapsed since `last_nudge_at` at `now`.
/// The first failing rail yields the matching [`NudgeDecision::Skip`]. A
/// `max_nudges_per_session` of `0` disables nudging as firmly as
/// `enabled = false`.
/// Test: `disabled_skips`, `not_parking_skips`, `human_wait_skips`,
/// `live_children_skips`, `cap_blocks_after_max`, `cooldown_blocks_within_window`,
/// `enabled_and_parking_and_idle_nudges`, `nudge_allowed_after_cooldown`.
pub fn decide_nudge(
    cfg: &IdleNudgeConfig,
    phrase: Option<&str>,
    has_live_children: bool,
    record: &NudgeRecord,
    now: DateTime<Utc>,
) -> NudgeDecision {
    if !cfg.enabled {
        return NudgeDecision::Skip(SkipReason::Disabled);
    }
    let Some(phrase) = phrase else {
        return NudgeDecision::Skip(SkipReason::NotParking);
    };
    if !crate::core::idle_parking::is_nudge_eligible(phrase) {
        return NudgeDecision::Skip(SkipReason::HumanWait);
    }
    if has_live_children {
        return NudgeDecision::Skip(SkipReason::HasLiveChildren);
    }
    if record.count >= cfg.max_nudges_per_session {
        return NudgeDecision::Skip(SkipReason::CapExhausted);
    }
    if let Some(last) = record.last_nudge_at {
        let elapsed = now.signed_duration_since(last);
        if elapsed < Duration::seconds(cfg.cooldown_secs as i64) {
            return NudgeDecision::Skip(SkipReason::WithinCooldown);
        }
    }
    NudgeDecision::Nudge
}

/// In-memory, per-session ledger of nudge history.
///
/// Why: the cap and cooldown rails need history that survives across hook
/// receipts but NOT across a daemon restart — a restart resetting the counters
/// only re-arms the (already conservative) nudge, never spams. A plain map keyed
/// by session id is enough; it lives behind a lock in `DaemonState`.
/// What: wraps `HashMap<Uuid, NudgeRecord>`. [`snapshot`](Self::snapshot) reads a
/// session's current record (default when unseen); [`record_nudge`](Self::record_nudge)
/// bumps the count and stamps the time; [`prune`](Self::prune) drops entries for
/// sessions no longer live so the map cannot grow without bound. Production
/// callers MUST call [`prune`](Self::prune) periodically — it is not automatic —
/// which `daemon::idle_nudge::run_nudge` does on every nudge attempt, keyed off
/// the same `SessionManager::list()` call it already makes to correlate the
/// hook's Claude session id.
/// Test: `ledger_snapshot_defaults_when_unseen`, `ledger_record_bumps_count`,
/// `ledger_prune_drops_stale`;
/// `daemon::idle_nudge::run_nudge_prunes_stale_ledger_entries` exercises the
/// production wiring end to end.
#[derive(Debug, Default)]
pub struct NudgeLedger {
    records: HashMap<Uuid, NudgeRecord>,
}

impl NudgeLedger {
    /// Create an empty ledger.
    ///
    /// Why: constructed once and held in `DaemonState`.
    /// What: zero-initialised map.
    /// Test: `ledger_snapshot_defaults_when_unseen`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the current [`NudgeRecord`] for `session`, defaulting when unseen.
    ///
    /// Why: [`decide_nudge`] needs the session's history as a snapshot; a
    /// never-nudged session reads as the zero record.
    /// What: returns a copy of the stored record, or `NudgeRecord::default()`.
    /// Test: `ledger_snapshot_defaults_when_unseen`, `ledger_record_bumps_count`.
    pub fn snapshot(&self, session: Uuid) -> NudgeRecord {
        self.records.get(&session).copied().unwrap_or_default()
    }

    /// Record that a nudge was delivered to `session` at `now`.
    ///
    /// Why: after a successful inject the ledger must reflect the new count and
    /// timestamp so the next decision sees the updated cap/cooldown state.
    /// What: increments `count` (saturating) and sets `last_nudge_at = now` for
    /// the session, inserting a fresh record when unseen.
    /// Test: `ledger_record_bumps_count`.
    pub fn record_nudge(&mut self, session: Uuid, now: DateTime<Utc>) {
        let entry = self.records.entry(session).or_default();
        entry.count = entry.count.saturating_add(1);
        entry.last_nudge_at = Some(now);
    }

    /// Drop ledger entries for sessions not in `live`.
    ///
    /// Why: a long-lived daemon must not accumulate records for sessions that
    /// have ended; pruning against the current live set bounds the map.
    /// What: `HashMap::retain` keeping only keys present in `live`.
    /// Test: `ledger_prune_drops_stale`.
    pub fn prune(&mut self, live: &std::collections::HashSet<Uuid>) {
        self.records.retain(|id, _| live.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_cfg() -> IdleNudgeConfig {
        IdleNudgeConfig {
            enabled: true,
            max_nudges_per_session: 2,
            cooldown_secs: 300,
            message: "nudge".into(),
        }
    }

    #[test]
    fn default_message_mentions_foreground() {
        // The canonical message must correct the actual anti-pattern.
        let m = DEFAULT_NUDGE_MESSAGE.to_lowercase();
        assert!(m.contains("foreground"), "must tell the agent to block");
        assert!(m.contains("#2621"), "must be attributable to the feature");
    }

    /// A machine-wait phrase (nudge-eligible per `core::idle_parking`), used as
    /// the standard "genuinely parked" input across these tests.
    const MACHINE_PHRASE: &str = "i'll wait for the";
    /// A human-addressed phrase (NOT nudge-eligible), used by `human_wait_skips`.
    const HUMAN_PHRASE: &str = "let me know once";

    #[test]
    fn skip_reason_tags_are_stable() {
        assert_eq!(SkipReason::Disabled.tag(), "disabled");
        assert_eq!(SkipReason::NotParking.tag(), "not_parking");
        assert_eq!(SkipReason::HumanWait.tag(), "human_wait");
        assert_eq!(SkipReason::HasLiveChildren.tag(), "has_live_children");
        assert_eq!(SkipReason::CapExhausted.tag(), "cap_exhausted");
        assert_eq!(SkipReason::WithinCooldown.tag(), "within_cooldown");
    }

    #[test]
    fn disabled_skips() {
        // Feature OFF is the firmest rail — it wins even when everything else
        // would nudge.
        let mut cfg = enabled_cfg();
        cfg.enabled = false;
        let d = decide_nudge(
            &cfg,
            Some(MACHINE_PHRASE),
            false,
            &NudgeRecord::default(),
            Utc::now(),
        );
        assert_eq!(d, NudgeDecision::Skip(SkipReason::Disabled));
        assert!(!d.should_nudge());
    }

    #[test]
    fn not_parking_skips() {
        // A clean turn-end (no phrase at all) must never be nudged.
        let cfg = enabled_cfg();
        let d = decide_nudge(&cfg, None, false, &NudgeRecord::default(), Utc::now());
        assert_eq!(d, NudgeDecision::Skip(SkipReason::NotParking));
    }

    #[test]
    fn human_wait_skips() {
        // A parking phrase that addresses a HUMAN ("let me know once…") must
        // never be nudged — #2621 code-critic HIGH 1: never nudge a legitimate
        // user-wait, even though it still counts as "parking" for logging.
        let cfg = enabled_cfg();
        let d = decide_nudge(
            &cfg,
            Some(HUMAN_PHRASE),
            false,
            &NudgeRecord::default(),
            Utc::now(),
        );
        assert_eq!(d, NudgeDecision::Skip(SkipReason::HumanWait));
    }

    #[test]
    fn live_children_skips() {
        // A session that still has live tracked children is doing work — the
        // "no live children" precondition (#2621 design note) must suppress it.
        let cfg = enabled_cfg();
        let d = decide_nudge(
            &cfg,
            Some(MACHINE_PHRASE),
            true,
            &NudgeRecord::default(),
            Utc::now(),
        );
        assert_eq!(d, NudgeDecision::Skip(SkipReason::HasLiveChildren));
    }

    #[test]
    fn enabled_and_parking_and_idle_nudges() {
        // All rails pass on a never-nudged, parked, child-free session with a
        // machine-wait phrase.
        let cfg = enabled_cfg();
        let d = decide_nudge(
            &cfg,
            Some(MACHINE_PHRASE),
            false,
            &NudgeRecord::default(),
            Utc::now(),
        );
        assert_eq!(d, NudgeDecision::Nudge);
        assert!(d.should_nudge());
    }

    #[test]
    fn cap_blocks_after_max() {
        // Once `count` reaches the cap, no further nudge — a stuck session is
        // never spammed in a loop.
        let cfg = enabled_cfg(); // max = 2
        let now = Utc::now();
        // count == max → blocked.
        let rec = NudgeRecord {
            count: 2,
            // Old enough that the cooldown would otherwise have elapsed, proving
            // the cap — not the cooldown — is what blocks here.
            last_nudge_at: Some(now - Duration::seconds(10_000)),
        };
        assert_eq!(
            decide_nudge(&cfg, Some(MACHINE_PHRASE), false, &rec, now),
            NudgeDecision::Skip(SkipReason::CapExhausted)
        );
        // count just under the cap → still allowed (cooldown elapsed).
        let rec = NudgeRecord {
            count: 1,
            last_nudge_at: Some(now - Duration::seconds(10_000)),
        };
        assert_eq!(
            decide_nudge(&cfg, Some(MACHINE_PHRASE), false, &rec, now),
            NudgeDecision::Nudge
        );
    }

    #[test]
    fn cap_of_zero_disables() {
        // `max_nudges_per_session = 0` must block from the first receipt.
        let mut cfg = enabled_cfg();
        cfg.max_nudges_per_session = 0;
        assert_eq!(
            decide_nudge(
                &cfg,
                Some(MACHINE_PHRASE),
                false,
                &NudgeRecord::default(),
                Utc::now()
            ),
            NudgeDecision::Skip(SkipReason::CapExhausted)
        );
    }

    #[test]
    fn cooldown_blocks_within_window() {
        // A nudge that fired 60 s ago (< 300 s cooldown) must suppress a second.
        let cfg = enabled_cfg();
        let now = Utc::now();
        let rec = NudgeRecord {
            count: 1,
            last_nudge_at: Some(now - Duration::seconds(60)),
        };
        assert_eq!(
            decide_nudge(&cfg, Some(MACHINE_PHRASE), false, &rec, now),
            NudgeDecision::Skip(SkipReason::WithinCooldown)
        );
    }

    #[test]
    fn nudge_allowed_after_cooldown() {
        // Past the cooldown (and under the cap) the next nudge is allowed.
        let cfg = enabled_cfg();
        let now = Utc::now();
        let rec = NudgeRecord {
            count: 1,
            last_nudge_at: Some(now - Duration::seconds(301)),
        };
        assert_eq!(
            decide_nudge(&cfg, Some(MACHINE_PHRASE), false, &rec, now),
            NudgeDecision::Nudge
        );
    }

    #[test]
    fn ledger_snapshot_defaults_when_unseen() {
        let ledger = NudgeLedger::new();
        let rec = ledger.snapshot(Uuid::new_v4());
        assert_eq!(rec, NudgeRecord::default());
        assert_eq!(rec.count, 0);
        assert!(rec.last_nudge_at.is_none());
    }

    #[test]
    fn ledger_record_bumps_count() {
        let mut ledger = NudgeLedger::new();
        let id = Uuid::new_v4();
        let t0 = Utc::now();
        ledger.record_nudge(id, t0);
        let rec = ledger.snapshot(id);
        assert_eq!(rec.count, 1);
        assert_eq!(rec.last_nudge_at, Some(t0));
        let t1 = t0 + Duration::seconds(500);
        ledger.record_nudge(id, t1);
        let rec = ledger.snapshot(id);
        assert_eq!(rec.count, 2);
        assert_eq!(rec.last_nudge_at, Some(t1));
    }

    #[test]
    fn ledger_prune_drops_stale() {
        let mut ledger = NudgeLedger::new();
        let live_id = Uuid::new_v4();
        let stale_id = Uuid::new_v4();
        let now = Utc::now();
        ledger.record_nudge(live_id, now);
        ledger.record_nudge(stale_id, now);
        let live = std::collections::HashSet::from([live_id]);
        ledger.prune(&live);
        assert_eq!(ledger.snapshot(live_id).count, 1, "live entry survives");
        assert_eq!(
            ledger.snapshot(stale_id),
            NudgeRecord::default(),
            "stale entry pruned back to default"
        );
    }

    /// End-to-end rail ordering: the full happy path then each rail's override,
    /// proving `decide_nudge` short-circuits at the first failing rail —
    /// Disabled > NotParking > HumanWait > HasLiveChildren > cap/cooldown.
    #[test]
    fn rails_short_circuit_in_order() {
        let cfg = enabled_cfg();
        let now = Utc::now();
        let exhausted = NudgeRecord {
            count: 99,
            last_nudge_at: Some(now),
        };

        // Disabled beats everything, even a human-addressed phrase.
        let mut disabled = cfg.clone();
        disabled.enabled = false;
        assert_eq!(
            decide_nudge(&disabled, Some(HUMAN_PHRASE), true, &exhausted, now),
            NudgeDecision::Skip(SkipReason::Disabled)
        );
        // Enabled but no phrase at all, with live children + exhausted cap →
        // NotParking wins over the later rails.
        assert_eq!(
            decide_nudge(&cfg, None, true, &exhausted, now),
            NudgeDecision::Skip(SkipReason::NotParking)
        );
        // Enabled, a human-addressed phrase, with live children + exhausted cap
        // → HumanWait wins over the later rails.
        assert_eq!(
            decide_nudge(&cfg, Some(HUMAN_PHRASE), true, &exhausted, now),
            NudgeDecision::Skip(SkipReason::HumanWait)
        );
        // Enabled, machine-wait phrase, live children, exhausted cap →
        // HasLiveChildren wins over the cap rail.
        assert_eq!(
            decide_nudge(&cfg, Some(MACHINE_PHRASE), true, &exhausted, now),
            NudgeDecision::Skip(SkipReason::HasLiveChildren)
        );
    }
}
