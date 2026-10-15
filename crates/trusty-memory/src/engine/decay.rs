//! Temporal decay: importance and confidence decrease with age unless refreshed.
//!
//! Why: Stale facts should not crowd out fresh ones. Exponential decay with a
//! configurable half-life naturally de-weights old memories without deletion.
//! What: `DecayConfig` + pure `effective_importance` function + KG triple sorting helper.
//! Test: See unit tests — half-life at 90 days, floor clamping, access boost.

use chrono::{DateTime, Utc};

/// Configuration for temporal decay of drawer importance and KG confidence.
#[derive(Debug, Clone)]
pub struct DecayConfig {
    /// Days for importance to halve. Default: 90.0
    pub half_life_days: f32,
    /// Minimum effective importance (floor). Default: 0.05
    pub floor: f32,
    /// Importance added per recall hit. Default: 0.05
    pub access_boost: f32,
    /// Maximum accumulated access boost. Default: 0.3
    pub access_boost_cap: f32,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            half_life_days: 90.0,
            floor: 0.05,
            access_boost: 0.05,
            access_boost_cap: 0.3,
        }
    }
}

impl DecayConfig {
    /// Compute effective importance after age-based decay and access boost.
    ///
    /// Why: Effective importance drives L1 selection and L2/L3 ranking; baking
    /// in decay here keeps retrieval code free of time arithmetic.
    /// What: `(base * 2^(-age_days / half_life) + boost).clamp(floor, 1.0)`
    /// Test: See unit tests — half-life, floor, boost all verified.
    pub fn effective_importance(&self, base: f32, age_days: f32, accumulated_boost: f32) -> f32 {
        let decayed = base * 2f32.powf(-age_days / self.half_life_days);
        let boost = accumulated_boost.min(self.access_boost_cap);
        (decayed + boost).clamp(self.floor, 1.0)
    }

    /// Age in fractional days from `created_at` to now.
    ///
    /// Why: Centralizes time-arithmetic so callers pass a single `f32` into
    /// `effective_importance` without juggling `Duration` arithmetic.
    /// What: Returns `(now - created_at).seconds / 86_400`, clamped to >= 0.
    /// Test: A fresh `Utc::now()` returns ~0; a timestamp from yesterday returns ~1.0.
    pub fn age_days(created_at: DateTime<Utc>) -> f32 {
        let elapsed = Utc::now().signed_duration_since(created_at);
        elapsed.num_seconds().max(0) as f32 / 86_400.0
    }

    /// Effective confidence for a KG triple (same decay formula, no boost).
    ///
    /// Why: KG triples should fade with age just like drawers, but they have no
    /// access-boost concept — confidence simply decays toward `floor`.
    /// What: `(base * 2^(-age_days / half_life)).clamp(floor, 1.0)`
    /// Test: See `kg_triple_effective_confidence` — base 1.0 at half-life ~= 0.5.
    pub fn effective_confidence(&self, base: f32, age_days: f32) -> f32 {
        let decayed = base * 2f32.powf(-age_days / self.half_life_days);
        decayed.clamp(self.floor, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_decay_at_age_zero() {
        let cfg = DecayConfig::default();
        let eff = cfg.effective_importance(0.8, 0.0, 0.0);
        assert!((eff - 0.8).abs() < 1e-4, "got {eff}");
    }

    #[test]
    fn half_life_at_90_days() {
        let cfg = DecayConfig::default();
        let eff = cfg.effective_importance(0.8, 90.0, 0.0);
        assert!((eff - 0.4).abs() < 1e-3, "expected ~0.4 got {eff}");
    }

    #[test]
    fn floor_clamps_minimum() {
        let cfg = DecayConfig::default();
        let eff = cfg.effective_importance(0.1, 365.0, 0.0);
        assert_eq!(eff, cfg.floor, "should be floored at {}", cfg.floor);
    }

    #[test]
    fn access_boost_applied() {
        let cfg = DecayConfig::default();
        // 0.5 base + 0.3 boost = 0.8 (no decay at age 0)
        let eff = cfg.effective_importance(0.5, 0.0, 0.3);
        assert!((eff - 0.8).abs() < 1e-4, "got {eff}");
    }

    #[test]
    fn access_boost_capped() {
        let cfg = DecayConfig::default();
        // boost capped at 0.3 even if we pass more
        let eff_capped = cfg.effective_importance(0.5, 0.0, 1.0);
        let eff_at_cap = cfg.effective_importance(0.5, 0.0, 0.3);
        assert!((eff_capped - eff_at_cap).abs() < 1e-4);
    }

    #[test]
    fn drawer_accumulated_boost() {
        use super::super::palace::Drawer;
        use uuid::Uuid;
        let cfg = DecayConfig::default();
        let mut d = Drawer::new(Uuid::new_v4(), "test");
        assert_eq!(d.accumulated_boost(&cfg), 0.0);
        d.record_access();
        d.record_access();
        // 2 * 0.05 = 0.10
        assert!((d.accumulated_boost(&cfg) - 0.10).abs() < 1e-4);
    }

    #[test]
    fn kg_triple_effective_confidence() {
        let cfg = DecayConfig::default();
        let eff = cfg.effective_confidence(1.0, 90.0);
        assert!((eff - 0.5).abs() < 1e-3, "expected ~0.5 got {eff}");
    }
}
