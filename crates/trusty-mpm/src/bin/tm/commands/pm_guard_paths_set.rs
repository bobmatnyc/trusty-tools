//! The `tm paths set` floor of `tm hook --pm-guard` (#11821 P6b).
//!
//! Why: `tm paths set` writes `~/.trusty-tools/install.toml`, a trust anchor;
//! the owner runs it from an operator terminal and no hooked session may,
//! the Architect included (Architect ruling, 2026-11-26). The command refuses
//! a Claude Code session itself; this floor denies the call before it runs.
//! What: [`evaluate_paths_set`] finds `tm paths set` in a Bash command with
//! the walk the `tm fleet relaunch` floor uses (`find_tm_verb`), so prefixes,
//! wrappers and substitutions read as `tm` reads them; `tm paths show` passes.
//! FAIL-CLOSED: an expanded word where the subcommand is chosen, an opaque
//! route and an unreadable command naming `tm` deny, as for the relaunch floor.
//! Test: `paths_set_binds_every_caller_on_every_path`.

use crate::commands::pm_guard_bash::fleet_relaunch::{VerbHit, Verbs, find_tm_verb};

/// The verb this floor denies.
const PATHS_SET: Verbs = &[&["paths", "set"]];

/// `Some(reason)` when `command` runs, or may run, `tm paths set`.
///
/// Test: `paths_set_binds_every_caller_on_every_path`.
pub(crate) fn evaluate_paths_set(command: &str) -> Option<String> {
    let what = match find_tm_verb(command, PATHS_SET)? {
        VerbHit::Runs => "runs `tm paths set`".to_owned(),
        VerbHit::Unresolved(word) => {
            format!("runs `tm` with `{word}` where the subcommand is chosen")
        }
        VerbHit::Opaque(why) => format!("names `tm`, and {why}"),
        VerbHit::Unreadable(_) => "names `tm` but cannot be parsed".to_owned(),
    };
    Some(format!(
        "Trust-anchor write denied (#11821): this command {what}. `tm paths set` writes \
         the install file `install.toml`, a trust anchor, so only the owner runs it, from an \
         operator terminal outside any Claude Code session — no PM, agent or Architect. \
         `tm paths show` reads it."
    ))
}
