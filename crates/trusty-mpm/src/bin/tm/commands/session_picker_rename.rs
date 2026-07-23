//! Interactive-picker rename action (issue #3724).
//!
//! Why: Bob's feedback — "let's provide a workstream rename facility" —
//! surfaces the ALREADY-EXISTING, #3714-hardened `tm sessions rename` path
//! directly in the picker rather than requiring the operator to leave it and
//! remember an id/name. Kept out of `session_picker.rs` (which sits near the
//! 500-SLOC production cap) mirroring how `picker_delete.rs` holds the
//! picker's delete action.
//!
//! What: [`rename_selected`] is the I/O driver for the picker's `r<N>
//! <new-name>` input mode (parsed by `session_picker::parse_picker_choice`
//! into `PickerDecision::Rename`). It calls
//! `commands::rename::do_rename_request` DIRECTLY — the exact PATCH
//! `/api/v1/sessions/managed/{id}` request `tm sessions rename` issues,
//! including its #3692 auto-suffix-on-collision response handling — never
//! reimplementing any tmux-rename logic. The picker already holds a
//! server-resolved `id` from the numbered menu, so (unlike the bare
//! `tm sessions rename <new-name>` in-session form) there is no env-var/pane
//! cross-check to perform here — the `#3600` hijack this guards against
//! cannot occur when the target was picked from a fetched, id-bearing row.
//!
//! Test: `session_picker_tests.rs` covers `PickerDecision::Rename` parsing;
//! the PATCH round trip is exercised unchanged by `rename_tests.rs`'s
//! `do_rename_request_*` coverage, which this module calls into rather than
//! duplicating.

use trusty_mpm::client::ManagedSessionSummary;

/// Rename the picker's selected session and print the outcome (issue #3724).
///
/// Why: a rename failure (404/400/409, or a transport error) must never
/// crash the interactive loop — every other picker refusal branch
/// (`ConfirmRestart`, `Unresumable`, `SlotDeleted`) prints a notice and lets
/// the SAME menu redisplay rather than propagating an `Err` that would exit
/// the whole picker. This mirrors that convention: the daemon round trip's
/// `Result` is always resolved to a printed line, never bubbled up.
/// What: calls `commands::rename::do_rename_request(client, url,
/// &session.name, &session.id, new_name)`; prints its `Ok(message)` verbatim
/// on success, or `"tm: rename failed: {err}"` on any failure (the daemon's
/// own 400/409 body text is already included in `err`'s message via
/// `do_rename_request`). Always returns `Ok(())` — see Why.
/// Test: exercised indirectly by `session_picker_tests.rs`'s `Rename`
/// decision-parsing tests (the HTTP path itself is `do_rename_request`'s,
/// already covered by `rename_tests.rs`).
pub(crate) async fn rename_selected(
    client: &reqwest::Client,
    url: &str,
    session: &ManagedSessionSummary,
    new_name: String,
) -> anyhow::Result<()> {
    match super::rename::do_rename_request(client, url, &session.name, &session.id, new_name).await
    {
        Ok(msg) => eprintln!("tm: {msg}"),
        Err(e) => eprintln!("tm: rename failed: {e}"),
    }
    Ok(())
}
