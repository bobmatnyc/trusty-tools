//! The Architect's own context alert.
//!
//! Why: the Architect's pane has no `ctx N%` field, and an Architect past its
//! refresh bar loses the fleet's thread (ruling b7: 25% of a 1M window).
//! What: [`self_ctx_alerts`] is the prototype's `self_ctx_alerts`: one
//! `self_ctx` alert for [`SUPERVISOR`], fingerprinted by the threshold, when
//! the newest interactive transcript's tokens reach `SELF_CTX_THRESHOLD`% of
//! `SELF_CTX_WINDOW` (#9171 A3a).
//! Test: `test_self_ctx_alerts_only_when_over_threshold`,
//! `self_ctx_alerts_match_python_goldens`.

use super::SUPERVISOR;
use super::config::Config;
use crate::classify::{Alert, alert};
use crate::error::SelfCtxError;
use crate::self_ctx::self_ctx_tokens;

/// One `self_ctx` alert when the Architect's own context is at or over the
/// threshold; none for a missing directory or transcript, or a window <= 0.
///
/// # Errors
/// [`SelfCtxError`] when the transcript cannot be read; the prototype logs
/// `self_ctx ERROR` and raises no alert.
pub fn self_ctx_alerts(cfg: &Config, now: f64) -> Result<Vec<Alert>, SelfCtxError> {
    let (window, threshold) = (cfg.self_ctx_window, cfg.self_ctx_threshold);
    let Some(tokens) = self_ctx_tokens(&cfg.self_ctx_dir)? else {
        return Ok(Vec::new());
    };
    if window <= 0 {
        return Ok(Vec::new());
    }
    let pct = 100.0 * tokens as f64 / window as f64;
    if pct < threshold as f64 {
        return Ok(Vec::new());
    }
    let detail = format!("self ctx {pct:.0}% ({tokens}/{window} tokens)");
    Ok(vec![alert(
        "self_ctx",
        &threshold.to_string(),
        &detail,
        SUPERVISOR,
        "",
        now,
    )])
}
