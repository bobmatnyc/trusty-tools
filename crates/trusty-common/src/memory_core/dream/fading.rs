//! Fading-memories resurface pass (issue #2352).
//!
//! Why: Ebbinghaus decay (`decay.rs`) silently pushes originally-important but
//! un-recalled memories toward the floor. Auto-boosting them would defeat the
//! forgetting curve, so instead we make them *visible*: each dream cycle
//! collects a ranked list of high-value memories whose effective importance has
//! fallen below a threshold, and surfaces it so a human or agent can decide to
//! touch (natural recall re-boosts) or `memory_forget` them.
//! What: `FadingParams` (tunables), `FadingMemory` (one surfaced entry), the
//! pure `rank_fading` detector over a drawer slice, and `detect_fading` which
//! snapshots a `PalaceHandle` and applies its `DecayConfig`.
//! Test: `tests` submodule below — flags a high-base aged unaccessed drawer,
//! rejects fresh / low-base / boosted / protected drawers, verifies ranking +
//! top-N cap.

use crate::memory_core::decay::DecayConfig;
use crate::memory_core::palace::Drawer;
use crate::memory_core::retrieval::PalaceHandle;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// Max characters of drawer content included in a surfaced `FadingMemory`.
///
/// Why: The resurface list is a decision aid, not a full dump — a short preview
/// is enough for an operator to recognise the memory without bloating the MCP
/// response or the persisted `dream_stats.json`.
/// What: Content longer than this is truncated on a char boundary and suffixed
/// with an ellipsis.
/// Test: `preview_truncates_long_content`.
const PREVIEW_MAX_CHARS: usize = 120;

/// Tunables for the fading-memories resurface pass.
///
/// Why: The detection thresholds must be configurable per deployment (and,
/// via `DreamConfig`, per palace) without recompiling. Embedded in `DreamConfig`
/// exactly like `semantic: SemanticConsolidationConfig` so the dream loop owns a
/// single config surface.
/// What: `resurface_min_base` gates on original importance; `resurface_threshold`
/// gates on decayed effective importance; `resurface_boost_epsilon` approximates
/// "not accessed recently"; `resurface_top_n` caps the ranked output.
/// Test: `default_params_match_spec`.
#[derive(Debug, Clone, PartialEq)]
pub struct FadingParams {
    /// Only memories whose *base* importance is at least this are candidates.
    /// Default: 0.7 (we only care about resurfacing originally high-value ones).
    pub resurface_min_base: f32,
    /// A candidate is fading when its *effective* importance has fallen below
    /// this. Default: 0.3.
    pub resurface_threshold: f32,
    /// Accumulated access boost below which a drawer is treated as "not accessed
    /// recently". Access recency is not tracked as an independently-decaying
    /// signal, so we approximate it via the accumulated boost: a value below
    /// this epsilon means essentially no recall reinforcement. Default: 0.01.
    pub resurface_boost_epsilon: f32,
    /// Cap on the number of ranked entries returned. Default: 10.
    pub resurface_top_n: usize,
}

impl Default for FadingParams {
    fn default() -> Self {
        Self {
            resurface_min_base: 0.7,
            resurface_threshold: 0.3,
            resurface_boost_epsilon: 0.01,
            resurface_top_n: 10,
        }
    }
}

/// One high-value memory that has decayed below the resurface threshold.
///
/// Why: The dream cycle emits these so operators/agents can decide to touch
/// (recall re-boosts) or forget them — without the cycle auto-boosting and
/// defeating the forgetting curve. Serialised into `dream_stats.json` and the
/// `palace_dream` MCP response.
/// What: The drawer id, its base and current effective importance, its age in
/// days, and a short content preview.
/// Test: `flags_high_base_aged_unaccessed_drawer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FadingMemory {
    /// Id of the fading drawer (use with `memory_forget` / recall).
    pub drawer_id: Uuid,
    /// Original (undecayed) importance the memory was written with.
    pub base_importance: f32,
    /// Current effective importance after decay + boost.
    pub effective_importance: f32,
    /// Age of the memory in fractional days.
    pub age_days: f32,
    /// Short preview of the drawer content (see `PREVIEW_MAX_CHARS`).
    pub content_preview: String,
}

/// Truncate `content` to at most `PREVIEW_MAX_CHARS` characters, char-safe.
///
/// Why: Keep previews bounded without panicking on multi-byte boundaries.
/// What: Returns the content unchanged when short; otherwise the first
/// `PREVIEW_MAX_CHARS` chars plus an ellipsis.
/// Test: `preview_truncates_long_content`.
fn preview(content: &str) -> String {
    if content.chars().count() <= PREVIEW_MAX_CHARS {
        return content.to_string();
    }
    let truncated: String = content.chars().take(PREVIEW_MAX_CHARS).collect();
    format!("{truncated}…")
}

/// Detect fading high-value memories in a drawer slice (pure).
///
/// Why: The core detection math is factored out of any handle/IO so it can be
/// unit-tested exhaustively and reused by both the idle dream cycle and the
/// on-demand `palace_dream` path.
/// What: Selects non-protected drawers where `base >= resurface_min_base`,
/// `effective_importance < resurface_threshold`, and `accumulated_boost <
/// resurface_boost_epsilon` (the "not accessed recently" approximation), ranks
/// them by base importance desc then age desc, and truncates to
/// `resurface_top_n`.
/// Test: `flags_high_base_aged_unaccessed_drawer`, `rejects_fresh_drawer`,
/// `rejects_low_base_drawer`, `rejects_recently_boosted_drawer`,
/// `rejects_protected_task_drawer`, `ranks_and_caps`.
pub fn rank_fading(
    drawers: &[Drawer],
    decay: &DecayConfig,
    params: &FadingParams,
) -> Vec<FadingMemory> {
    if params.resurface_top_n == 0 {
        return Vec::new();
    }

    let mut fading: Vec<FadingMemory> = drawers
        .iter()
        .filter(|d| !d.drawer_type.is_protected())
        .filter(|d| d.importance >= params.resurface_min_base)
        .filter_map(|d| {
            let age_days = DecayConfig::age_days(d.created_at);
            let boost = d.accumulated_boost(decay);
            // "Not accessed recently" is approximated by near-zero accumulated
            // boost: we only resurface memories that received essentially no
            // recall reinforcement.
            if boost >= params.resurface_boost_epsilon {
                return None;
            }
            let effective = decay.effective_importance(d.importance, age_days, boost);
            if effective >= params.resurface_threshold {
                return None;
            }
            Some(FadingMemory {
                drawer_id: d.id,
                base_importance: d.importance,
                effective_importance: effective,
                age_days,
                content_preview: preview(&d.content),
            })
        })
        .collect();

    // Rank: highest base importance first; break ties by oldest (largest age).
    fading.sort_by(|a, b| {
        b.base_importance
            .partial_cmp(&a.base_importance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                b.age_days
                    .partial_cmp(&a.age_days)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    fading.truncate(params.resurface_top_n);
    fading
}

/// Detect fading memories for a live palace handle.
///
/// Why: The dream cycle and the `palace_dream` MCP handler both need a
/// palace-wide fading list computed against that palace's own `DecayConfig`
/// (per-palace override); this snapshots the drawer table and delegates to the
/// pure `rank_fading`.
/// What: Reads `handle.drawers`, applies `handle.decay_config`, and returns the
/// ranked, capped list. Read-only — never mutates drawers (no auto-boost).
/// Test: covered via `rank_fading` unit tests (pure core) and the
/// `palace_dream` MCP response-shape test in trusty-memory.
pub fn detect_fading(handle: &Arc<PalaceHandle>, params: &FadingParams) -> Vec<FadingMemory> {
    let snapshot = handle.drawers.read().clone();
    rank_fading(&snapshot, &handle.decay_config, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_core::palace::DrawerType;

    /// Build a drawer with an explicit importance and created-at age in days.
    fn aged_drawer(importance: f32, age_days: i64) -> Drawer {
        let mut d = Drawer::new(Uuid::new_v4(), "an important thing worth remembering");
        d.importance = importance;
        d.created_at = chrono::Utc::now() - chrono::Duration::days(age_days);
        d
    }

    #[test]
    fn default_params_match_spec() {
        let p = FadingParams::default();
        assert_eq!(p.resurface_min_base, 0.7);
        assert_eq!(p.resurface_threshold, 0.3);
        assert_eq!(p.resurface_top_n, 10);
    }

    #[test]
    fn flags_high_base_aged_unaccessed_drawer() {
        let decay = DecayConfig::default();
        let params = FadingParams::default();
        // base 0.8, aged 360d: eff = 0.8 * 2^(-360/90) = 0.8 * 0.0625 = 0.05
        // which is < 0.3 threshold; no access boost.
        let d = aged_drawer(0.8, 360);
        let out = rank_fading(&[d], &decay, &params);
        assert_eq!(out.len(), 1, "high-base aged unaccessed drawer should flag");
        assert!(out[0].effective_importance < params.resurface_threshold);
        assert_eq!(out[0].base_importance, 0.8);
    }

    #[test]
    fn rejects_fresh_drawer() {
        let decay = DecayConfig::default();
        let params = FadingParams::default();
        // Fresh: eff ~= base 0.8, well above threshold.
        let d = aged_drawer(0.8, 0);
        assert!(rank_fading(&[d], &decay, &params).is_empty());
    }

    #[test]
    fn rejects_low_base_drawer() {
        let decay = DecayConfig::default();
        let params = FadingParams::default();
        // Low base (0.5 < 0.7 min_base) even though it has decayed.
        let d = aged_drawer(0.5, 360);
        assert!(rank_fading(&[d], &decay, &params).is_empty());
    }

    #[test]
    fn rejects_recently_boosted_drawer() {
        let decay = DecayConfig::default();
        let params = FadingParams::default();
        let mut d = aged_drawer(0.8, 360);
        // Simulate recall reinforcement: accumulated_boost >= epsilon.
        d.record_access();
        assert!(
            d.accumulated_boost(&decay) >= params.resurface_boost_epsilon,
            "one access should exceed the tiny epsilon"
        );
        assert!(
            rank_fading(&[d], &decay, &params).is_empty(),
            "a reinforced drawer is not fading"
        );
    }

    #[test]
    fn rejects_protected_task_drawer() {
        let decay = DecayConfig::default();
        let params = FadingParams::default();
        let mut d = aged_drawer(0.9, 360);
        d.drawer_type = DrawerType::Task;
        assert!(
            rank_fading(&[d], &decay, &params).is_empty(),
            "protected Task drawers are never resurfaced"
        );
    }

    #[test]
    fn ranks_and_caps() {
        let decay = DecayConfig::default();
        let params = FadingParams {
            resurface_top_n: 2,
            ..FadingParams::default()
        };
        // Three fading candidates with different base importances.
        let a = aged_drawer(0.75, 360);
        let b = aged_drawer(0.95, 360);
        let c = aged_drawer(0.85, 360);
        let out = rank_fading(&[a, b, c], &decay, &params);
        assert_eq!(out.len(), 2, "capped at top_n");
        assert_eq!(out[0].base_importance, 0.95, "highest base first");
        assert_eq!(out[1].base_importance, 0.85);
    }

    #[test]
    fn top_n_zero_returns_empty() {
        let decay = DecayConfig::default();
        let params = FadingParams {
            resurface_top_n: 0,
            ..FadingParams::default()
        };
        let d = aged_drawer(0.8, 360);
        assert!(rank_fading(&[d], &decay, &params).is_empty());
    }

    #[test]
    fn preview_truncates_long_content() {
        let long = "x".repeat(PREVIEW_MAX_CHARS + 50);
        let p = preview(&long);
        assert_eq!(p.chars().count(), PREVIEW_MAX_CHARS + 1, "chars + ellipsis");
        assert!(p.ends_with('…'));
    }
}
