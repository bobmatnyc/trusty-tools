//! The two rules that gate a HEAD move in a main checkout, asked together.
//!
//! Why: `pm_guard`'s Bash block carried both inline and crossed the 500-SLOC
//! cap when the second one arrived (#7905 review round 2). They belong together
//! anyway — both answer "may this command move the shared HEAD" — and keeping
//! them adjacent is what makes it visible that they ask DIFFERENT questions and
//! that neither substitutes for the other:
//!
//! * **ADR-0044 decision 7 (#7905)** — would the move change the project's
//!   `documents_only` declaration? No daemon round-trip, no live-writer
//!   condition, and it covers every verb that can move HEAD. See
//!   [`evaluate_declaration_head_move`].
//! * **ADR-0048 decision 10, narrowed by ADR-0053** — would the move disturb
//!   another session standing in the same tree? The verb alone is not the whole
//!   decision: the directory must be a main checkout AND the daemon must report
//!   another live writer, so a solo session updating its own checkout is never
//!   denied and a daemon that cannot answer allows. It covers `merge` and
//!   `rebase`.
//!
//! The second was in place when `git merge other/declare-branch` landed the
//! declaration at HEAD on the built binary, which is exactly why the first
//! cannot be conditioned on a daemon answer.
//!
//! What: [`evaluate_head_move_guards`] returns the first refusal either rule
//! produces, or `None`. The order is deliberate — the declaration rule is
//! lexical and local, so it answers before the daemon is asked at all, and
//! ordinary Bash traffic never pays for the query.
//! Test: `crates/trusty-mpm/src/bin/tm/commands/pm_guard_bash/declaration_head_move_tests.rs`
//! for the first rule, `main_checkout_head_move_*` for the second, and
//! `tests/tm_hook_pm_guard.rs` end to end.

use std::path::Path;

use crate::commands::pm_guard_bash::{
    evaluate_declaration_head_move, head_move_deny_reason, main_checkout_head_move,
};
use crate::commands::pm_guard_dispatch;

/// Both HEAD-move refusals, in one call.
///
/// Why: see the module doc — two rules, one question class, and a cap that the
/// inline pair no longer fit under.
/// What: `Some(reason)` from whichever rule refuses first; `None` when both
/// allow. `command` is the Bash command, `hook_cwd` the directory the hook was
/// invoked in, and `payload` the `PreToolUse` body the live-writer query needs.
/// Test: as the module doc.
pub(crate) async fn evaluate_head_move_guards(
    url: &str,
    session_id: &str,
    command: &str,
    hook_cwd: &Path,
    payload: &serde_json::Value,
) -> Option<String> {
    if let Some(reason) = evaluate_declaration_head_move(command, hook_cwd) {
        return Some(reason);
    }
    let (verb, target, root) = main_checkout_head_move(command, hook_cwd)?;
    // Two keys, not one (#5769): `tm hook` stamps a delegation's `cwd` from its
    // own process directory, while `target` is resolved through `cd` and
    // `git -C`. They name the same HEAD but need not be the same string, and a
    // query on one alone matched nothing for a command run from a subdirectory.
    let live = pm_guard_dispatch::live_shared_tree_writers_in(
        url,
        session_id,
        &[root.as_path(), target.as_path()],
        payload,
    )
    .await;
    (!live.is_empty()).then(|| head_move_deny_reason(&verb, &root, &live))
}
