//! The egress verdict contract between the daemon and `tm hook
//! --egress-guard` (#8454 VC, #8433).
//!
//! Why: a channel-tainted session's path tool call needs the policy
//! readiness verdict, and loading the policy inline costs ~1.5 s. The daemon
//! already holds the policy snapshot, so the hook asks it. Both ends read
//! this one definition, so the reply is a closed set on both sides.
//! What: [`VERDICT_ROUTE`], the [`VerdictRequest`] the hook sends, and the
//! [`VerdictReply`] the daemon answers. Only `Ready` and `NotReady` carry a
//! verdict; `Unserved` tells the hook to run its own inline load.
//! Test: `verdict_reply_wire_shape_is_closed`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::channels_readiness::NotReady;

/// The daemon route the hook POSTs a [`VerdictRequest`] to.
pub const VERDICT_ROUTE: &str = "/api/v1/channels/egress-verdict";

/// The hook's question: is the policy under `home` ready?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictRequest {
    /// The home the hook reads the taint and the policy under. The daemon
    /// serves a verdict only for its own home.
    pub home: PathBuf,
}

/// The daemon's answer, tagged by `answer`.
///
/// Why: a verdict the hook cannot read as one of these must never be taken
/// for "ready".
/// What: `Ready` and `NotReady` carry the snapshot's fingerprint and its age
/// in milliseconds; `NotReady` carries the audit slug and the branch.
/// `Unserved` carries why no verdict was served.
/// Test: `verdict_reply_wire_shape_is_closed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum VerdictReply {
    /// The policy is ready.
    Ready {
        /// `LoadReport::fingerprint` of the report judged.
        fingerprint: u64,
        /// Time since the snapshot's last fresh refresh started, in ms.
        age_ms: u64,
    },
    /// The policy is not ready.
    NotReady {
        /// The audit slug, `NOT_READY_CHECK`.
        slug: String,
        /// The failed branch.
        not_ready: NotReady,
        /// `LoadReport::fingerprint` of the report judged.
        fingerprint: u64,
        /// Time since the snapshot's last fresh refresh started, in ms.
        age_ms: u64,
    },
    /// No verdict is served; the hook loads the policy inline.
    Unserved {
        /// Why.
        why: Unserved,
    },
}

/// Why the daemon served no verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unserved {
    /// The hook's home is not the daemon's home.
    HomeMismatch,
    /// No policy load has completed.
    NeverLoaded,
    /// The snapshot is older than its max age.
    Stale,
    /// The snapshot lock is poisoned.
    Poisoned,
    /// The snapshot refused for any other reason.
    Refused,
    /// The report carries no loader fingerprint (0).
    NoFingerprint,
}
