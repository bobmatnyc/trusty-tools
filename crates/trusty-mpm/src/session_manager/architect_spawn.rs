//! Whether a managed launch spec names the supervisor (Architect) session
//! (#9092).
//!
//! Why: an Architect `claude` starts only through `tm fleet init|relaunch`,
//! which write its launch record. The launch shim (`tm
//! internal-spawn-disclaimed --launch-spec`) accepts a spec from any path, so
//! it must refuse a spec that names the supervisor session rather than start
//! an Architect outside the fleet path.
//! What: [`claims_supervisor`] reads the session store under the
//! `~/.trusty-mpm` root and reports whether the spec's session is a
//! `supervisor` record.
//! Test: the shim's `a_supervisor_spec_with_markers_stripped_writes_no_record_and_spawns_nothing_9092`,
//! `an_unreadable_store_refuses_a_spec_that_names_a_session_9092`.

use std::path::Path;

use super::session_kind::SessionKind;
use super::store_integrity;

/// `Ok(true)` when `session_id` names a `supervisor` record in `root`'s store.
///
/// Why: see the module doc.
/// What: `Ok(false)` for an empty `session_id` (no store read), a missing
/// store, a session the store does not hold, and every kind but `supervisor`,
/// `Unknown` included. `Err` when the store exists but cannot be read or fails
/// [`store_integrity::validate`]: the kind cannot be told, and the shim fails
/// closed on a spec that names a session.
/// Test: `a_supervisor_spec_with_markers_stripped_writes_no_record_and_spawns_nothing_9092`,
/// `ordinary_unknown_and_unlisted_specs_still_launch_9092`,
/// `an_unreadable_store_refuses_a_spec_that_names_a_session_9092`.
pub fn claims_supervisor(root: &Path, session_id: &str) -> Result<bool, String> {
    if session_id.trim().is_empty() {
        return Ok(false);
    }
    let store = root.join("session-manager").join("sessions.json");
    let raw = match std::fs::read_to_string(&store) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", store.display())),
        Ok(raw) => raw,
    };
    let data = store_integrity::validate(&store, &raw).map_err(|e| e.diagnostic())?;
    Ok(data
        .sessions
        .get(session_id)
        .is_some_and(|record| record.kind == SessionKind::Architect))
}
