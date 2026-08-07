//! Relevance floor for recall results (#5037).
//!
//! Why: every retrieval path in [`super::layers`] ends in `truncate(top_k)` and
//! nothing else — `top_k` is the only length control, so a query with no good
//! answer still returns a full `top_k` of whatever ranked highest. This module
//! is the missing half: a minimum score a candidate must clear to be shown at
//! all, plus a count of what was dropped so the caller can say so out loud.
//! What: [`DEFAULT_RELEVANCE_FLOOR`], [`FloorOutcome`], and
//! [`apply_relevance_floor`]. Pure — no I/O, no allocation beyond the returned
//! vector.
//! Test: `relevance_floor_drops_l1_penalty_noise`,
//! `relevance_floor_keeps_genuine_hit`, `relevance_floor_keeps_unscored_items`,
//! `relevance_floor_counts_withheld`.
//!
//! Lives beside `layers.rs` rather than inside it because `layers.rs` sits at
//! 464 of its 500 SLOC budget; the repo's cap forces the split
//! (`docs/reference/sloc-cap.md`).

/// Minimum score a recall candidate must reach to be worth showing.
///
/// Why: `rescore_l1_by_similarity` assigns an L1 drawer the HNSW search never
/// returned a score of `importance * L1_NO_SIMILARITY_PENALTY` — at most
/// `0.15`. That is a nonzero score, so it competes for a `top_k` slot and
/// renders identically to a real hit. Measured against the live `trusty-tools`
/// palace (1,332 drawers) over four query populations:
///
/// | population | n | min | median | max |
/// |---|---|---|---|---|
/// | 15 off-topic prompts × top-10 ("capital of France", "go", "ok", …) | 150 | 0.1500 | 0.1510 | **0.3439** |
/// | 60 self-retrieval queries, score of the *correct* drawer | 57 | **0.4844** | 0.6950 | 0.9743 |
/// | 120 real logged hook prompts × top-10 | 1200 | 0.4042 | 0.5422 | 0.7527 |
/// | 12 paraphrased on-topic questions, best hit per query | 12 | 0.3293 | 0.4317 | 0.5359 |
///
/// 75 of the 150 off-topic candidates scored *exactly* 0.1500 — the L1 penalty
/// itself. The noise and signal distributions are nearly disjoint, and the gap
/// between them (0.3439 … 0.4042) is where this constant sits.
/// What: `0.35` — the smallest swept value at which zero off-topic candidates
/// survive, rounded up from the highest observed noise score (0.3439). At this
/// floor the real logged corpus loses 0 of 1200 candidates and 0 of 120
/// queries; self-retrieval keeps 57 of 57 correct drawers; 10 of 12 paraphrased
/// questions still return something. The 2 that stop returning are short
/// keyword-bearing queries ("what is the MSRV for this workspace", 0.3293),
/// which is dense retrieval's known weak spot and BM25's strength — #5036 is
/// the named fix for that class. Their loss is reported, never silent: see
/// [`FloorOutcome::withheld`].
/// Test: `relevance_floor_drops_l1_penalty_noise` pins the 0.15 case;
/// `relevance_floor_keeps_genuine_hit` pins the 0.56 case.
pub const DEFAULT_RELEVANCE_FLOOR: f32 = 0.35;

/// What cleared a relevance floor, and how much did not.
///
/// Why: dropping weak candidates silently turns "we found nothing good" into
/// something indistinguishable from "this palace is empty". Carrying the count
/// alongside the survivors is what lets a caller render the difference.
/// What: `kept` in the input's order; `withheld` is how many were dropped.
/// Test: `relevance_floor_counts_withheld`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloorOutcome<T> {
    /// Candidates at or above the floor, in input order.
    pub kept: Vec<T>,
    /// How many candidates fell below the floor.
    pub withheld: usize,
}

impl<T> FloorOutcome<T> {
    /// True when the floor dropped everything it was given a score for.
    ///
    /// Why: this is the case a caller must announce rather than render as an
    /// empty section — see [`FloorOutcome`].
    /// What: `kept.is_empty() && withheld > 0`. A run with nothing to judge
    /// (`withheld == 0`) is "nothing existed", not "everything was withheld".
    /// Test: `relevance_floor_counts_withheld`.
    pub fn silenced(&self) -> bool {
        self.kept.is_empty() && self.withheld > 0
    }
}

/// Drop candidates scoring below `floor`, counting what went.
///
/// Why: the one implementation of "below the floor, it is not shown" — the
/// prompt-context injection and any future recall consumer must agree on the
/// comparison and on how an unscored candidate is treated, or the two drift.
/// What: keeps every item whose `score_of` is `>= floor`. `score_of` returning
/// `None` means the score is unknown, and an unknown-score item is **kept** and
/// not counted as withheld — dropping what cannot be judged would let a wire
/// format that stops emitting `score` silently empty the result set. A
/// non-positive or NaN `floor` disables the gate entirely.
/// Test: `relevance_floor_drops_l1_penalty_noise`,
/// `relevance_floor_keeps_genuine_hit`, `relevance_floor_keeps_unscored_items`,
/// `relevance_floor_counts_withheld`.
pub fn apply_relevance_floor<T>(
    items: Vec<T>,
    floor: f32,
    score_of: impl Fn(&T) -> Option<f32>,
) -> FloorOutcome<T> {
    if floor.is_nan() || floor <= 0.0 {
        return FloorOutcome {
            kept: items,
            withheld: 0,
        };
    }
    let mut kept = Vec::with_capacity(items.len());
    let mut withheld = 0usize;
    for item in items {
        match score_of(&item) {
            Some(s) if s < floor => withheld += 1,
            _ => kept.push(item),
        }
    }
    FloorOutcome { kept, withheld }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#5037): a probe of "what is the capital of France" returned five
    /// drawers all scoring exactly 0.15 — `L1_NO_SIMILARITY_PENALTY` — and they
    /// rendered identically to a real hit. The floor exists to make that set
    /// empty.
    /// What: five 0.15 candidates against the default floor.
    /// Test: itself.
    #[test]
    fn relevance_floor_drops_l1_penalty_noise() {
        let noise = vec![0.15_f32; 5];
        let out = apply_relevance_floor(noise, DEFAULT_RELEVANCE_FLOOR, |s| Some(*s));
        assert!(out.kept.is_empty(), "0.15 noise must not survive the floor");
        assert_eq!(out.withheld, 5);
        assert!(out.silenced(), "all-withheld must be announceable");
    }

    /// Why (#5037): the floor must not be so high that it eats real answers.
    /// 0.56 is the measured median of the self-retrieval signal population.
    /// What: one genuine hit plus four noise candidates.
    /// Test: itself.
    #[test]
    fn relevance_floor_keeps_genuine_hit() {
        let mixed = vec![0.56_f32, 0.15, 0.15, 0.15, 0.15];
        let out = apply_relevance_floor(mixed, DEFAULT_RELEVANCE_FLOOR, |s| Some(*s));
        assert_eq!(out.kept, vec![0.56_f32]);
        assert_eq!(out.withheld, 4);
        assert!(!out.silenced(), "a surviving hit is not a silenced recall");
    }

    /// Why: an unscored candidate is unjudgeable, and dropping it would let a
    /// wire format that stops emitting `score` empty the injection silently —
    /// the fail-open/fail-closed inversion this fix exists to prevent.
    /// What: `score_of` returns `None`.
    /// Test: itself.
    #[test]
    fn relevance_floor_keeps_unscored_items() {
        let out = apply_relevance_floor(vec!["a", "b"], DEFAULT_RELEVANCE_FLOOR, |_| None);
        assert_eq!(out.kept.len(), 2);
        assert_eq!(out.withheld, 0);
        assert!(!out.silenced());
    }

    /// Why: `withheld` is the input to requirement 4's "more" signal, so its
    /// arithmetic is load-bearing, including the disabled-gate case.
    /// What: partial drop, empty input, and a zero floor.
    /// Test: itself.
    #[test]
    fn relevance_floor_counts_withheld() {
        let out = apply_relevance_floor(vec![0.9_f32, 0.2, 0.4, 0.1], 0.35, |s| Some(*s));
        assert_eq!(out.kept, vec![0.9_f32, 0.4]);
        assert_eq!(out.withheld, 2);

        let empty = apply_relevance_floor(Vec::<f32>::new(), 0.35, |s| Some(*s));
        assert_eq!(empty.withheld, 0);
        assert!(
            !empty.silenced(),
            "nothing to judge is not the same as everything withheld"
        );

        let off = apply_relevance_floor(vec![0.01_f32, 0.02], 0.0, |s| Some(*s));
        assert_eq!(off.kept.len(), 2, "a zero floor must disable the gate");
        assert_eq!(off.withheld, 0);
    }
}
