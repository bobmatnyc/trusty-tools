//! Is the channel route policy ready? The egress guard's gate (#9741, #8454
//! G3 S4).
//!
//! Why: a channel-tainted session's path tool call is refused either way
//! (#8433 P0). Architect ruling (a), 2026-10-12: the policy readiness check
//! runs first, so a policy that is missing, broken or slow is reported as
//! such, by its own audit slug, before the taint deny. No verdict loosens.
//! What: [`policy_ready`] loads the policy inline under [`HOOK_LOAD_LIMIT`]
//! through `crate::channels::readiness` and answers `Err(`[`NotReady`]`)` for
//! every branch that is not a clean, non-empty load. A build without the
//! `channels` feature cannot load it and is never ready (B9).
//! Test: `channels_readiness_tests.rs`; the branches through the hook in
//! `bin/tm/commands/egress_guard_policy_tests.rs`.

use std::path::Path;
use std::time::Duration;

/// The audit slug of the egress guard's not-ready deny (#9741).
pub const NOT_READY_CHECK: &str = "egress-policy-not-ready";

/// How long the hook waits for the inline policy load (S4 plan decision 7).
pub const HOOK_LOAD_LIMIT: Duration = Duration::from_millis(1500);

/// Which host-ceiling fault left the policy not ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostBranch {
    /// No home directory is known (B1).
    HomeUnknown,
    /// `config.yaml` does not exist (B2).
    Missing,
    /// `config.yaml` has no `channels:` section (B4).
    NoChannels,
    /// `config.yaml` cannot be read: not a regular file, not under the home,
    /// over the size cap, not UTF-8, or an I/O error (B3).
    Unreadable,
    /// `config.yaml` does not parse, or its `channels:` section is invalid
    /// (B4).
    Invalid,
    /// Any other host-ceiling refusal, such as a non-canonical project path.
    Refused,
}

impl std::fmt::Display for HostBranch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::HomeUnknown => "no home directory is known",
            Self::Missing => "config.yaml is missing",
            Self::NoChannels => "config.yaml has no channels section",
            Self::Unreadable => "config.yaml is unreadable",
            Self::Invalid => "config.yaml is unparseable or invalid",
            Self::Refused => "the host ceiling is refused",
        })
    }
}

/// Why the route policy is not ready. Every text names a branch and a
/// variant name, never a value from the policy.
///
/// Why: the deny reason names the failed branch (#9741), and a recipient or
/// credential ref must never reach a hook's output.
/// What: one variant per failure branch of the S4 plan's table (B1-B9).
/// Test: `home_unknown_denies_a_gated_call`, `config_missing_denies_a_gated_call`,
/// `config_unreadable_denies_a_gated_call`, `config_unparseable_or_unknown_key_denies`,
/// `denied_report_denies`, `finding_denies_a_gated_call`,
/// `file_not_in_effect_denies_a_gated_call`, `load_past_deadline_denies`,
/// `load_panic_denies`, `empty_policy_with_enabled_channel_denies`,
/// `build_without_channels_feature_denies`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotReady {
    /// The host ceiling was refused (B1-B4).
    #[error("{branch} ({error})")]
    Host {
        /// The branch.
        branch: HostBranch,
        /// The `HostError` variant name.
        error: String,
    },
    /// The whole load was denied without a host fault, such as a route
    /// overlap across two files (B5).
    #[error("the policy load was denied ({finding})")]
    Denied {
        /// The variant name of the first deny-all finding.
        finding: String,
    },
    /// The load dropped a route or refused a file (B6).
    #[error("the policy load reported {count} finding(s), the first {first}")]
    Findings {
        /// How many findings.
        count: usize,
        /// The variant name of the first.
        first: String,
    },
    /// A listed project's route file is not in effect (B6).
    #[error("a listed project's route file is {state}, not in effect")]
    File {
        /// The `FileState` variant name.
        state: String,
    },
    /// The load is clean but no route is in effect (B8).
    #[error("no route is in effect")]
    NoRoutes,
    /// The load did not return within the wait (B7).
    #[error("the policy load did not finish within {0:?}")]
    TimedOut(Duration),
    /// The load ended without a report: it panicked (B7).
    #[error("the policy load stopped without a result")]
    Stopped,
    /// This build has no `channels` feature (B9).
    #[error("this build has no `channels` feature, so it cannot load the policy")]
    Unsupported,
}

impl NotReady {
    /// True when no channel is configured: no `config.yaml`, or no
    /// `channels:` section in it. `tm doctor` reads that as the deny state.
    pub fn is_not_configured(&self) -> bool {
        matches!(
            self,
            Self::Host {
                branch: HostBranch::Missing | HostBranch::NoChannels,
                ..
            }
        )
    }
}

/// Load the route policy under `home` and judge whether it is ready.
///
/// Why: see the module doc. The hook calls this only for a gated call it
/// will deny for taint, so an untainted session never pays the load.
/// What: with the `channels` feature, `crate::channels::readiness::check`
/// with [`HOOK_LOAD_LIMIT`]; without it, [`NotReady::Unsupported`].
/// Test: `config_missing_denies_a_gated_call` (through the hook),
/// `build_without_channels_feature_denies`,
/// `policy_ready_slug_budget_and_missing_config`.
pub fn policy_ready(home: Option<&Path>) -> Result<(), NotReady> {
    #[cfg(feature = "channels")]
    {
        crate::channels::readiness::check(home, HOOK_LOAD_LIMIT)
    }
    // #9741 B9: no policy code in this build; never ready.
    #[cfg(not(feature = "channels"))]
    {
        let _ = home;
        Err(NotReady::Unsupported)
    }
}
