//! The service's tmux source: pane activity read by polling, no client.
//!
//! Why: an attached `tmux -C` client counts in `#{session_attached}` and
//! `list-clients`, so `tm ls -a` showed every session attached and `tm`'s
//! switch-target lookup refused a session with two clients (#9779 review).
//! The service attaches no tmux client to any session, in any mode.
//! What: [`TmuxActivity::poll`] runs one `tmux list-panes -a -F` reading each
//! pane's session, `%N` id and `#{window_activity}` (the poll session left
//! out) and compares the rows with the last successful read: any change (a
//! newer activity time, a pane opened or closed) is an event. The first read
//! is the baseline. A failed call is `Err`; the loop raises it as the
//! [`TMUX_FAULT`] fault and clears it on the next success.
//! Test: `tmux_activity_change_is_an_event`,
//! `tmux_failure_is_a_fault_then_clears`, `serve_never_attaches_a_tmux_client`
//! (tests/serve_activity.rs).

use std::collections::BTreeMap;

use crate::tmux::{SystemTmux, Tmux};

/// The fault kind of a failed activity read.
pub const TMUX_FAULT: &str = "watch-tmux";
/// The one tmux call per scan: session, pane id, window activity time.
pub const ACTIVITY_FORMAT: &str = "#{session_name}\t#{pane_id}\t#{window_activity}";

/// Pane activity on one tmux server; see the module docs.
pub struct TmuxActivity {
    tmux: SystemTmux,
    exclude: Vec<String>,
    last: Option<BTreeMap<String, String>>,
}

impl TmuxActivity {
    /// Activity on `tmux`'s server for every session not in `exclude`.
    pub fn new(tmux: SystemTmux, exclude: Vec<String>) -> Self {
        Self {
            tmux,
            exclude,
            last: None,
        }
    }

    /// Reads the activity; true when it changed since the last read.
    ///
    /// # Errors
    /// The `list-panes` call failed (the text is one line). The baseline is
    /// kept, so the next good read compares with the last good one.
    pub fn poll(&mut self) -> Result<bool, String> {
        let out = self
            .tmux
            .run(&["list-panes", "-a", "-F", ACTIVITY_FORMAT])
            .map_err(|e| e.to_string().replace('\n', " "))?;
        let rows: BTreeMap<String, String> = out
            .lines()
            .filter_map(|line| {
                let mut f = line.splitn(3, '\t');
                let (session, pane, at) = (f.next()?, f.next()?, f.next()?);
                if self.exclude.iter().any(|x| x == session) {
                    return None;
                }
                Some((format!("{session}\t{pane}"), at.trim().to_owned()))
            })
            .collect();
        let changed = self.last.as_ref().is_some_and(|last| *last != rows);
        self.last = Some(rows);
        Ok(changed)
    }
}
