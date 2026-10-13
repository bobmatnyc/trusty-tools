//! Naming each `tm-*` window after its session: `fleet-poll.py`'s
//! `plan_window_renames` and `rename_windows` (988152c lines 1423-1457).
//!
//! Why: Claude Code renames its window to its version string, so a tmux
//! status bar of `2.1.287` windows hides which fleet session is which
//! (#9171 A3b-2).
//! What: [`plan_window_renames`] picks the `tm-*` windows whose name is not
//! their session's; [`rename_windows`] lists the windows and renames each
//! with `rename-window -t =S:IDX S`, an exact target. It sends no keys, never
//! raises and never alerts: each failure is a log line.
//! Test: `test_mismatch_is_planned_and_match_is_left_alone`,
//! `test_non_tm_sessions_are_ignored`,
//! `test_runner_renames_with_exact_target_and_no_send_keys`,
//! `windows_list_failure_is_logged_not_raised`.

use crate::pystr::{py_repr, splitlines};
use crate::tmux::{Tmux, WindowTarget};

/// The `list-windows -F` format: session, window index, window name.
const WINDOW_FORMAT: &str = "#{session_name}\t#{window_index}\t#{window_name}";

/// `(session, index, old name)` for each `tm-*` window not named after its
/// session. `listing` is `list-windows -a -F` output in [`WINDOW_FORMAT`].
pub fn plan_window_renames(listing: &str) -> Vec<(String, String, String)> {
    splitlines(listing)
        .into_iter()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            let [s, idx, name] = parts[..] else {
                return None;
            };
            (s.starts_with("tm-") && name != s)
                .then(|| (s.to_owned(), idx.to_owned(), name.to_owned()))
        })
        .collect()
}

/// Renames every planned window; returns how many were renamed and the log
/// lines. A dry run renames nothing and lists what it would rename (lines
/// starting with two spaces, which the prototype prints).
pub fn rename_windows<T: Tmux + ?Sized>(tmux: &T, dry_run: bool) -> (usize, Vec<String>) {
    let mut log = Vec::new();
    let listing = match tmux.run(&["list-windows", "-a", "-F", WINDOW_FORMAT]) {
        Ok(out) => out,
        Err(e) => {
            log.push(format!("window-rename list ERROR {e}"));
            return (0, log);
        }
    };
    let mut done = 0;
    for (s, idx, old) in plan_window_renames(&listing) {
        let (old_r, new_r) = (py_repr(&old), py_repr(&s));
        if dry_run {
            log.push(format!(
                "  would rename window {s}:{idx} {old_r} -> {new_r}"
            ));
            continue;
        }
        let target = WindowTarget::exact(&s, &idx);
        match tmux.run(&["rename-window", "-t", target.as_target(), &s]) {
            Ok(_) => {
                log.push(format!("window-rename {s}:{idx} {old_r} -> {new_r}"));
                done += 1;
            }
            Err(e) => log.push(format!("window-rename {s}:{idx} ERROR {e}")),
        }
    }
    (done, log)
}
