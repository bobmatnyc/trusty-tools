//! Recognise a persisted hook (or `statusLine`) command whose executable lives
//! in a Cargo build tree, whatever the binary is named (issue #7262).
//!
//! Why: [`super::is_mpm_hash_suffixed_artifact`] asks two questions at once —
//! is the stem one of ours, and is the path a `deps/` directory — and answers
//! "not ours" when either fails. #7244 wrote
//! `<repo>/target-7247/debug/deps/test_session_lifecycle-<hash> hook --pm-guard`
//! into a real project's `settings.json` seven times in one day: the stem is a
//! test binary's, so neither `tm hooks clean` nor `tm doctor` could see it, and
//! [`super::write_project_hooks`]'s strip-by-identity step added a correct
//! group BESIDE the broken one instead of replacing it. The corruption is not
//! about the stem. Nothing tm persists may ever point into a build tree, so
//! asking WHERE the executable lives — and nothing about its name — closes the
//! whole class, including stems nobody has produced yet.
//!
//! What: [`is_build_tree_hook_command`] and [`is_build_tree_statusline_command`]
//! split the command string at one of [`TM_HOOK_ARGV_TAILS`] /
//! [`STATUSLINE_ARGV_TAIL`] and ask
//! [`trusty_common::bin_resolve::is_ephemeral_build_path`] about the executable
//! half. [`super::is_mpm_hook_command`] consults the first, so the ONE
//! classifier every consumer already shares — the doctor probe, `tm hooks
//! clean`, and both writers' replace-by-identity strips — sees this shape
//! without a second predicate to keep in sync.
//!
//! **Where the line is drawn.** A build-tree path ALONE is not enough: a
//! project may legitimately register its own hook that happens to live under
//! `target/debug`, and tm must not delete another owner's entry. Both halves
//! must hold — the executable is in a Cargo build tree AND the argv after it is
//! one of the four shapes tm itself writes. A command tm never writes
//! (`<build-tree>/mytool --check`, `<build-tree>/tm lint`) is left untouched no
//! matter where it lives. The residual overlap is a foreign binary invoked with
//! tm's exact argv from inside a Cargo build tree; that command is already dead
//! (the artifact is deleted by the next `cargo build`), and `tm hooks clean`'s
//! dry run prints every matched command before `--force` removes anything.
//!
//! Test: `build_tree_tests.rs`.

use std::path::Path;

/// The `hook` sub-command argv tails tm persists into a settings file.
///
/// Why (#7262): the second half of the two-part test above. Each entry is a
/// shape one of tm's writers produces: `" hook"` is the six-event lifecycle
/// triad ([`super::mpm_hook_additions_with_exe`]), [`PM_GUARD_SUFFIX`] the PM
/// enforcement guard, and [`DIVERT_CHECK_SUFFIX`] the #6887 bulk-read diversion
/// groups. Anything else is not tm's to classify.
/// What: the three tails, matched as exact string suffixes so the whole prefix
/// is the executable path — spaces in that path included. They are mutually
/// exclusive: a command ending in [`PM_GUARD_SUFFIX`] cannot also end in
/// `" hook"`.
/// Test: `build_tree_hook_command_matches_every_tm_argv_shape`,
/// `pm_guard_and_divert_commands_end_in_a_known_argv_tail` (the drift guard, in
/// `session_launch::project_hooks_tests`).
const TM_HOOK_ARGV_TAILS: &[&str] = &[" hook", PM_GUARD_SUFFIX, DIVERT_CHECK_SUFFIX];

/// The `statusLine.command` argv tail (`#4492`, `#7262`).
///
/// Why: `statusLine.command` is written by a different code path than the hook
/// groups (`session_launch::settings::resolve_statusline_binary_with`) and is
/// not repaired by the hooks writer, so it is deliberately NOT in
/// [`TM_HOOK_ARGV_TAILS`] — a strip that removed it would silently drop a key
/// no writer here puts back. Detection still has to name it, which is what this
/// separate tail is for.
/// What: the one tail `resolve_statusline_command` appends.
/// Test: `build_tree_statusline_command_flags_a_build_tree_binary`.
const STATUSLINE_ARGV_TAIL: &str = " statusline";

/// The `hook --divert-check` argv tail (#6887).
///
/// Why (#7262): `session_launch::divert_hooks` used to own this literal and
/// this module needs the same string. Defining it in the lower layer and
/// re-using it there keeps ONE definition, so the diversion shape cannot drift
/// out of [`TM_HOOK_ARGV_TAILS`] the way a copied literal would.
/// What: the suffix `divert_hook_groups` appends to the resolved binary.
/// Test: `is_project_managed_hook_command_recognises_divert_check`.
pub(crate) const DIVERT_CHECK_SUFFIX: &str = " hook --divert-check";

/// The `hook --pm-guard` argv tail (#1914, #7262).
///
/// Why: three modules read or write this exact shape — the writer
/// (`session_launch::settings::pm_guard_hook_value`), the strip's identity
/// predicate (`session_launch::project_hooks::is_project_managed_hook_command`),
/// and [`TM_HOOK_ARGV_TAILS`] here. Each spelled the literal itself, so a
/// change to the sub-flag would have silently split them: the writer would
/// persist a shape the strip no longer recognises, and every relaunch would
/// append a duplicate group instead of replacing one — the #2948 failure mode.
/// One definition in the lowest layer, re-used upward, is the same fix
/// [`DIVERT_CHECK_SUFFIX`] already applies to the sibling shape.
/// What: the literal suffix, including its leading space.
/// Test: `pm_guard_and_divert_commands_end_in_a_known_argv_tail` (the drift
/// guard, in `session_launch::project_hooks_tests`),
/// `build_tree_hook_command_matches_every_tm_argv_shape`.
pub(crate) const PM_GUARD_SUFFIX: &str = " hook --pm-guard";

/// Does `cmd` invoke a hook from a binary inside a Cargo build tree?
///
/// Why (#7262): the predicate [`super::is_mpm_hook_command`] consults so every
/// consumer — the `tm doctor` hook-hygiene probe, `tm hooks clean`, and the
/// replace-by-identity strip in both writers — classifies the #7244 corruption
/// identically, without any of them growing its own copy of the rule.
/// What: `true` when `cmd` ends with one of [`TM_HOOK_ARGV_TAILS`] and the
/// remaining prefix is an ABSOLUTE path that
/// [`trusty_common::bin_resolve::is_ephemeral_build_path`] rejects (any
/// `target*/{debug,release}` layout, a `CARGO_TARGET_DIR` tree, a
/// `.claude/worktrees/` checkout, or a system temp root). A relative prefix is
/// never matched: tm always persists an absolute path, so a bare `tm hook`
/// belongs to the exact-name branch, not this one.
/// Test: `build_tree_hook_command_flags_the_incident_shape`,
/// `build_tree_hook_command_matches_every_tm_argv_shape`,
/// `build_tree_hook_command_ignores_an_installed_binary`,
/// `build_tree_hook_command_ignores_a_foreign_argv_shape`.
pub fn is_build_tree_hook_command(cmd: &str) -> bool {
    exe_in_build_tree(cmd, TM_HOOK_ARGV_TAILS)
}

/// Does `cmd` invoke `statusline` from a binary inside a Cargo build tree?
///
/// Why (#7262, #4492): the `statusLine.command` half of the same corruption.
/// `tm doctor` must NAME it — an operator whose statusline points at a deleted
/// test harness sees a blank segment and no error anywhere — but the hooks
/// writer does not own that key, so nothing here removes it.
/// What: [`is_build_tree_hook_command`]'s rule against
/// [`STATUSLINE_ARGV_TAIL`] instead of the hook tails.
/// Test: `build_tree_statusline_command_flags_a_build_tree_binary`,
/// `build_tree_statusline_command_ignores_an_installed_binary`.
pub fn is_build_tree_statusline_command(cmd: &str) -> bool {
    exe_in_build_tree(cmd, &[STATUSLINE_ARGV_TAIL])
}

/// Shared rule behind both predicates above.
///
/// Why: the two differ only in which argv tails they accept; the path test is
/// the same question and must not be answered twice.
/// What: finds the first `tails` entry that is a suffix of `cmd`, then returns
/// whether the remaining prefix is an absolute build-tree path.
fn exe_in_build_tree(cmd: &str, tails: &[&str]) -> bool {
    let Some(exe) = tails.iter().find_map(|tail| cmd.strip_suffix(tail)) else {
        return false;
    };
    let path = Path::new(exe);
    path.is_absolute() && trusty_common::bin_resolve::is_ephemeral_build_path(path)
}

// `pub(crate)` so the #7244 incident fixture below is written ONCE and shared
// by the cleanup, writer, and `tm doctor` regression tests (#7262).
#[cfg(test)]
#[path = "build_tree_tests.rs"]
pub(crate) mod tests;
