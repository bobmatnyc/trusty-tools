//! The delivery trace of inbound channel answers, `delivery.jsonl` (#9873 Q5).
//!
//! Why: an answer is final once its question resolves (#8433 Q4), because the
//! table keeps only the text's digest. A delivery that failed, or that never
//! settled because the daemon died mid-send, must stay visible to `tm doctor`.
//! What: [`DeliveryLine`], one append-only line per step: `pending` before the
//! send, then `delivered` or `failed`. The file sits in the anchored taint
//! directory beside [`crate::core::channels_taint::REAPPLY_FAILURES_FILE`],
//! so no session can edit it, and a retire leaves it alone (it is not a
//! `*.json` taint).
//! Test: `channels_delivery_tests.rs`, `deliver_tests.rs`,
//! `doctor_channel_taint_tests.rs`.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::channels_taint::TaintError;

/// The delivery trace's file name in the taint directory.
pub const DELIVERY_FILE: &str = "delivery.jsonl";

/// The record format version.
pub const FORMAT_VERSION: u32 = 1;

/// One delivery step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// The asker is tainted and the send is about to run.
    Pending,
    /// The pane accepted the answer.
    Delivered,
    /// The answer was not delivered; it is lost (#8433 Q4).
    Failed,
}

/// One line of `delivery.jsonl`. It holds no answer text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryLine {
    /// [`FORMAT_VERSION`].
    pub v: u32,
    /// When the step was recorded.
    pub ts: DateTime<Utc>,
    /// The step.
    pub state: DeliveryState,
    /// The resolved question's id, decimal.
    pub question_id: String,
    /// The asker's managed session id.
    pub asker: String,
    /// Why a delivery failed: a content-free code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl DeliveryLine {
    /// A line for `state` stamped now.
    pub fn new(state: DeliveryState, question_id: &str, asker: &str, reason: Option<&str>) -> Self {
        Self {
            v: FORMAT_VERSION,
            ts: Utc::now(),
            state,
            question_id: question_id.to_owned(),
            asker: asker.to_owned(),
            reason: reason.map(str::to_owned),
        }
    }
}

/// RED STUB (#9873 S5): writes nothing; the next commit replaces it.
pub fn record_delivery(_dir: &Path, _line: &DeliveryLine) -> Result<(), TaintError> {
    Ok(())
}

/// RED STUB (#9873 S5): reads nothing; the next commit replaces it.
pub fn read_deliveries(_dir: &Path) -> Result<Vec<DeliveryLine>, TaintError> {
    Ok(Vec::new())
}

/// RED STUB (#9873 S5): finds nothing; the next commit replaces it.
pub fn delivery_problems(_lines: &[DeliveryLine], _resolved: &[(String, String)]) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
#[path = "channels_delivery_tests.rs"]
mod tests;
