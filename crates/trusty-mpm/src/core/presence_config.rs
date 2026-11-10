//! `[presence]` in `~/.trusty-mpm/config.toml` (#8878 PR2b-1a).
//!
//! Why: Bob D5b Q2 sets a 5-minute request TTL; plan-pr2b1.md §1 makes it a
//! key. The 120-minute idle window is fixed in `presence::IDLE_LIMIT`, not a
//! key. #8878 PR2b-1b adds the WebAuthn RP id and origin, with no default:
//! missing either leaves the device verifier unavailable (fail-closed).
//! What: [`PresenceConfig::request_ttl`] is the configured TTL, clamped to
//! 1 minute .. `presence_request::MAX_TTL`; a clamp is logged. `rp_id` and
//! `rp_origin` feed `presence_webauthn::WebauthnVerifier::from_config`.
//! [`PresenceConfig::enrol_origin`] is where the browser enrol page lives
//! (#8878 PR2b-2), or `None` while `rp_origin` is unset.
//! Test: `the_request_ttl_defaults_to_five_minutes_and_is_clamped_8878`,
//! `the_enrol_origin_is_rp_origin_when_set_8878`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::presence_request::MAX_TTL;

/// The request TTL when the key is absent (Bob D5b Q2).
pub const DEFAULT_REQUEST_TTL_MINUTES: u64 = 5;

/// `[presence]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresenceConfig {
    /// Minutes a pending presence request stays approvable.
    pub request_ttl_minutes: u64,
    /// The WebAuthn relying-party id (the console's tailnet name); no default.
    pub rp_id: Option<String>,
    /// The one origin approvals may come from, `https://<rp_id>`; no default.
    pub rp_origin: Option<String>,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            request_ttl_minutes: DEFAULT_REQUEST_TTL_MINUTES,
            rp_id: None,
            rp_origin: None,
        }
    }
}

impl PresenceConfig {
    /// The origin the browser enrol page is served on: `rp_origin` without a
    /// trailing `/`, or `None` while it is unset or empty (fail-closed).
    pub fn enrol_origin(&self) -> Option<&str> {
        let origin = self.rp_origin.as_deref()?.trim_end_matches('/');
        (!origin.is_empty()).then_some(origin)
    }

    /// The request TTL, clamped to 1 minute .. [`MAX_TTL`].
    pub fn request_ttl(&self) -> Duration {
        let max = MAX_TTL.as_secs() / 60;
        let minutes = self.request_ttl_minutes.clamp(1, max);
        if minutes != self.request_ttl_minutes {
            tracing::warn!(
                "#8878: [presence] request_ttl_minutes = {} is out of range; using {minutes}",
                self.request_ttl_minutes
            );
        }
        Duration::from_secs(minutes * 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_ttl_defaults_to_five_minutes_and_is_clamped_8878() {
        assert_eq!(
            PresenceConfig::default().request_ttl(),
            Duration::from_secs(300)
        );
        let parsed: crate::core::config::MpmConfig =
            toml::from_str("[presence]\nrequest_ttl_minutes = 7\n").expect("parse");
        assert_eq!(parsed.presence.request_ttl(), Duration::from_secs(420));
        let zero = PresenceConfig {
            request_ttl_minutes: 0,
            ..PresenceConfig::default()
        };
        assert_eq!(zero.request_ttl(), Duration::from_secs(60));
        let huge = PresenceConfig {
            request_ttl_minutes: 10_000,
            ..PresenceConfig::default()
        };
        assert_eq!(huge.request_ttl(), MAX_TTL);
    }

    #[test]
    fn the_enrol_origin_is_rp_origin_when_set_8878() {
        let parsed: crate::core::config::MpmConfig =
            toml::from_str("[presence]\nrp_origin = \"https://s.ts.net/\"\n").expect("parse");
        assert_eq!(parsed.presence.enrol_origin(), Some("https://s.ts.net"));
        assert_eq!(
            PresenceConfig::default().enrol_origin(),
            None,
            "no rp_origin"
        );
        let empty = PresenceConfig {
            rp_origin: Some("/".to_owned()),
            ..PresenceConfig::default()
        };
        assert_eq!(empty.enrol_origin(), None);
    }
}
