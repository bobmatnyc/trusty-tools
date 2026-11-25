//! The event-time bounds gate of the coordinate tools (#11696 Phase A).
//!
//! Why: a window can shrink, close or change between the frame an agent saw
//! and the event it asks for; a check against an old frame would let a click
//! land outside the window the operator delegated.
//! What: [`Inner::pointer_bounds`] reads the granted window's frame from the
//! daemon as the last step before the `forward` audit line and the driver
//! call, keeps the broker's snapshot of that window in step with the
//! daemon's (the read replaces the daemon's element cache), and refuses any
//! point outside the frame. A drag then passes the window-on-top gate
//! ([`window_order::on_top`], #11756) on the window order read right after.
//! Test: `click_at_outside_the_current_frame_is_refused`,
//! `shrunk_window_refuses_a_point_valid_in_the_agents_view`,
//! `covered_drag_is_refused_before_the_driver`.

use serde_json::{Map, Value, json};

use crate::broker::Inner;
use crate::launcher::Launcher;
use crate::manifest::LeaseScope;
use crate::menu;
use crate::pointer::{self, Refusal};
use crate::redact::redact_reply;
use crate::screenshot;
use crate::window_order;

impl<L: Launcher> Inner<L> {
    /// Check every point of a coordinate call against the window's frame now.
    ///
    /// Postcondition: `Ok(frame)` only when the daemon proved the granted
    /// window's frame on this call and every point of `args` lies inside it,
    /// and, for a drag, no window was in front of it on the path (#11756);
    /// the returned JSON is that frame, for the audit.
    /// Test: `click_at_outside_the_current_frame_is_refused`,
    /// `frame_read_failure_refuses_the_input`,
    /// `covered_drag_is_refused_before_the_driver`.
    pub(crate) fn pointer_bounds(
        &mut self,
        tool: &str,
        args: &Map<String, Value>,
        scope: LeaseScope,
    ) -> Result<Value, Refusal> {
        let request = pointer::frame_args(scope.pid, scope.window_id);
        let (mut reply, _) = self.drive(pointer::FRAME_TOOL, &request, scope);
        let key = (scope.pid, scope.window_id);
        if reply["ok"] != json!(true) {
            self.snaps.remove(&key);
            return Err((
                "frame_unavailable",
                "the window frame could not be read".into(),
            ));
        }
        screenshot::strip_images(&mut reply["result"]);
        let frame = pointer::frame(&reply["result"], scope.window_id);
        if reply["result"]["structuredContent"].is_object() {
            let (paths, _) = menu::allowed_paths(&reply["result"]);
            let red = redact_reply(&reply["result"]);
            self.snaps.insert(key, (red.tokens, red.fingerprint, paths));
        } else {
            self.snaps.remove(&key);
        }
        let frame = frame?;
        let points = pointer::points(tool, args);
        pointer::within(frame, &points)?;
        let mut audit = json!({"width": frame.width, "height": frame.height});
        if tool == pointer::DRAG_TOOL {
            // #11756: read last, so the order checked is the one the drag meets.
            let windows = self
                .windows
                .on_screen()
                .map_err(|e| (window_order::UNAVAILABLE, e))?;
            let order = window_order::on_top(&windows, scope.pid, scope.window_id, frame, &points)?;
            audit["on_top"] = order;
        }
        Ok(audit)
    }
}
