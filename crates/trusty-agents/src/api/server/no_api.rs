//! The `TAGENT_NO_API` off-switch for `tagent --api` / `tagent --serve`.
//!
//! Why (#6433 interim slice, DOC-90 AG-1): a cloud sled sets
//! `TAGENT_NO_API=1`, and the API must then refuse to start before any
//! listener binds or the Telegram gateway starts.
//! What: [`no_api_refusal`] is the pure decision; [`process_no_api_refusal`]
//! reads the process env and calls it. Two sites call it: the clap branch in
//! `runtime::mode_dispatch` (before the gateway) and the top of
//! `routes::serve_with_config`, which the `--api` early dispatch in
//! `runtime::startup` reaches without passing through `mode_dispatch`.
//! Test: `tests::refuses_api_and_serve_under_no_api`,
//! `tests::does_not_refuse_when_unset_empty_or_zero`.

use std::ffi::OsStr;

/// The off-switch: `1` refuses `--api` / `--serve` (DOC-90 AG-1).
pub(crate) const NO_API_ENV: &str = "TAGENT_NO_API";

/// The refusal message for `--api` / `--serve`, or `None` to proceed.
///
/// Why: a pure function over the env value keeps tests off process env
/// (pattern: `runtime::startup_provision::provision_skip`).
/// What: returns `Some(message)` when `no_api` is exactly `1` and either
/// `api` or `serve` is set; `None` otherwise, including unset, empty or `0`.
/// The message names #6433, `TAGENT_NO_API` and the flag.
/// Test: `tests::refuses_api_and_serve_under_no_api`,
/// `tests::does_not_refuse_when_unset_empty_or_zero`.
pub(crate) fn no_api_refusal(no_api: Option<&OsStr>, api: bool, serve: bool) -> Option<String> {
    if !(api || serve) || no_api.and_then(OsStr::to_str) != Some("1") {
        return None;
    }
    let flag = if api { "--api" } else { "--serve" };
    Some(format!(
        "refusing to start tagent {flag}: {NO_API_ENV}=1 disables the HTTP API on this host \
         (#6433, DOC-90 AG-1). Unset {NO_API_ENV} to run the API."
    ))
}

/// [`no_api_refusal`] over this process's `TAGENT_NO_API` value.
/// Test: the decision is covered by `tests::refuses_api_and_serve_under_no_api`.
pub(crate) fn process_no_api_refusal(api: bool, serve: bool) -> Option<String> {
    no_api_refusal(std::env::var_os(NO_API_ENV).as_deref(), api, serve)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: DOC-90 AG-1 — `TAGENT_NO_API=1` refuses both API flags.
    /// What: asserts a refusal naming #6433, the variable and the flag.
    /// Test: this test.
    #[test]
    fn refuses_api_and_serve_under_no_api() {
        for (api, serve, flag) in [(true, false, "--api"), (false, true, "--serve")] {
            let msg = no_api_refusal(Some(OsStr::new("1")), api, serve)
                .unwrap_or_else(|| panic!("{flag} must be refused under TAGENT_NO_API=1"));
            assert!(msg.contains("#6433"), "message must name #6433: {msg}");
            assert!(
                msg.contains(NO_API_ENV),
                "message must name {NO_API_ENV}: {msg}"
            );
            assert!(msg.contains(flag), "message must name {flag}: {msg}");
        }
    }

    /// Why: the switch is off unless set to exactly `1`.
    /// What: unset, empty and `0` never refuse; nor does `1` with no API flag.
    /// Test: this test.
    #[test]
    fn does_not_refuse_when_unset_empty_or_zero() {
        for value in [None, Some(OsStr::new("")), Some(OsStr::new("0"))] {
            for (api, serve) in [(true, false), (false, true)] {
                assert_eq!(
                    no_api_refusal(value, api, serve),
                    None,
                    "value {value:?} must not refuse (api={api}, serve={serve})"
                );
            }
        }
        assert_eq!(no_api_refusal(Some(OsStr::new("1")), false, false), None);
    }
}
