//! Context-window memory-protection model.
//!
//! Why: a Claude Code session that silently runs into its context limit loses
//! work and produces degraded output. trusty-mpm tracks token usage per
//! session and acts at configurable thresholds (warn / alert / auto-compact).
//! Centralizing the threshold math here keeps the daemon, TUI gauge, and
//! Telegram alert in agreement on what "85%" means.
//! What: `MemoryConfig` (the three thresholds), `MemoryUsage` (a token count
//! snapshot), and `MemoryPressure` (the derived warn/alert/compact level).
//! Test: `cargo test -p trusty-mpm-core` checks threshold classification at
//! the boundaries and that the config rejects nonsensical ordering.

use serde::{Deserialize, Serialize};

/// Memory-protection thresholds, as fractions of the context window.
///
/// Why: every deployment may want different headroom; defaults match the
/// task spec (warn 70%, alert 85%, auto-compact 90%).
/// What: three `f32` ratios in `0.0..=1.0`, plus a validity check.
/// Test: `default_config_is_valid` and `rejects_disordered_thresholds`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Fraction at which a non-blocking warning is surfaced.
    pub warn_at: f32,
    /// Fraction at which an alert (Telegram push) fires.
    pub alert_at: f32,
    /// Fraction at which the daemon triggers an automatic compaction.
    pub compact_at: f32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            warn_at: 0.70,
            alert_at: 0.85,
            compact_at: 0.90,
        }
    }
}

impl MemoryConfig {
    /// True if the thresholds are sane: each in `(0,1]` and strictly ordered.
    ///
    /// Why: a misconfigured `compact_at < warn_at` would compact constantly.
    /// What: validates range and `warn_at < alert_at < compact_at`.
    /// Test: `rejects_disordered_thresholds`.
    pub fn is_valid(&self) -> bool {
        let in_range = |x: f32| x > 0.0 && x <= 1.0;
        in_range(self.warn_at)
            && in_range(self.alert_at)
            && in_range(self.compact_at)
            && self.warn_at < self.alert_at
            && self.alert_at < self.compact_at
    }
}

/// A point-in-time token-usage snapshot for one session.
///
/// Why: hook events (`TokenUsageUpdate`, `PostCompact`) report token counts;
/// the daemon stores the latest snapshot per session to drive the gauge.
/// What: used tokens vs. the model's context window size.
/// Test: `fraction_is_ratio` checks the derived ratio.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MemoryUsage {
    /// Tokens currently occupying the context window.
    pub used_tokens: u64,
    /// Total context window size for the session's model.
    pub window_tokens: u64,
}

impl MemoryUsage {
    /// Fraction of the context window currently used, clamped to `0.0..=1.0`.
    ///
    /// Why: the TUI gauge and threshold classifier both need this ratio.
    /// What: `used / window`, guarding against a zero window.
    /// Test: `fraction_is_ratio`.
    pub fn fraction(&self) -> f32 {
        if self.window_tokens == 0 {
            return 0.0;
        }
        (self.used_tokens as f32 / self.window_tokens as f32).clamp(0.0, 1.0)
    }

    /// Classify this usage against a `MemoryConfig` into a pressure level.
    ///
    /// Why: the daemon needs one place that decides warn vs. alert vs. compact.
    /// What: returns the highest threshold the current fraction has crossed.
    /// Test: `pressure_classification_at_boundaries`.
    pub fn pressure(&self, config: &MemoryConfig) -> MemoryPressure {
        let f = self.fraction();
        if f >= config.compact_at {
            MemoryPressure::Compact
        } else if f >= config.alert_at {
            MemoryPressure::Alert
        } else if f >= config.warn_at {
            MemoryPressure::Warn
        } else {
            MemoryPressure::Ok
        }
    }
}

/// Derived memory-pressure level for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MemoryPressure {
    /// Below the warn threshold — healthy.
    Ok,
    /// At/above warn — surface a non-blocking warning.
    Warn,
    /// At/above alert — push a Telegram alert.
    Alert,
    /// At/above compact — trigger automatic compaction.
    Compact,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert!(MemoryConfig::default().is_valid());
    }

    #[test]
    fn rejects_disordered_thresholds() {
        let bad = MemoryConfig {
            warn_at: 0.9,
            alert_at: 0.8,
            compact_at: 0.95,
        };
        assert!(!bad.is_valid());
    }

    #[test]
    fn fraction_is_ratio() {
        let u = MemoryUsage {
            used_tokens: 50,
            window_tokens: 200,
        };
        assert!((u.fraction() - 0.25).abs() < f32::EPSILON);
        let empty = MemoryUsage {
            used_tokens: 10,
            window_tokens: 0,
        };
        assert_eq!(empty.fraction(), 0.0);
    }

    #[test]
    fn pressure_classification_at_boundaries() {
        let cfg = MemoryConfig::default();
        let at = |frac: f32| MemoryUsage {
            used_tokens: (frac * 1000.0) as u64,
            window_tokens: 1000,
        };
        assert_eq!(at(0.50).pressure(&cfg), MemoryPressure::Ok);
        assert_eq!(at(0.70).pressure(&cfg), MemoryPressure::Warn);
        assert_eq!(at(0.85).pressure(&cfg), MemoryPressure::Alert);
        assert_eq!(at(0.90).pressure(&cfg), MemoryPressure::Compact);
        assert_eq!(at(0.99).pressure(&cfg), MemoryPressure::Compact);
    }

    #[test]
    fn pressure_levels_are_ordered() {
        assert!(MemoryPressure::Ok < MemoryPressure::Warn);
        assert!(MemoryPressure::Warn < MemoryPressure::Alert);
        assert!(MemoryPressure::Alert < MemoryPressure::Compact);
    }
}
