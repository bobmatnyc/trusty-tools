//! Update detection + the confirm-then-apply decision logic (#1316).
//!
//! Why: The added #1316 requirement is that `tctl` *never mutates installed
//! binaries without confirmation*. That safety property is a pure decision —
//! "given the candidates, whether we are on a TTY, and the `--yes` flag, do we
//! apply?" — and it must be unit-testable in isolation from any network or
//! process side effects. This module holds (a) the network listing of available
//! updates and (b) the pure [`decide_apply`] gate that the upgrade command and
//! the opt-in auto-update check both route through.
//!
//! What:
//! - [`UpdateCandidate`] — one member with an available newer version.
//! - [`gather_candidates`] — async; probes crates.io for each stable member's
//!   currently-installed version and returns those with a newer release.
//! - [`ApplyDecision`] / [`decide_apply`] — the pure confirm-then-apply gate.
//!
//! Test: `tests` exhaustively covers `decide_apply` (the load-bearing safety
//! gate) and the installed-version parsing; the network path is side-effecting
//! and validated manually / via `trusty_common::update` tests.

use std::process::Command;

use serde::Serialize;

use super::stable_set::StableMember;

/// A stable-set member that has a newer version available.
///
/// Why: `tctl updates` lists these and `tctl upgrade` applies them; a typed
/// record keeps the installed→latest diff explicit and JSON-serialisable.
///
/// What: Carries the crate/binary names plus the installed and latest version
/// strings. `installed` is `None` when the binary is not present on PATH (a
/// candidate for install rather than upgrade).
///
/// Test: `tests::decide_*` build these; `gather_candidates` populates them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UpdateCandidate {
    /// crates.io package name.
    pub crate_name: String,
    /// Installed binary name.
    pub binary: String,
    /// Currently-installed version (`None` if the binary is absent).
    pub installed: Option<String>,
    /// The latest version available on crates.io.
    pub latest: String,
    /// Whether this member is a supervised daemon (drives restart-on-upgrade).
    pub daemon: bool,
}

/// The outcome of the confirm-then-apply gate.
///
/// Why: Separating the *decision* from the *action* lets the upgrade command be
/// tested without spawning `cargo install`; the command pattern-matches on this.
///
/// What: `Apply` — proceed (confirmed via `--yes` or an interactive yes);
/// `Decline` — the user said no at the prompt; `NothingToDo` — there were no
/// candidates; `NeedsConfirmation` — there are candidates but we are
/// non-interactive without `--yes`, so applying is forbidden (the default-safe
/// posture: never mutate without confirmation).
///
/// Test: `tests::decide_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyDecision {
    /// Confirmed — proceed to apply the upgrades.
    Apply,
    /// The user explicitly declined at the interactive prompt.
    Decline,
    /// No candidates — nothing to apply.
    NothingToDo,
    /// Candidates exist but no confirmation is possible (non-TTY, no `--yes`).
    /// Applying is refused; the caller must report the candidates and exit
    /// non-zero so automation does not silently skip an intended upgrade.
    NeedsConfirmation,
}

/// Inputs to the pure confirm-then-apply gate.
///
/// Why: Bundling the inputs keeps [`decide_apply`] a single-argument pure
/// function that is trivial to exhaustively test across the decision matrix.
///
/// What: `has_candidates` — are there any updates to apply; `yes` — the global
/// `--yes` flag (non-interactive auto-confirm); `is_tty` — whether we can prompt;
/// `interactive_consent` — the result of an interactive prompt (only consulted
/// when `yes` is false and `is_tty` is true).
///
/// Test: `tests::decide_*`.
#[derive(Clone, Copy, Debug)]
pub struct ApplyInputs {
    /// Whether any upgrade candidates exist.
    pub has_candidates: bool,
    /// The `--yes` flag (skip the prompt, auto-confirm).
    pub yes: bool,
    /// Whether stdin is an interactive terminal we can prompt on.
    pub is_tty: bool,
    /// The interactive prompt result (consulted only on the TTY, no-`--yes` path).
    pub interactive_consent: bool,
}

/// The pure confirm-then-apply gate (#1316 default-safe posture).
///
/// Why: This is THE safety property of #1316: tctl must never apply an upgrade
/// without confirmation. Encoding the rule as one pure function makes it
/// auditable and exhaustively testable, decoupled from prompts and processes.
///
/// What:
/// 1. No candidates → [`ApplyDecision::NothingToDo`].
/// 2. `--yes` → [`ApplyDecision::Apply`] (explicit non-interactive consent).
/// 3. Not a TTY (and not `--yes`) → [`ApplyDecision::NeedsConfirmation`]
///    (refuse to mutate; cannot obtain consent).
/// 4. TTY → reflect `interactive_consent`: yes → `Apply`, no → `Decline`.
///
/// Test: `tests::decide_nothing_to_do`, `decide_yes_applies`,
/// `decide_non_tty_needs_confirmation`, `decide_tty_yes_applies`,
/// `decide_tty_no_declines`.
pub fn decide_apply(inputs: ApplyInputs) -> ApplyDecision {
    if !inputs.has_candidates {
        return ApplyDecision::NothingToDo;
    }
    if inputs.yes {
        return ApplyDecision::Apply;
    }
    if !inputs.is_tty {
        return ApplyDecision::NeedsConfirmation;
    }
    if inputs.interactive_consent {
        ApplyDecision::Apply
    } else {
        ApplyDecision::Decline
    }
}

/// Probe the installed version of a binary via `<binary> --version`.
///
/// Why: To compute the installed→latest diff we need the version of the
/// currently-installed binary; the trusty-* CLIs (and most cargo binaries) print
/// it via `--version`.
///
/// What: Spawns `<binary> --version`, returns the first whitespace-delimited
/// token that looks like a version (the last token containing a digit and a
/// dot), or `None` if the binary is absent / does not respond.
///
/// Test: `tests::extract_version_from_line` covers the parsing; the spawn is
/// side-effect-only.
pub fn installed_version(binary: &str) -> Option<String> {
    let out = Command::new(binary).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    extract_version_from_line(&line)
}

/// Extract a `MAJOR.MINOR…` version token from a `--version` output line.
///
/// Why: `--version` output varies (`tctl 0.1.0`, `trusty-search 0.20.0 (abc)`);
/// isolating the token extraction makes it unit-testable without a subprocess.
///
/// What: Returns the first whitespace-delimited token that contains a `.` and a
/// leading digit, stripping a leading `v`. Returns `None` when no token matches.
///
/// Test: `tests::extract_version_from_line`.
pub fn extract_version_from_line(line: &str) -> Option<String> {
    line.split_whitespace()
        .map(|t| t.trim_start_matches('v'))
        .find(|t| t.contains('.') && t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|t| t.to_owned())
}

/// Probe crates.io for newer versions of each stable-set member (async).
///
/// Why: `tctl updates` and `tctl upgrade` both need the current set of upgrade
/// candidates; centralising the probe keeps them consistent and reuses the
/// shared `trusty_common::update::check_crates_io` primitive.
///
/// What: For each member, reads the installed version via [`installed_version`]
/// then calls `check_crates_io`. A member with no installed binary uses a `0.0.0`
/// baseline so any published version registers as an available update (install
/// candidate). Returns the members that have a strictly-newer release.
///
/// Test: Side-effecting (network + subprocess); the pure pieces it composes
/// (`decide_apply`, `extract_version_from_line`, `check_crates_io`) are tested
/// independently.
pub async fn gather_candidates(members: &[StableMember]) -> Vec<UpdateCandidate> {
    let mut candidates = Vec::new();
    for m in members {
        let installed = installed_version(&m.binary);
        let baseline = installed.clone().unwrap_or_else(|| "0.0.0".to_owned());
        if let Some(info) = trusty_common::update::check_crates_io(&m.crate_name, &baseline).await {
            candidates.push(UpdateCandidate {
                crate_name: m.crate_name.clone(),
                binary: m.binary.clone(),
                installed,
                latest: info.latest,
                daemon: m.daemon,
            });
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: No candidates must never be reported as "apply".
    /// What: Asserts `NothingToDo` when `has_candidates = false`, regardless of flags.
    /// Test: This is the test.
    #[test]
    fn decide_nothing_to_do() {
        let d = decide_apply(ApplyInputs {
            has_candidates: false,
            yes: true,
            is_tty: true,
            interactive_consent: true,
        });
        assert_eq!(d, ApplyDecision::NothingToDo);
    }

    /// Why: `--yes` is the sanctioned non-interactive auto-confirm.
    /// What: Asserts `Apply` when candidates exist and `yes = true`.
    /// Test: This is the test.
    #[test]
    fn decide_yes_applies() {
        let d = decide_apply(ApplyInputs {
            has_candidates: true,
            yes: true,
            is_tty: false,
            interactive_consent: false,
        });
        assert_eq!(d, ApplyDecision::Apply);
    }

    /// Why: THE safety property — never mutate without confirmation when we
    /// cannot prompt and `--yes` was not given.
    /// What: Asserts `NeedsConfirmation` on a non-TTY without `--yes`.
    /// Test: This is the test.
    #[test]
    fn decide_non_tty_needs_confirmation() {
        let d = decide_apply(ApplyInputs {
            has_candidates: true,
            yes: false,
            is_tty: false,
            interactive_consent: true, // ignored — cannot actually prompt
        });
        assert_eq!(d, ApplyDecision::NeedsConfirmation);
    }

    /// Why: An interactive yes is confirmation.
    /// What: Asserts `Apply` when TTY + consent.
    /// Test: This is the test.
    #[test]
    fn decide_tty_yes_applies() {
        let d = decide_apply(ApplyInputs {
            has_candidates: true,
            yes: false,
            is_tty: true,
            interactive_consent: true,
        });
        assert_eq!(d, ApplyDecision::Apply);
    }

    /// Why: An interactive no must abort the mutation.
    /// What: Asserts `Decline` when TTY + refusal.
    /// Test: This is the test.
    #[test]
    fn decide_tty_no_declines() {
        let d = decide_apply(ApplyInputs {
            has_candidates: true,
            yes: false,
            is_tty: true,
            interactive_consent: false,
        });
        assert_eq!(d, ApplyDecision::Decline);
    }

    /// Why: The version extractor must cope with the varied `--version` shapes.
    /// What: Covers `tctl 0.1.0`, a trailing-paren shape, a leading `v`, and a
    /// line with no version.
    /// Test: This is the test.
    #[test]
    fn extract_version_from_line() {
        assert_eq!(
            super::extract_version_from_line("tctl 0.1.0"),
            Some("0.1.0".to_owned())
        );
        assert_eq!(
            super::extract_version_from_line("trusty-search 0.20.0 (abcdef)"),
            Some("0.20.0".to_owned())
        );
        assert_eq!(
            super::extract_version_from_line("mytool v2.8.0"),
            Some("2.8.0".to_owned())
        );
        assert_eq!(super::extract_version_from_line("no version here"), None);
    }
}
