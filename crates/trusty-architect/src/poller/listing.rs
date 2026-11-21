//! Pane listing and capture: the poller's two read-only tmux calls.
//!
//! Why: every cycle starts from `list-panes -a` and one `capture-pane` per
//! watched pane; both must name panes by their exact `%N` id (#9171 A3a).
//! What: [`list_panes`] runs the prototype's `list-panes` format and
//! [`parse_list_panes`] splits each row like `line.split("\t", 5)`: rows with
//! fewer than 4 fields are dropped from [`Listing::panes`] and kept in
//! [`Listing::unreadable`], rows with 4 or 5 are padded with `""`. A row whose
//! pane field is not a `%N` id is kept out of [`Listing::panes`] and reported
//! in [`Listing::skipped`]. [`list_panes`] logs both at warn level. [`capture`] runs
//! `capture-pane -e -p -S -80 -t <pane>`.
//! Test: `list_panes_matches_python_goldens`, `capture_and_list_build_exact_argv`,
//! `underscore_separated_rows_are_unreadable_not_silent`,
//! `list_panes_reads_rows_without_a_utf8_locale`.

use serde::Serialize;

use crate::error::TmuxError;
use crate::pystr::splitlines;
use crate::tmux::{PaneId, Tmux};

/// The `list-panes -F` format: session, pane, command, window+pane active,
/// cwd, dead, tab-separated.
pub const LIST_FORMAT: &str = "#{session_name}\t#{pane_id}\t#{pane_current_command}\t\
#{window_active}#{pane_active}\t#{pane_current_path}\t#{pane_dead}";

/// One tmux pane, with the prototype's dict keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pane {
    /// Session name.
    pub session: String,
    /// The pane's `%N` id.
    pub pane: PaneId,
    /// `#{pane_current_command}`.
    pub cmd: String,
    /// `#{window_active}#{pane_active}`, `"11"` for the active pane.
    pub active: String,
    /// `#{pane_current_path}`.
    pub path: String,
    /// `#{pane_dead}` (and any tab-joined rest of the row).
    pub dead: String,
}

/// The parsed `list-panes` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Rows with a valid pane id, in tmux's order.
    pub panes: Vec<Pane>,
    /// Rows the prototype would keep whose pane field is not `%N`.
    pub skipped: Vec<String>,
    /// Non-blank rows with fewer than 4 tab-separated fields (#11543).
    pub unreadable: Vec<String>,
}

/// Parses `list-panes` output in the prototype's `list_panes` shape.
pub fn parse_list_panes(out: &str) -> Listing {
    let mut listing = Listing::default();
    for line in splitlines(out) {
        let mut parts: Vec<&str> = line.splitn(6, '\t').collect();
        if parts.len() < 4 {
            // #11543: a separator tmux rewrote must not vanish silently.
            if !line.trim().is_empty() {
                listing.unreadable.push(line.to_owned());
            }
            continue;
        }
        parts.resize(6, "");
        let Ok(pane) = PaneId::parse(parts[1]) else {
            listing.skipped.push(line.to_owned());
            continue;
        };
        listing.panes.push(Pane {
            session: parts[0].to_owned(),
            pane,
            cmd: parts[2].to_owned(),
            active: parts[3].to_owned(),
            path: parts[4].to_owned(),
            dead: parts[5].to_owned(),
        });
    }
    listing
}

/// Every pane on the server.
///
/// # Errors
/// The `list-panes` call failed; the prototype skips the cycle.
pub fn list_panes<T: Tmux + ?Sized>(tmux: &T) -> Result<Listing, TmuxError> {
    let out = tmux.run(&["list-panes", "-a", "-F", LIST_FORMAT])?;
    let listing = parse_list_panes(&out);
    for row in &listing.unreadable {
        tracing::warn!(row, "list-panes row has fewer than 4 tab-separated fields");
    }
    for row in &listing.skipped {
        tracing::warn!(row, "list-panes row has no %N pane id");
    }
    Ok(listing)
}

/// The pane's screen plus 80 rows of scrollback, with escape codes.
///
/// # Errors
/// The `capture-pane` call failed; the prototype skips that pane.
pub fn capture<T: Tmux + ?Sized>(tmux: &T, pane: &PaneId) -> Result<String, TmuxError> {
    tmux.run(&[
        "capture-pane",
        "-e",
        "-p",
        "-S",
        "-80",
        "-t",
        pane.as_target(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeTmux;

    #[test]
    fn underscore_separated_rows_are_unreadable_not_silent() {
        // tmux's output for LIST_FORMAT with no UTF-8 locale (#11543).
        let listing = parse_list_panes("fleet_%0_claude_11_/tmp_0\n\nfleet_%1_zsh_10_/_0\n");
        assert!(listing.panes.is_empty());
        assert_eq!(
            listing.unreadable,
            ["fleet_%0_claude_11_/tmp_0", "fleet_%1_zsh_10_/_0"]
        );
    }

    #[test]
    fn capture_and_list_build_exact_argv() -> Result<(), TmuxError> {
        let fake = FakeTmux::new();
        capture(&fake, &PaneId::parse("%7")?)?;
        assert_eq!(list_panes(&fake)?, Listing::default());
        let format = "#{session_name}\t#{pane_id}\t#{pane_current_command}\t#{window_active}#{pane_active}\t#{pane_current_path}\t#{pane_dead}";
        assert_eq!(
            fake.calls(),
            [
                vec!["capture-pane", "-e", "-p", "-S", "-80", "-t", "%7"],
                vec!["list-panes", "-a", "-F", format]
            ]
        );
        Ok(())
    }
}
