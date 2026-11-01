//! `tm hook` mode dispatch.
//!
//! Why: `main.rs` sits at the 500-SLOC cap, and each new hook mode (#6887,
//! #7688, #8454) added a branch to its match arm. The branches live here so a
//! mode costs `main.rs` nothing.
//! What: [`run`] routes one parsed `tm hook` invocation to its mode.
//! Test: the flag parse in `cli_parses_hook_egress_guard` and its siblings; each
//! mode end to end through the binary (`tests/tm_hook_*.rs`).

use crate::cli::Command;

/// Run the `tm hook` mode its flags select.
///
/// Why: see the module doc.
/// What: `--egress-guard` first — it is a fail-closed deny layer, so it wins
/// any flag combination (#8454) — then `--pm-guard`, `--divert-check`,
/// `--prompt-feedback`, else the daemon relay. `hook` is always a
/// [`Command::Hook`]; any other variant is a caller bug and returns an error.
/// Test: `egress_denies_each_family_tool_name` reaches the egress branch through
/// the binary; `cli_parses_hook_egress_guard` the flag.
pub(crate) async fn run(
    hook: Command,
    client: &reqwest::Client,
    url: &str,
    started: std::time::Instant,
) -> anyhow::Result<()> {
    let Command::Hook {
        pm_guard,
        divert_check,
        prompt_feedback,
        egress_guard,
    } = hook
    else {
        anyhow::bail!("hook_mode::run was handed a command that is not `tm hook`");
    };
    if egress_guard {
        crate::commands::egress_guard::egress_guard(url).await
    } else if pm_guard {
        // #10600: a guard that panics or errors denies instead of exiting non-zero.
        crate::commands::pm_guard_refusal::run(url, started).await
    // #6887: a separate hook mode, not a pm-guard variant.
    } else if divert_check {
        crate::commands::divert_check::divert_check().await
    // #7688: likewise — the capture never relays to the daemon.
    } else if prompt_feedback {
        crate::commands::prompt_feedback_hook::prompt_feedback_hook().await
    } else {
        crate::commands::misc::hook(client, url).await
    }
}
