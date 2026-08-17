//! Bounded logical-order reconciliation for durable-memory turn outcomes
//! (#2425).
//!
//! Why: queue-full outcomes are reported synchronously by
//! `TurnMemorySink::enqueue`, while accepted turns finish later in the detached
//! FIFO drain. Arrival order can therefore be `D2, D3, D1` even though
//! durability status is defined over QUEUED-TURN order — and folding outcomes
//! in arrival order lets an older durable completion erase a newer dropped
//! turn's streak, or skip a warning threshold entirely. Keeping every early
//! outcome in a map instead would make a stuck drain plus sustained queue-full
//! load an unbounded in-process memory leak.
//! What: adjacent equal outcomes are stored as compact sequence RUNS and folded
//! only when the next logical sequence is available. The default sink has at
//! most `QUEUE_CAPACITY` queued turns plus one in the drain, and each pair of
//! pending synchronous-failure runs must be separated by one such accepted
//! turn, so at most `QUEUE_CAPACITY + 2` runs are reachable. That bound is
//! enforced structurally even if a caller violates those sink-ordering
//! invariants: the excess outcome is REFUSED, and the refusal is reported to
//! the caller rather than silently dropped — see
//! `SessionRegistry::record_memory_durability`.
//! Test: `registry_tests::two_out_of_order_degradations_fold_in_logical_sequence`,
//! `registry_tests::three_out_of_order_degradations_emit_every_crossed_warning_threshold`,
//! `registry_tests::outcome_beyond_the_reorder_bound_is_counted_as_unrecorded`.

use chrono::{DateTime, Utc};

use super::memory_sink::{MemoryFailureCategory, MemoryTurnOutcome, QUEUE_CAPACITY};
use super::transcript::MemoryDurabilityStatus;

/// The most pending runs reachable under the sink's own ordering invariants.
const MAX_PENDING_RUNS: usize = QUEUE_CAPACITY + 2;

/// One streak threshold this fold crossed, for the caller to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MemoryDurabilityWarning {
    pub category: MemoryFailureCategory,
    pub consecutive: u32,
}

/// A maximal run of adjacent sequences sharing one verdict.
#[derive(Debug)]
enum PendingRun {
    Durable {
        start: u64,
        end: u64,
    },
    Degraded {
        start: u64,
        end: u64,
        category: MemoryFailureCategory,
        latest_at: DateTime<Utc>,
    },
}

impl PendingRun {
    fn start(&self) -> u64 {
        match self {
            Self::Durable { start, .. } | Self::Degraded { start, .. } => *start,
        }
    }

    fn end(&self) -> u64 {
        match self {
            Self::Durable { end, .. } | Self::Degraded { end, .. } => *end,
        }
    }

    fn contains(&self, sequence: u64) -> bool {
        self.start() <= sequence && sequence <= self.end()
    }

    /// Fuse `self` with the run that immediately follows it, or hand both back.
    ///
    /// Why: compaction is what makes the bound hold — without it, sustained
    /// queue-full load under a stuck drain would add one entry per turn.
    /// What: only adjacent runs of the SAME verdict (and, when degraded, the
    /// same category) merge; the merged run keeps the later `latest_at`, which
    /// is the one the status reports.
    fn merge(self, right: Self) -> Result<Self, (Self, Self)> {
        if self.end().checked_add(1) != Some(right.start()) {
            return Err((self, right));
        }
        match (self, right) {
            (Self::Durable { start, .. }, Self::Durable { end, .. }) => {
                Ok(Self::Durable { start, end })
            }
            (
                Self::Degraded {
                    start, category, ..
                },
                Self::Degraded {
                    end,
                    category: right_category,
                    latest_at,
                    ..
                },
            ) if category == right_category => Ok(Self::Degraded {
                start,
                end,
                category,
                latest_at,
            }),
            (left, right) => Err((left, right)),
        }
    }
}

/// Folds out-of-order turn outcomes back into queued-turn order (#2425).
#[derive(Debug)]
pub(super) struct MemoryOutcomeReconciler {
    next_sequence: u64,
    pending: Vec<PendingRun>,
}

impl Default for MemoryOutcomeReconciler {
    fn default() -> Self {
        Self {
            // `TurnMemorySink::enqueue` numbers its first turn 1.
            next_sequence: 1,
            pending: Vec::new(),
        }
    }
}

impl MemoryOutcomeReconciler {
    /// Fold one outcome into `status`, returning the streak thresholds it
    /// crossed.
    ///
    /// Why: the status must reflect LOGICAL turn order, so an outcome that
    /// arrives before its predecessor is parked rather than applied.
    /// `total_failed_turns` is the exception — it is order-free, so a degraded
    /// outcome counts the moment it arrives, whether or not it can fold yet.
    /// What: `Ok(warnings)` on success, possibly empty. `Err(())` when the
    /// pending-run bound would be exceeded; the outcome is then NOT applied and
    /// NOT counted, which is why the caller must record the refusal rather than
    /// discard it. A sequence already folded or already pending is idempotently
    /// ignored.
    /// Test: `registry_tests::mixed_out_of_order_memory_outcomes_preserve_the_newest_logical_state`,
    /// `registry_tests::outcome_beyond_the_reorder_bound_is_counted_as_unrecorded`.
    pub(super) fn observe(
        &mut self,
        status: &mut MemoryDurabilityStatus,
        outcome: MemoryTurnOutcome,
    ) -> Result<Vec<MemoryDurabilityWarning>, ()> {
        let sequence = outcome_sequence(&outcome);
        if sequence < self.next_sequence || self.pending.iter().any(|run| run.contains(sequence)) {
            return Ok(Vec::new());
        }

        if sequence == self.next_sequence {
            if matches!(outcome, MemoryTurnOutcome::Degraded { .. }) {
                status.total_failed_turns = status.total_failed_turns.saturating_add(1);
            }
            let mut warnings = Vec::new();
            self.fold(status, outcome.into(), &mut warnings);
            self.fold_ready(status, &mut warnings);
            return Ok(warnings);
        }

        let degraded = matches!(outcome, MemoryTurnOutcome::Degraded { .. });
        self.insert(outcome.into())?;
        if degraded {
            status.total_failed_turns = status.total_failed_turns.saturating_add(1);
        }
        Ok(Vec::new())
    }

    /// Park a run that cannot fold yet, refusing it if the bound is reached.
    fn insert(&mut self, run: PendingRun) -> Result<(), ()> {
        let position = self
            .pending
            .partition_point(|existing| existing.start() < run.start());
        self.pending.insert(position, run);
        self.compact();
        if self.pending.len() > MAX_PENDING_RUNS {
            self.pending.remove(position.min(self.pending.len() - 1));
            return Err(());
        }
        Ok(())
    }

    fn compact(&mut self) {
        let mut index = 0;
        while index + 1 < self.pending.len() {
            let left = self.pending.remove(index);
            let right = self.pending.remove(index);
            match left.merge(right) {
                Ok(merged) => self.pending.insert(index, merged),
                Err((left, right)) => {
                    self.pending.insert(index, right);
                    self.pending.insert(index, left);
                    index += 1;
                }
            }
        }
    }

    fn fold_ready(
        &mut self,
        status: &mut MemoryDurabilityStatus,
        warnings: &mut Vec<MemoryDurabilityWarning>,
    ) {
        while self
            .pending
            .first()
            .is_some_and(|run| run.start() == self.next_sequence)
        {
            let run = self.pending.remove(0);
            self.fold(status, run, warnings);
        }
    }

    /// Apply one run at the head of logical order.
    ///
    /// Why: a run of N degraded turns can cross BOTH warning thresholds at
    /// once, so the thresholds are tested against the streak's before/after
    /// span rather than incremented one turn at a time.
    fn fold(
        &mut self,
        status: &mut MemoryDurabilityStatus,
        run: PendingRun,
        warnings: &mut Vec<MemoryDurabilityWarning>,
    ) {
        debug_assert_eq!(run.start(), self.next_sequence);
        let end = run.end();
        match run {
            PendingRun::Durable { .. } => status.consecutive_failed_turns = 0,
            PendingRun::Degraded {
                start,
                end,
                category,
                latest_at,
            } => {
                let previous = status.consecutive_failed_turns;
                let run_len =
                    u32::try_from(end.saturating_sub(start).saturating_add(1)).unwrap_or(u32::MAX);
                let next = previous.saturating_add(run_len);
                for threshold in [1, 3] {
                    if previous < threshold && threshold <= next {
                        warnings.push(MemoryDurabilityWarning {
                            category,
                            consecutive: threshold,
                        });
                    }
                }
                status.consecutive_failed_turns = next;
                status.latest_failure_category = Some(category);
                status.latest_failure_at = Some(latest_at);
            }
        }
        self.next_sequence = end.saturating_add(1);
    }
}

fn outcome_sequence(outcome: &MemoryTurnOutcome) -> u64 {
    match outcome {
        MemoryTurnOutcome::Durable { sequence } | MemoryTurnOutcome::Degraded { sequence, .. } => {
            *sequence
        }
    }
}

impl From<MemoryTurnOutcome> for PendingRun {
    fn from(outcome: MemoryTurnOutcome) -> Self {
        match outcome {
            MemoryTurnOutcome::Durable { sequence } => Self::Durable {
                start: sequence,
                end: sequence,
            },
            MemoryTurnOutcome::Degraded {
                sequence,
                category,
                at,
            } => Self::Degraded {
                start: sequence,
                end: sequence,
                category,
                latest_at: at,
            },
        }
    }
}
