//! Circuit-breaker model for agent delegations.
//!
//! Why: claude-mpm uses a circuit breaker to stop runaway delegation loops and
//! cap per-agent failures. trusty-mpm keeps the same semantics but as in-daemon
//! state instead of disk-persisted Python counters. Centralizing the transition
//! math here keeps the daemon enforcer, the MCP `circuit_breaker_status` tool,
//! and the TUI badge in agreement.
//! What: `CircuitState` (closed/open/half-open), `CircuitConfig` (failure and
//! delegation-depth limits), and `CircuitBreaker` (a per-agent counter that
//! drives transitions).
//! Test: `cargo test -p trusty-mpm-core` walks the closed → open → half-open →
//! closed cycle and checks the depth limit.

use serde::{Deserialize, Serialize};

/// State of a circuit breaker guarding one agent.
///
/// Why: a tri-state breaker is the standard pattern — `Closed` lets calls
/// through, `Open` fails fast, `HalfOpen` lets a single probe through.
/// What: serde uses snake_case so the wire form is stable across clients.
/// Test: covered by the `CircuitBreaker` transition tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    /// Healthy — delegations are allowed through.
    Closed,
    /// Tripped — delegations fail fast without running.
    Open,
    /// Probing — one delegation is allowed through to test recovery.
    HalfOpen,
}

/// Tuning knobs for a circuit breaker.
///
/// Why: every deployment may want a different failure tolerance and a different
/// cap on how deep PM → subagent → subagent chains may nest.
/// What: `failure_threshold` trips the breaker; `max_depth` caps delegation
/// nesting to stop infinite recursion.
/// Test: `default_config_is_sane`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CircuitConfig {
    /// Consecutive failures that trip the breaker from `Closed` to `Open`.
    pub failure_threshold: u32,
    /// Maximum delegation-tree depth allowed before delegations are refused.
    pub max_depth: u32,
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            max_depth: 5,
        }
    }
}

/// A per-agent circuit breaker.
///
/// Why: the daemon holds one of these per agent name and consults it before
/// allowing a new delegation. It is a plain value type (no async, no IO) so it
/// is trivially testable and cheap to snapshot for the dashboard.
/// What: tracks `CircuitState` plus a consecutive-failure counter; `record_*`
/// methods drive transitions per `CircuitConfig`.
/// Test: `breaker_trips_and_recovers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreaker {
    /// Current breaker state.
    pub state: CircuitState,
    /// Consecutive failures observed since the last success.
    pub consecutive_failures: u32,
    /// Configuration governing transitions.
    pub config: CircuitConfig,
}

impl CircuitBreaker {
    /// Create a closed breaker with the given config.
    pub fn new(config: CircuitConfig) -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            config,
        }
    }

    /// True if a delegation is currently permitted to run.
    ///
    /// Why: the daemon calls this as the gate before spawning a subagent.
    /// What: `Closed` and `HalfOpen` allow a call; `Open` refuses.
    /// Test: `breaker_trips_and_recovers` checks the gate at each state.
    pub fn allows_delegation(&self) -> bool {
        !matches!(self.state, CircuitState::Open)
    }

    /// True if `depth` exceeds the configured maximum nesting.
    ///
    /// Why: independent of failures, runaway PM → subagent recursion must be
    /// stopped by depth alone.
    /// What: compares `depth` against `config.max_depth`.
    /// Test: `depth_limit_is_enforced`.
    pub fn exceeds_depth(&self, depth: u32) -> bool {
        depth > self.config.max_depth
    }

    /// Record a successful delegation.
    ///
    /// Why: success resets the failure counter and closes a probing breaker.
    /// What: clears `consecutive_failures` and moves `HalfOpen` → `Closed`.
    /// Test: `breaker_trips_and_recovers`.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        if self.state == CircuitState::HalfOpen {
            self.state = CircuitState::Closed;
        }
    }

    /// Record a failed delegation.
    ///
    /// Why: failures accumulate and trip the breaker at the threshold.
    /// What: increments the counter; `Closed` trips to `Open` at the threshold;
    /// a failure while `HalfOpen` re-opens the breaker immediately.
    /// Test: `breaker_trips_and_recovers`.
    pub fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        match self.state {
            CircuitState::Closed => {
                if self.consecutive_failures >= self.config.failure_threshold {
                    self.state = CircuitState::Open;
                }
            }
            CircuitState::HalfOpen => {
                self.state = CircuitState::Open;
            }
            CircuitState::Open => {}
        }
    }

    /// Move an open breaker into the half-open probing state.
    ///
    /// Why: after a cool-down the daemon lets one probe delegation through.
    /// What: `Open` → `HalfOpen`; other states are unchanged.
    /// Test: `breaker_trips_and_recovers`.
    pub fn attempt_reset(&mut self) {
        if self.state == CircuitState::Open {
            self.state = CircuitState::HalfOpen;
        }
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(CircuitConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_sane() {
        let cfg = CircuitConfig::default();
        assert!(cfg.failure_threshold > 0);
        assert!(cfg.max_depth > 0);
    }

    #[test]
    fn breaker_trips_and_recovers() {
        let mut cb = CircuitBreaker::new(CircuitConfig {
            failure_threshold: 3,
            max_depth: 5,
        });
        assert_eq!(cb.state, CircuitState::Closed);
        assert!(cb.allows_delegation());

        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state, CircuitState::Closed);
        cb.record_failure(); // third failure trips it
        assert_eq!(cb.state, CircuitState::Open);
        assert!(!cb.allows_delegation());

        cb.attempt_reset();
        assert_eq!(cb.state, CircuitState::HalfOpen);
        assert!(cb.allows_delegation());

        // A failure while probing re-opens immediately.
        cb.record_failure();
        assert_eq!(cb.state, CircuitState::Open);

        cb.attempt_reset();
        cb.record_success(); // a successful probe closes it
        assert_eq!(cb.state, CircuitState::Closed);
        assert_eq!(cb.consecutive_failures, 0);
    }

    #[test]
    fn depth_limit_is_enforced() {
        let cb = CircuitBreaker::new(CircuitConfig {
            failure_threshold: 3,
            max_depth: 4,
        });
        assert!(!cb.exceeds_depth(4));
        assert!(cb.exceeds_depth(5));
    }
}
