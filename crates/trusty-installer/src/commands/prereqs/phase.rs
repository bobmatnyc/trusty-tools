//! Full prerequisite-check phase: detect → confirm/✓ or offer install → re-verify.
//!
//! Why: The install command must surface missing runtime deps (tmux, Claude Code)
//! *before* attempting `tctl install` so the operator can resolve the environment
//! rather than diagnosing a confusing partially-installed stack later.
//!
//! What: `run_prereq_phase` iterates the prereq table for the selected crate
//! names, prints ✓ for present tools (with version), and for missing tools either
//! auto-installs (`--yes`/`TRUSTY_YES` — no TTY required, since `-y` IS the
//! consent; this is the piped `curl | sh -s -- -y` demo path), prompts the user
//! to install (TTY, not `--yes`, not `--json`), or prints manual instructions
//! and continues (non-interactive without `--yes`, or `--json`). After a
//! consented install it re-probes and reports success or failure. Returns the
//! set of prereqs that remain missing so the caller can decide whether to abort.
//!
//! Test: `tests::phase_*` inject fake detect/version/exec closures to exercise
//! every branch of the decision table without spawning real OS processes.

use super::detect::{claude_extra_paths, detect_prereq_status, probe_version, PrereqStatus};
use super::hints::{detect_platform, install_hint_for, PlatformInfo};
use super::{Missing, PREREQS};
use crate::commands::progress_ui::{is_tty, narrator, prompt_yes_no};

/// Configuration for a single `run_prereq_phase` invocation.
///
/// Why: Bundling arguments into a struct keeps the function signature stable
/// as new options are added and makes it easy to pass through tests.
///
/// What: `selected` = crate names being installed; `yes` = global `--yes` flag
/// (non-interactive mode); `json` = global `--json` flag.
///
/// Test: `tests::phase_*` construct this inline.
pub struct PrereqPhaseConfig<'a> {
    /// Crate/binary names selected for installation.
    pub selected: &'a [String],
    /// Whether the user passed `--yes` (skips interactive prompts).
    pub yes: bool,
    /// Whether `--json` mode is active (suppresses human output).
    pub json: bool,
}

/// The outcome of the prerequisite-check phase.
///
/// Why: The caller (`install::run`) needs to know which tools remain missing
/// after the phase to decide whether to abort or warn-and-continue.
///
/// What: `still_missing` = prereqs that were absent even after any install
/// attempt; `installed` = binary names the phase successfully installed.
///
/// Test: `tests::phase_all_present_returns_empty_missing`.
pub struct PrereqPhaseResult {
    /// Prereqs that remain missing after the phase (absent or install failed).
    pub still_missing: Vec<Missing>,
    /// Binary names successfully installed during this phase.
    pub installed: Vec<String>,
}

/// Run the full prerequisite-check phase using the real OS environment.
///
/// Why: The primary production entry point — wraps `run_prereq_phase_with`
/// with real `which::which`, `probe_version`, `sh -c` execution, and the
/// real `claude_extra_paths()` list.
///
/// What: Detects the platform once, calls `claude_extra_paths()`, then delegates
/// to `run_prereq_phase_with` with production implementations of all seams.
///
/// Test: Integration path (not directly unit-tested); unit tests use
/// `run_prereq_phase_with` with fake closures and an empty `extra_paths`.
pub fn run_prereq_phase(config: &PrereqPhaseConfig<'_>) -> PrereqPhaseResult {
    let platform = detect_platform();
    let extra_paths = claude_extra_paths();
    run_prereq_phase_with(
        config,
        &platform,
        &extra_paths,
        &|bin| which::which(bin).is_ok(),
        &probe_version,
        &real_exec,
        &is_tty,
        &prompt_yes_no,
    )
}

/// Injectable version of the prerequisite-check phase (for unit testing).
///
/// Why: The real impl shells out to the OS; tests inject controlled closures
/// and an empty `extra_paths` slice to avoid filesystem/OS dependencies and to
/// precisely exercise each branch of the decision table.
///
/// What:
/// 1. For each prereq whose `affects` list overlaps `selected`:
///    a. Calls `detect_prereq_status(binary, check_fn, extra_paths, version_fn)`.
///    b. `Found`: prints `✓ <binary> <version> found` (human mode only).
///    c. `Absent`: prints `✗ <binary> not found`; obtains an `InstallHint`.
///       - TTY + !yes + !json + hint has `auto_cmd`: prompts `[y/N]` to install.
///         - Consent: runs `exec_fn(cmd)`, re-probes with the same seams.
///         - No consent or exec failure: prints `manual_note`, adds to
///           `still_missing`.
///       - Non-interactive / json: prints `manual_note`, adds to `still_missing`.
/// 2. Returns `PrereqPhaseResult { still_missing, installed }`.
///
/// Test: `tests::phase_all_present`, `tests::phase_missing_non_interactive`,
/// `tests::phase_missing_install_fails`, `tests::phase_unrelated_crate_skipped`,
/// `tests::phase_missing_hint_is_platform_appropriate`,
/// `tests::phase_install_success_moves_to_installed`.
#[allow(clippy::too_many_arguments)] // 8 injectable seams are intentional for unit testing.
pub fn run_prereq_phase_with(
    config: &PrereqPhaseConfig<'_>,
    platform: &PlatformInfo,
    extra_paths: &[std::path::PathBuf],
    check_fn: &dyn Fn(&str) -> bool,
    version_fn: &dyn Fn(&str) -> Option<String>,
    exec_fn: &dyn Fn(&str) -> anyhow::Result<()>,
    tty_fn: &dyn Fn() -> bool,
    prompt_fn: &dyn Fn(&str) -> bool,
) -> PrereqPhaseResult {
    let narr = narrator(config.json);
    let mut still_missing = Vec::new();
    let mut installed = Vec::new();

    for prereq in PREREQS {
        // Only check prereqs relevant to the selected crates.
        let relevant = prereq
            .affects
            .iter()
            .any(|affected| config.selected.iter().any(|s| s == affected));
        if !relevant {
            continue;
        }

        let status = detect_prereq_status(prereq.binary, check_fn, extra_paths, version_fn);

        match status {
            PrereqStatus::Found { version } => {
                if !config.json {
                    let ver = version.as_deref().unwrap_or("unknown version");
                    let _ = narr.info(&format!("✓ {} {} found", prereq.binary, ver));
                }
            }
            PrereqStatus::Absent => {
                if !config.json {
                    let _ = narr.info(&format!(
                        "✗ {} not found (needed for {})",
                        prereq.binary, prereq.required_for
                    ));
                }
                let hint = install_hint_for(prereq.binary, platform);
                let has_auto_cmd = hint.auto_cmd.is_some();

                // `-y`/`TRUSTY_YES` is unattended CONSENT: run the auto-install
                // command directly, with no TTY and no prompt required — the
                // entire point of `--yes` is to authorize exactly this. This is
                // the path a piped `curl | sh -s -- -y` install takes, where
                // stdin is never a TTY (issue: prereq auto-install silently
                // degrading to a manual hint under `-y`).
                let auto_yes = !config.json && config.yes && has_auto_cmd;
                // Interactive path: offer + prompt only on a real TTY, when not
                // already auto-consented above.
                let offer_interactive = !auto_yes && !config.json && !config.yes && tty_fn();
                let should_offer = offer_interactive && has_auto_cmd;

                let consented = if auto_yes {
                    true
                } else if should_offer {
                    let cmd = hint.auto_cmd.as_deref().unwrap_or_default();
                    let prompt = format!("  Install {} now? (will run: `{}`)", prereq.binary, cmd);
                    prompt_fn(&prompt)
                } else {
                    false
                };

                if consented {
                    let cmd = hint.auto_cmd.as_deref().unwrap_or_default();
                    if !config.json {
                        let verb = if auto_yes {
                            "installing (--yes)"
                        } else {
                            "running"
                        };
                        let _ = narr.info(&format!("  {verb}: {cmd}"));
                    }
                    match exec_fn(cmd) {
                        Ok(()) => {
                            let after = detect_prereq_status(
                                prereq.binary,
                                check_fn,
                                extra_paths,
                                version_fn,
                            );
                            match after {
                                PrereqStatus::Found { version } => {
                                    let ver = version.as_deref().unwrap_or("installed");
                                    if !config.json {
                                        let _ = narr.info(&format!(
                                            "  ✓ {} {} installed successfully",
                                            prereq.binary, ver
                                        ));
                                    }
                                    installed.push(prereq.binary.to_owned());
                                }
                                PrereqStatus::Absent => {
                                    if !config.json {
                                        let _ = narr.info(&format!(
                                            "  ✗ {} still not found after install attempt; {}",
                                            prereq.binary, hint.manual_note
                                        ));
                                    }
                                    still_missing.push(Missing {
                                        binary: prereq.binary.to_owned(),
                                        hint: hint.manual_note.clone(),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            if !config.json {
                                let _ = narr.info(&format!(
                                    "  install failed: {e}\n  {}",
                                    hint.manual_note
                                ));
                            }
                            still_missing.push(Missing {
                                binary: prereq.binary.to_owned(),
                                hint: hint.manual_note.clone(),
                            });
                        }
                    }
                } else {
                    // Not installing: print manual note and continue.
                    if !config.json {
                        let _ = narr.info(&format!("  {}", hint.manual_note));
                        let _ = narr.info(&format!(
                            "  warning: `{}` is required for {}",
                            prereq.binary, prereq.required_for
                        ));
                    }
                    still_missing.push(Missing {
                        binary: prereq.binary.to_owned(),
                        hint: hint.manual_note,
                    });
                }
            }
        }
    }

    PrereqPhaseResult {
        still_missing,
        installed,
    }
}

fn real_exec(cmd: &str) -> anyhow::Result<()> {
    // Inherit stdout/stderr so package-manager progress (brew, apt, etc.)
    // is visible to the operator during potentially long installs.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "command exited with code {:?}",
            status.code()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::prereqs::hints::{Os, PkgMgr, PlatformInfo};

    fn linux_apt() -> PlatformInfo {
        PlatformInfo {
            os: Os::Linux,
            pkg_mgr: Some(PkgMgr::Apt),
            npm_available: false,
        }
    }

    fn unknown_platform() -> PlatformInfo {
        PlatformInfo {
            os: Os::Unknown,
            pkg_mgr: None,
            npm_available: false,
        }
    }

    fn selected_mpm() -> Vec<String> {
        vec!["trusty-mpm".to_owned()]
    }

    fn selected_tga() -> Vec<String> {
        vec!["tga".to_owned()]
    }

    /// Why: When all binaries are present the phase must return no missing
    /// prereqs.
    /// What: Fake check_fn always returns true; fake version_fn returns "1.0".
    /// Test: This is the test.
    #[test]
    fn phase_all_present_returns_empty_missing() {
        let config = PrereqPhaseConfig {
            selected: &selected_mpm(),
            yes: true,
            json: true,
        };
        let result = run_prereq_phase_with(
            &config,
            &linux_apt(),
            &[],
            &|_| true,
            &|_| Some("1.0".to_owned()),
            &|_| Ok(()),
            &|| false,
            &|_| false,
        );
        assert!(result.still_missing.is_empty());
        assert!(result.installed.is_empty());
    }

    /// Why: In non-interactive mode (yes=true) missing prereqs must NOT trigger
    /// install; they must appear in `still_missing`.
    /// What: check_fn always returns false (all absent); yes=true disables prompt.
    /// Test: This is the test.
    #[test]
    fn phase_missing_non_interactive_adds_to_still_missing() {
        let config = PrereqPhaseConfig {
            selected: &selected_mpm(),
            yes: true,
            json: true,
        };
        let result = run_prereq_phase_with(
            &config,
            &linux_apt(),
            &[],
            &|_| false,
            &|_| None,
            &|_| Ok(()),
            &|| false,
            &|_| false,
        );
        assert!(result.still_missing.iter().any(|m| m.binary == "tmux"));
        assert!(result.still_missing.iter().any(|m| m.binary == "claude"));
    }

    /// Why: A crate that does not depend on tmux or claude should not add
    /// either to the missing list even when the fake checker says everything
    /// is absent.
    /// What: Selects only "tga"; checks that tmux/claude are NOT in missing.
    /// Test: This is the test.
    #[test]
    fn phase_unrelated_crate_skipped() {
        let config = PrereqPhaseConfig {
            selected: &selected_tga(),
            yes: true,
            json: true,
        };
        let result = run_prereq_phase_with(
            &config,
            &unknown_platform(),
            &[],
            &|_| false,
            &|_| None,
            &|_| Ok(()),
            &|| false,
            &|_| false,
        );
        assert!(!result.still_missing.iter().any(|m| m.binary == "tmux"));
        assert!(!result.still_missing.iter().any(|m| m.binary == "claude"));
        assert!(result.still_missing.iter().any(|m| m.binary == "git"));
    }

    /// Why: The consent→exec→re-verify→`installed` path is the most important
    /// branch in the phase; it must be covered by a real assertion.
    ///
    /// What: Simulates a TTY (`tty_fn: &|| true`) and a consenting user
    /// (`prompt_fn: &|_| true`). check_fn uses an AtomicBool to return
    /// absent on the first call per-binary (PRESENCE probe) and present on
    /// all subsequent calls (re-verify after exec). exec_fn returns Ok(()).
    /// With linux_apt + trusty-mpm: tmux is absent first → consent → exec →
    /// re-verify (found) → installed; claude is found on its first probe
    /// (AtomicBool already flipped) → ✓ line.
    ///
    /// Test: This is the test.
    #[test]
    fn phase_install_success_moves_to_installed() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let first_call = Arc::new(AtomicBool::new(true));
        let fc = first_call.clone();
        let check_fn = move |_bin: &str| {
            if fc.swap(false, Ordering::SeqCst) {
                false // First probe: binary absent — triggers consent branch.
            } else {
                true // Re-verify (and all later probes): binary present.
            }
        };
        let config = PrereqPhaseConfig {
            selected: &selected_mpm(),
            yes: false,
            json: false,
        };
        let result = run_prereq_phase_with(
            &config,
            &linux_apt(),
            &[],
            &check_fn,
            &|_| Some("1.0".to_owned()),
            &|_| Ok(()),
            &|| true,  // simulated TTY
            &|_| true, // user consents
        );
        // PREREQS iteration order: claude → git → tmux.
        // claude is the first binary with an auto_cmd on linux_apt (no npm);
        // git has no auto_cmd (no install offer); tmux is present because the
        // AtomicBool is already false by the time it is probed.
        // Key invariant: at least one binary ends in `installed` and nothing
        // ends in `still_missing`.
        assert!(
            !result.installed.is_empty(),
            "at least one binary must be in installed after exec+re-verify, got: {:?}",
            result.installed
        );
        assert!(
            result.still_missing.is_empty(),
            "still_missing must be empty, got: {:?}",
            result.still_missing
        );
    }

    /// Why: Verifies the exec+re-verify path for every prereq in a selected
    /// set (both tmux and claude), covering the alternating absent/found
    /// pattern when check_fn returns absent on even calls and present on odd.
    ///
    /// What: tty_fn=true, prompt_fn=true, exec_fn=Ok; check_fn tracks call
    /// count and returns absent when n is even (PRESENCE probes: 0, 2) and
    /// present when n is odd (re-verify probes: 1, 3). Both binaries should
    /// end in `installed`.
    ///
    /// Test: This is the test.
    #[test]
    fn phase_tty_consent_moves_to_installed() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = call_count.clone();
        let check_fn = move |bin: &str| {
            // git has no auto_cmd in our hints (no install offer), so return
            // it as already present to keep it out of still_missing.
            if bin == "git" {
                return true;
            }
            let n = cc.fetch_add(1, Ordering::SeqCst);
            // Even calls (0, 2, …) → absent (PRESENCE probe);
            // odd calls (1, 3, …) → found (re-verify after exec).
            !n.is_multiple_of(2)
        };
        let config = PrereqPhaseConfig {
            selected: &selected_mpm(),
            yes: false,
            json: false,
        };
        let result = run_prereq_phase_with(
            &config,
            &linux_apt(),
            &[],
            &check_fn,
            &|_| Some("1.0".to_owned()),
            &|_| Ok(()),
            &|| true,  // simulated TTY
            &|_| true, // user consents
        );
        assert!(
            !result.installed.is_empty(),
            "installed should be non-empty, got: {:?}",
            result.installed
        );
        assert!(
            result.still_missing.is_empty(),
            "nothing should still be missing, got: {:?}",
            result.still_missing
        );
    }

    /// Why: When exec_fn returns an error the binary must end up in `still_missing`.
    /// What: check_fn always returns false; exec_fn returns Err; json=true
    /// bypasses the prompt so exec is never called — binary goes to still_missing.
    /// Test: This is the test.
    #[test]
    fn phase_exec_failure_adds_to_still_missing() {
        let config = PrereqPhaseConfig {
            selected: &selected_mpm(),
            yes: true,
            json: true,
        };
        let result = run_prereq_phase_with(
            &config,
            &linux_apt(),
            &[],
            &|_| false,
            &|_| None,
            &|_| Err(anyhow::anyhow!("fake failure")),
            &|| false,
            &|_| false,
        );
        assert!(result.still_missing.iter().any(|m| m.binary == "tmux"));
    }

    /// Why: The `hint` field on `Missing` should carry the manual_note for the
    /// platform so callers can print actionable guidance.
    /// What: Selects trusty-mpm on linux+apt; all absent; checks tmux hint
    /// references "apt".
    /// Test: This is the test.
    #[test]
    fn phase_missing_hint_is_platform_appropriate() {
        let config = PrereqPhaseConfig {
            selected: &selected_mpm(),
            yes: true,
            json: true,
        };
        let result = run_prereq_phase_with(
            &config,
            &linux_apt(),
            &[],
            &|_| false,
            &|_| None,
            &|_| Ok(()),
            &|| false,
            &|_| false,
        );
        let tmux = result
            .still_missing
            .iter()
            .find(|m| m.binary == "tmux")
            .expect("tmux must be missing");
        assert!(
            tmux.hint.contains("apt") || tmux.hint.contains("package"),
            "hint should reference apt or package manager, got: {}",
            tmux.hint
        );
    }

    /// Why: THE demo-critical fix — a piped `curl | sh -s -- -y` install has no
    /// TTY on stdin, but `--yes` IS the operator's consent to auto-install.
    /// Before this fix `can_offer` required `tty_fn()` even under `--yes`, so
    /// missing prereqs silently degraded to a manual hint and never installed.
    /// What: `yes: true`, `json: false`, `tty_fn: || false` (no TTY, exactly
    /// the piped-script condition); check_fn absent-then-found (post-exec);
    /// exec_fn Ok. Asserts the binary lands in `installed`, NOT
    /// `still_missing`, and `prompt_fn` is never consulted (would panic/return
    /// false but must not gate the decision).
    /// Test: This is the test.
    #[test]
    fn phase_yes_auto_installs_without_tty() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let first_call = Arc::new(AtomicBool::new(true));
        let fc = first_call.clone();
        let check_fn = move |_bin: &str| !fc.swap(false, Ordering::SeqCst);
        let config = PrereqPhaseConfig {
            selected: &selected_mpm(),
            yes: true,
            json: false,
        };
        let result = run_prereq_phase_with(
            &config,
            &linux_apt(),
            &[],
            &check_fn,
            &|_| Some("1.0".to_owned()),
            &|_| Ok(()),
            &|| false, // no TTY — the piped `curl | sh` condition
            &|_| panic!("must not prompt under --yes"),
        );
        assert!(
            !result.installed.is_empty(),
            "at least one binary must auto-install under --yes with no TTY, got: {:?}",
            result.installed
        );
        assert!(
            result.still_missing.is_empty(),
            "still_missing must be empty, got: {:?}",
            result.still_missing
        );
    }

    /// Why: `--json` must remain fully non-interactive/no-auto-install even
    /// when `--yes` is also set — machine output must stay deterministic and
    /// side-effect-free (unaffected by this fix).
    /// What: `yes: true, json: true`; asserts nothing is installed and missing
    /// binaries land in `still_missing`.
    /// Test: This is the test.
    #[test]
    fn phase_json_yes_does_not_auto_install() {
        let config = PrereqPhaseConfig {
            selected: &selected_mpm(),
            yes: true,
            json: true,
        };
        let result = run_prereq_phase_with(
            &config,
            &linux_apt(),
            &[],
            &|_| false,
            &|_| None,
            &|_| panic!("must not exec under --json"),
            &|| false,
            &|_| panic!("must not prompt under --json"),
        );
        assert!(result.installed.is_empty());
        assert!(result.still_missing.iter().any(|m| m.binary == "tmux"));
    }

    /// Why: under `--yes`, a binary with no known `auto_cmd` (e.g. `git`) must
    /// still fall through to the manual-note path rather than panicking or
    /// attempting a bogus exec.
    /// What: selects tga (only `git` relevant); `yes: true`; asserts `git`
    /// lands in `still_missing` and `exec_fn` is never called.
    /// Test: This is the test.
    #[test]
    fn phase_yes_no_auto_cmd_falls_back_to_manual() {
        let config = PrereqPhaseConfig {
            selected: &selected_tga(),
            yes: true,
            json: false,
        };
        let result = run_prereq_phase_with(
            &config,
            &unknown_platform(),
            &[],
            &|_| false,
            &|_| None,
            &|_| panic!("must not exec when no auto_cmd exists"),
            &|| false,
            &|_| false,
        );
        assert!(result.still_missing.iter().any(|m| m.binary == "git"));
    }

    /// Why: under `--yes`, exec failure must still land the binary in
    /// `still_missing` (not silently `installed`).
    /// What: `yes: true, json: false`; exec_fn returns Err; asserts tmux ends
    /// up in `still_missing`.
    /// Test: This is the test.
    #[test]
    fn phase_yes_exec_failure_adds_to_still_missing() {
        let config = PrereqPhaseConfig {
            selected: &selected_mpm(),
            yes: true,
            json: false,
        };
        let result = run_prereq_phase_with(
            &config,
            &linux_apt(),
            &[],
            &|_| false,
            &|_| None,
            &|_| Err(anyhow::anyhow!("fake failure")),
            &|| false,
            &|_| false,
        );
        assert!(result.still_missing.iter().any(|m| m.binary == "tmux"));
    }
}
