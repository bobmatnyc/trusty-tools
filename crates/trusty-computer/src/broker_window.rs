//! The event-time owner gate of the window tools (#11696 Phase B).
//!
//! Why: the static checks prove the call names the granted pid and window,
//! not that the daemon still sees that window under that pid. A window that
//! closed, or a window id that now belongs to another process, must not be
//! focused, moved or resized.
//! What: [`Inner::window_owner`] reads the granted pid's windows from the
//! daemon as the last step before the `forward` audit line, refuses unless
//! the window is listed under that pid, and returns the one daemon call built
//! from the window's current frame.
//! Test: `window_of_another_pid_is_refused_before_the_driver`,
//! `owner_read_failure_refuses_the_call`.

use serde_json::{Map, Value, json};

use crate::broker::Inner;
use crate::launcher::Launcher;
use crate::manifest::LeaseScope;
use crate::window_mgmt::{self, Refusal};

impl<L: Launcher> Inner<L> {
    /// Prove the granted window is the granted pid's now, and build its call.
    ///
    /// Postcondition: `Ok((bounds, call))` only when the daemon listed
    /// `scope.window_id` under `scope.pid` on this call; `bounds` is that
    /// window's frame for the audit and `call` the one daemon request.
    /// Test: `window_of_another_pid_is_refused_before_the_driver`,
    /// `owner_read_failure_refuses_the_call`.
    pub(crate) fn window_owner(
        &mut self,
        tool: &str,
        args: &Map<String, Value>,
        scope: LeaseScope,
    ) -> Result<(Value, (&'static str, Value)), Refusal> {
        let request = window_mgmt::owner_args(scope.pid);
        let (reply, _) = self.drive(window_mgmt::OWNER_TOOL, &request, scope);
        if reply["ok"] != json!(true) {
            return Err((
                "window_frame_unavailable",
                "the window list could not be read".into(),
            ));
        }
        let now = window_mgmt::owned_bounds(&reply["result"], scope.pid, scope.window_id)?;
        let audit = json!({"x": now.x, "y": now.y, "width": now.width, "height": now.height});
        Ok((audit, window_mgmt::driver_args(tool, args, now)))
    }
}
