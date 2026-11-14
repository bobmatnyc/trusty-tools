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

/// The off-switch: any set value refuses `--api` / `--serve` except an
/// explicit off value (DOC-90 AG-1). The sled image sets it to `1`.
pub(crate) const NO_API_ENV: &str = "TAGENT_NO_API";

/// Whether `value` turns the switch off: unset, empty, or `0`/`false`/`no`/
/// `off` after trimming, in any case. A non-UTF-8 value is not off.
fn no_api_off(value: Option<&OsStr>) -> bool {
    let Some(value) = value else { return true };
    let Some(text) = value.to_str() else {
        return false;
    };
    let text = text.trim();
    text.is_empty()
        || ["0", "false", "no", "off"]
            .iter()
            .any(|off| text.eq_ignore_ascii_case(off))
}

/// The refusal message for `--api` / `--serve`, or `None` to proceed.
///
/// Why: a security switch must fail closed — a typo or `true` must not leave
/// the API on (#6433). A pure function over the env value keeps tests off
/// process env (pattern: `runtime::startup_provision::provision_skip`).
/// What: returns `Some(message)` when `api` or `serve` is set and `no_api` is
/// set to anything but an explicit off value (see `no_api_off`); `None`
/// otherwise. The message names #6433, `TAGENT_NO_API` and the flag:
/// `--api/--serve` when both are passed, as `serve_with_config` does.
/// Test: `tests::refuses_api_and_serve_under_no_api`,
/// `tests::does_not_refuse_when_unset_empty_or_zero`.
pub(crate) fn no_api_refusal(no_api: Option<&OsStr>, api: bool, serve: bool) -> Option<String> {
    if !(api || serve) || no_api_off(no_api) {
        return None;
    }
    let flag = match (api, serve) {
        (true, true) => "--api/--serve",
        (true, false) => "--api",
        _ => "--serve",
    };
    Some(format!(
        "refusing to start tagent {flag}: {NO_API_ENV} is set, which disables the HTTP API on \
         this host (#6433, DOC-90 AG-1). Unset it, or set it to 0, to run the API."
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

    use std::ffi::OsString;

    /// Every set value that must refuse: on-words, padded `1`, any other
    /// string and, on Unix, a non-UTF-8 value.
    fn refusing_values() -> Vec<OsString> {
        let mut values: Vec<OsString> = ["1", "true", "TRUE", "yes", "on", " 1", "1\n", "enabled"]
            .into_iter()
            .map(OsString::from)
            .collect();
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            values.push(OsStr::from_bytes(&[0xff_u8, 0xfe_u8]).to_os_string());
        }
        values
    }

    /// Why: DOC-90 AG-1 — the switch fails closed: any set value other than
    /// an explicit off value refuses both API flags.
    /// What: for every refusing value and each flag shape, asserts a refusal
    /// naming #6433, the variable and the flag; collects every miss so one
    /// run reports them all.
    /// Test: this test.
    #[test]
    fn refuses_api_and_serve_under_no_api() {
        let mut misses = Vec::new();
        for value in refusing_values() {
            for (api, serve, flag) in [
                (true, false, "--api"),
                (false, true, "--serve"),
                (true, true, "--api/--serve"),
            ] {
                match no_api_refusal(Some(value.as_os_str()), api, serve) {
                    None => misses.push(format!("{value:?} {flag}: not refused")),
                    Some(msg)
                        if !(msg.contains("#6433")
                            && msg.contains(NO_API_ENV)
                            && msg.contains(flag)) =>
                    {
                        misses.push(format!("{value:?} {flag}: message lacks parts: {msg}"))
                    }
                    Some(_) => {}
                }
            }
        }
        assert!(misses.is_empty(), "fail-open cases:\n{}", misses.join("\n"));
    }

    /// Why: only unset, empty or an explicit off word turns the switch off.
    /// What: unset, empty, `0`/`false`/`no`/`off` in any case and padding
    /// never refuse; nor does a refusing value with no API flag.
    /// Test: this test.
    #[test]
    fn does_not_refuse_when_unset_empty_or_zero() {
        let off = ["", "0", "false", "no", "off", " OFF ", "False", "0\n", "No"];
        let values = std::iter::once(None).chain(off.iter().map(|v| Some(OsStr::new(v))));
        for value in values {
            for (api, serve) in [(true, false), (false, true), (true, true)] {
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
