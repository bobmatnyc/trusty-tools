//! The Studio's local presence check: macOS LocalAuthentication (#8878 PR2a).
//!
//! Why: plan.md §3 — presence is proven by Touch ID, or the login password
//! when Touch ID is unavailable, on the Mac that runs tm. A caller with no
//! one at that screen (SSH, no GUI session, Linux) must get `Unavailable` and
//! grant nothing, never a silent allow.
//! What: [`Authenticator`] is the seam `tm presence local` and `tm fleet bind`
//! call; tests pass a fake. [`LocalAuthenticator`] refuses a headless caller
//! ([`headless_reason`]) and otherwise asks `LAContext` to evaluate
//! `LAPolicyDeviceOwnerAuthentication`, waiting at most [`PROMPT_TIMEOUT`].
//! Test: `presence_local_tests.rs` (`an_ssh_caller_is_headless`); the
//! ignored `local_authentication_class_answers_both_selectors` touches the OS
//! without showing a prompt.

use std::ffi::OsString;
use std::time::Duration;

/// How long the Touch ID / password prompt may stay open.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

/// What one presence check came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenceOutcome {
    /// The user authenticated.
    Granted,
    /// The user cancelled, failed, or the prompt timed out.
    Declined(String),
    /// No prompt can reach a user here.
    Unavailable(String),
}

/// Ask the user at this Mac to prove presence.
pub trait Authenticator {
    /// Show `reason` in the prompt and report the answer.
    fn authenticate(&self, reason: &str) -> PresenceOutcome;
}

/// Why no presence prompt can reach a user, from the environment `var` reads.
///
/// What: an SSH session (`SSH_CONNECTION`, `SSH_CLIENT`, `SSH_TTY`) is
/// headless: the prompt would show on a screen the caller is not at.
/// Test: `an_ssh_caller_is_headless`.
pub fn headless_reason(var: impl Fn(&str) -> Option<OsString>) -> Option<&'static str> {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .into_iter()
        .any(|name| var(name).is_some_and(|v| !v.is_empty()))
        .then_some("this session runs over SSH, so no one at the Studio's screen can answer")
}

/// The Studio's LocalAuthentication prompt.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalAuthenticator;

impl Authenticator for LocalAuthenticator {
    fn authenticate(&self, reason: &str) -> PresenceOutcome {
        if let Some(why) = headless_reason(|name| std::env::var_os(name)) {
            return PresenceOutcome::Unavailable(why.to_owned());
        }
        platform::evaluate(reason)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::CString;

    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool};

    use super::{PROMPT_TIMEOUT, PresenceOutcome};

    #[link(name = "LocalAuthentication", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {}

    /// `LAPolicyDeviceOwnerAuthentication`: biometrics, else the password.
    const DEVICE_OWNER_AUTHENTICATION: isize = 2;

    fn unavailable(why: &str) -> PresenceOutcome {
        PresenceOutcome::Unavailable(why.to_owned())
    }

    /// A new `LAContext`, when the framework loaded.
    pub(super) fn context() -> Option<Retained<AnyObject>> {
        let class = AnyClass::get(c"LAContext")?;
        // SAFETY: `+new` on an NSObject subclass returns a retained instance.
        unsafe { msg_send![class, new] }
    }

    /// Whether the policy can be evaluated now (no prompt is shown).
    pub(super) fn can_evaluate(context: &AnyObject) -> bool {
        let no_error: *mut *mut AnyObject = std::ptr::null_mut();
        // SAFETY: `canEvaluatePolicy:error:` takes an LAPolicy and a nullable
        // `NSError **`, and returns BOOL.
        let can: Bool = unsafe {
            msg_send![context, canEvaluatePolicy: DEVICE_OWNER_AUTHENTICATION, error: no_error]
        };
        can.as_bool()
    }

    pub(super) fn evaluate(reason: &str) -> PresenceOutcome {
        let Some(context) = context() else {
            return unavailable("LocalAuthentication is not available");
        };
        if !can_evaluate(&context) {
            return unavailable(
                "this Mac cannot show a Touch ID or password prompt now (no GUI session)",
            );
        }
        let (Ok(text), Some(string_class)) = (CString::new(reason), AnyClass::get(c"NSString"))
        else {
            return unavailable("the prompt text cannot be built");
        };
        // SAFETY: `+stringWithUTF8String:` copies a NUL-terminated UTF-8 string.
        let reason: Option<Retained<AnyObject>> =
            unsafe { msg_send![string_class, stringWithUTF8String: text.as_ptr()] };
        let Some(reason) = reason else {
            return unavailable("the prompt text cannot be built");
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let reply = RcBlock::new(move |ok: Bool, _error: *mut AnyObject| {
            let _ = tx.send(ok.as_bool());
        });
        // SAFETY: `evaluatePolicy:localizedReason:reply:` takes an LAPolicy, an
        // NSString and a `void (^)(BOOL, NSError *)` block, which it copies.
        let () = unsafe {
            msg_send![&*context, evaluatePolicy: DEVICE_OWNER_AUTHENTICATION, localizedReason: &*reason, reply: &*reply]
        };
        match rx.recv_timeout(PROMPT_TIMEOUT) {
            Ok(true) => PresenceOutcome::Granted,
            Ok(false) => PresenceOutcome::Declined("the prompt was cancelled or failed".to_owned()),
            Err(_) => PresenceOutcome::Declined("the prompt was not answered in time".to_owned()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::PresenceOutcome;

    pub(super) fn evaluate(_: &str) -> PresenceOutcome {
        PresenceOutcome::Unavailable("LocalAuthentication exists only on macOS".to_owned())
    }
}

#[cfg(test)]
#[path = "presence_local_tests.rs"]
mod tests;
