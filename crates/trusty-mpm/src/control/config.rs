//! Control-plane auth + cost configuration (§9 of SPEC-SESSCTL-01, WI-5 #1596).
//!
//! Why: WI-5 introduces the auth-and-cost guardrails for the session control
//! plane — a concurrent-session cap, a staggered-launch delay, an auth timeout,
//! and the optional OpenRouter activity classifier (§8.3). These need a single,
//! opt-in, fully-defaulted config surface so operators can tune them in
//! `~/.trusty-mpm/config.toml` without recompiling, and so an absent or partial
//! `[control_plane]` section deserializes to the spec's documented defaults
//! (cap 5, stagger 2000 ms, auth timeout 30 s, classifier off).
//! What: [`ControlPlaneConfig`] mirrors the §9 / §8.3 TOML shape — three
//! top-level cost fields plus a nested `[control_plane.observability]`
//! subsection that gates the optional LLM classifier. Every struct and field is
//! `#[serde(default)]` so partial and absent config both parse.
//! Test: `control_plane_config_*` in the inline `tests` module.

use serde::{Deserialize, Serialize};

/// Default maximum number of concurrent sessions (§9.2).
///
/// Why: a single named constant keeps the spec default (`5`) authoritative and
/// referenceable from both `Default` and tests, so a careless edit fails a
/// pinning test rather than silently changing behavior.
/// What: the spec §9.2 `max_concurrent_sessions` default.
/// Test: `control_plane_config_default_values`.
pub const DEFAULT_MAX_CONCURRENT_SESSIONS: u32 = 5;

/// Default launch-stagger delay in milliseconds (§9.2).
///
/// Why: the spec mandates a 2 s gap between successive spawns to avoid
/// thundering-herd contention on the shared Max rate bucket; pinning it as a
/// constant lets tests reference it and guards against accidental zeroing.
/// What: the spec §9.2 `launch_stagger_ms` default.
/// Test: `control_plane_config_default_values`.
pub const DEFAULT_LAUNCH_STAGGER_MS: u64 = 2_000;

/// Default auth timeout in seconds (§4.4).
///
/// Why: a stream-JSON session that lands in `AuthFailed` auto-stops after this
/// many seconds if the operator does not intervene; pinning the spec default
/// (`30`) keeps `tm auth`'s documented behavior stable.
/// What: the spec §4.4 `auth_timeout_secs` default.
/// Test: `control_plane_config_default_values`.
pub const DEFAULT_AUTH_TIMEOUT_SECS: u64 = 30;

/// `[control_plane]` — auth + cost model configuration (§9 of SPEC-SESSCTL-01).
///
/// Why: the daemon must enforce a concurrency cap and a launch stagger to keep
/// concurrent Max-OAuth sessions from saturating the shared rate bucket (§9.2),
/// and must size the auth timeout (§4.4). Bundling these into one typed,
/// fully-defaulted struct gives the registry a single injectable knob set and
/// keeps absent config a strict no-op (all spec defaults).
/// What: `max_concurrent_sessions` (cap), `launch_stagger_ms` (inter-spawn
/// delay), `auth_timeout_secs` (stream-JSON `AuthFailed` auto-stop), and the
/// nested `observability` subsection (LLM-classifier gate, §8.3). All fields
/// `#[serde(default)]`.
/// Test: `control_plane_config_full_parsed`, `control_plane_config_absent_is_default`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControlPlaneConfig {
    /// Maximum number of concurrent live sessions (§9.2). Default `5`. A value
    /// of `0` is treated as "no admission allowed" (every launch is rejected).
    /// There is no "unbounded" sentinel: to effectively disable the cap, set a
    /// large number.
    pub max_concurrent_sessions: u32,

    /// Delay in milliseconds applied before each session launch when launching
    /// in a loop (§9.2). Default `2000`. Tests inject `0` for determinism.
    pub launch_stagger_ms: u64,

    /// Seconds a stream-JSON session may remain in `AuthFailed` before the
    /// daemon auto-stops it (§4.4). Default `30`.
    pub auth_timeout_secs: u64,

    /// `[control_plane.observability]` — optional LLM-classifier gate (§8.3).
    pub observability: ObservabilityConfig,
}

impl Default for ControlPlaneConfig {
    /// Why: `#[serde(default)]` on each field requires a `Default` whose values
    /// match the spec §9 / §4.4 documented defaults, not Rust zero-values
    /// (which would give `max_concurrent_sessions = 0`, blocking all launches).
    /// What: returns cap `5`, stagger `2000 ms`, auth timeout `30 s`, and the
    /// default (classifier-off) observability subsection.
    /// Test: `control_plane_config_default_values`.
    fn default() -> Self {
        Self {
            max_concurrent_sessions: DEFAULT_MAX_CONCURRENT_SESSIONS,
            launch_stagger_ms: DEFAULT_LAUNCH_STAGGER_MS,
            auth_timeout_secs: DEFAULT_AUTH_TIMEOUT_SECS,
            observability: ObservabilityConfig::default(),
        }
    }
}

impl ControlPlaneConfig {
    /// Returns the launch stagger as a [`std::time::Duration`].
    ///
    /// Why: the registry's stagger-sleep path wants a `Duration`, not a raw
    /// millisecond count; centralizing the conversion keeps unit handling in
    /// one place.
    /// What: `Duration::from_millis(self.launch_stagger_ms)`.
    /// Test: `control_plane_config_stagger_duration`.
    pub fn stagger_duration(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.launch_stagger_ms)
    }

    /// Returns the auth timeout as a [`std::time::Duration`].
    ///
    /// Why: callers that arm the `AuthFailed` auto-stop timer want a `Duration`.
    /// What: `Duration::from_secs(self.auth_timeout_secs)`.
    /// Test: `control_plane_config_auth_timeout_duration`.
    pub fn auth_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.auth_timeout_secs)
    }
}

/// `[control_plane.observability]` — optional OpenRouter classifier gate (§8.3).
///
/// Why: the §8.3 LLM classifier is an explicit fallback, NOT the default. It is
/// gated by config so the default path never requires an `OPENROUTER_API_KEY`
/// and never makes an outbound call. Encoding the gate as a typed flag (default
/// `false`) makes the no-op guarantee (AC-9) auditable.
/// What: a single `llm_classifier` toggle (default `false`).
/// Test: `control_plane_config_default_values`, `observability_classifier_default_off`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObservabilityConfig {
    /// Enable the optional §8.3 OpenRouter classifier. Default `false`. Even
    /// when `true`, the classifier only runs if `OPENROUTER_API_KEY` is present
    /// AND no SM agent is connected (see [`classifier`](crate::control::classifier)).
    pub llm_classifier: bool,
}

impl Default for ObservabilityConfig {
    /// Why: the headline AC-9 guarantee is that the classifier is OFF by
    /// default; the derived zero-value `Default` happens to give `false` but we
    /// spell it out so the intent is explicit and a future field addition does
    /// not silently flip it.
    /// What: returns `llm_classifier = false`.
    /// Test: `observability_classifier_default_off`.
    fn default() -> Self {
        Self {
            llm_classifier: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_plane_config_absent_is_default() {
        let cfg: ControlPlaneConfig = toml::from_str("").expect("empty input parses");
        assert_eq!(cfg, ControlPlaneConfig::default());
    }

    #[test]
    fn control_plane_config_default_values() {
        let cfg = ControlPlaneConfig::default();
        assert_eq!(cfg.max_concurrent_sessions, 5);
        assert_eq!(cfg.launch_stagger_ms, 2_000);
        assert_eq!(cfg.auth_timeout_secs, 30);
        assert!(!cfg.observability.llm_classifier);
    }

    #[test]
    fn observability_classifier_default_off() {
        assert!(!ObservabilityConfig::default().llm_classifier);
    }

    #[test]
    fn control_plane_config_full_parsed() {
        let toml = r#"
max_concurrent_sessions = 8
launch_stagger_ms = 500
auth_timeout_secs = 45

[observability]
llm_classifier = true
"#;
        let cfg: ControlPlaneConfig = toml::from_str(toml).expect("full config parses");
        assert_eq!(cfg.max_concurrent_sessions, 8);
        assert_eq!(cfg.launch_stagger_ms, 500);
        assert_eq!(cfg.auth_timeout_secs, 45);
        assert!(cfg.observability.llm_classifier);
    }

    #[test]
    fn control_plane_config_partial_takes_defaults() {
        let toml = r#"
launch_stagger_ms = 0
"#;
        let cfg: ControlPlaneConfig = toml::from_str(toml).expect("partial config parses");
        // Explicitly set:
        assert_eq!(cfg.launch_stagger_ms, 0);
        // Defaulted:
        assert_eq!(cfg.max_concurrent_sessions, 5);
        assert_eq!(cfg.auth_timeout_secs, 30);
        assert_eq!(cfg.observability, ObservabilityConfig::default());
    }

    #[test]
    fn control_plane_config_stagger_duration() {
        let cfg = ControlPlaneConfig {
            launch_stagger_ms: 1_500,
            ..Default::default()
        };
        assert_eq!(
            cfg.stagger_duration(),
            std::time::Duration::from_millis(1_500)
        );
        // Zero stagger (test-injection path) yields a zero duration.
        let zero = ControlPlaneConfig {
            launch_stagger_ms: 0,
            ..Default::default()
        };
        assert_eq!(zero.stagger_duration(), std::time::Duration::ZERO);
    }

    #[test]
    fn control_plane_config_auth_timeout_duration() {
        let cfg = ControlPlaneConfig::default();
        assert_eq!(cfg.auth_timeout(), std::time::Duration::from_secs(30));
    }
}
