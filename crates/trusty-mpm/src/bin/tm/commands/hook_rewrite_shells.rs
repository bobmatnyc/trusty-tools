//! The shells the hook-rewrite real-shell tests run under (test-only).
//!
//! See #11658: required CI installs zsh, but the tests dropped a missing zsh
//! leg on every host, so removing that install step would shrink coverage
//! with only a stderr line to show for it. Under `CI` a missing zsh now fails.

/// Whether the zsh leg is dropped (`Ok(false)`), run (`Ok(true)`) or refused.
///
/// Why: a missing zsh must be tolerated on a dev box and fatal in CI.
/// What: a present shell runs; a missing zsh with `ci` set is an error naming
/// the CI step; any other missing shell is dropped.
/// Test: `a_present_shell_runs_in_and_out_of_ci`,
/// `a_missing_zsh_is_dropped_outside_ci`, `a_missing_zsh_fails_in_ci`
pub(crate) fn shell_leg(shell: &str, present: bool, ci: bool) -> Result<bool, String> {
    match (present, ci && shell == "zsh") {
        (true, _) => Ok(true),
        (false, false) => Ok(false),
        (false, true) => Err(
            "no zsh on a CI host: the `Install system dependencies` step in \
             .github/workflows/ci.yml must install zsh (#11658)"
                .to_string(),
        ),
    }
}

/// The shells of `["sh", "bash", "zsh"]` this host can run.
///
/// See #11644: `run_in` panics on a missing shell, so an `exit 0` guard that
/// only skips never worked. The rewrite is POSIX-shaped and every case runs
/// under `sh` and `bash`, so outside CI a missing zsh drops only the zsh leg,
/// named on stderr. `sh` must exist. See #11658: with `CI` set, a missing
/// zsh panics.
pub(crate) fn available_shells() -> Vec<&'static str> {
    let ci = std::env::var_os("CI").is_some_and(|v| !v.is_empty());
    let shells: Vec<&'static str> = ["sh", "bash", "zsh"]
        .into_iter()
        .filter(|shell| {
            let present = std::process::Command::new(shell)
                .args(["-c", "exit 0"])
                .output()
                .is_ok();
            let run = shell_leg(shell, present, ci).unwrap_or_else(|e| panic!("{e}"));
            if !run {
                eprintln!("no {shell} on this host; its leg of the shell parity test is not run");
            }
            run
        })
        .collect();
    assert!(shells.contains(&"sh"), "sh is required: {shells:?}");
    shells
}

#[cfg(test)]
mod tests {
    use super::shell_leg;

    #[test]
    fn a_present_shell_runs_in_and_out_of_ci() {
        for ci in [false, true] {
            assert_eq!(shell_leg("zsh", true, ci), Ok(true));
        }
    }

    #[test]
    fn a_missing_zsh_is_dropped_outside_ci() {
        assert_eq!(shell_leg("zsh", false, false), Ok(false));
    }

    #[test]
    fn a_missing_zsh_fails_in_ci() {
        let err = shell_leg("zsh", false, true).expect_err("CI must not drop zsh");
        assert!(err.contains("Install system dependencies"), "{err}");
        // Only zsh is CI-required; a missing bash is still just dropped.
        assert_eq!(shell_leg("bash", false, true), Ok(false));
    }
}
