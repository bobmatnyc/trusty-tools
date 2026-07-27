//! Read the `pm_guard` kill-switch env vars from the CURRENT process's own
//! environment, at the moment `tm sessions new`/`start`/`resume` runs
//! (issue #3981 Part 2).
//!
//! Why: this is a tiny, shared read-side counterpart to the two env var
//! names `bin/tm/commands/misc.rs`'s `DISABLE_HOOKS_ENV` and
//! `bin/tm/commands/pm_guard.rs`'s `PM_UNRESTRICTED_ENV` name (that crate
//! target is the `tm` binary, not this library, so those constants are not
//! reachable from here — the two literal names are duplicated rather than
//! shared across the binary/library boundary, the same tradeoff already
//! accepted elsewhere for env var names in this codebase). Both the
//! managed-spawn path (`client::executor::managed::managed_new`, THIS crate)
//! and the resume path (`bin/tm/commands/guided_resume.rs`, which depends on
//! this crate) call these helpers so both call the exact same "operator's
//! launching shell, right now" env read, matching the truthy-parsing rule
//! `pm_guard`'s OWN kill switches use.
//! What: [`disable_hooks_requested`] mirrors `DISABLE_HOOKS_ENV`'s
//! presence-only semantics (any value, even empty, counts);
//! [`pm_unrestricted_requested`] mirrors `PM_UNRESTRICTED_ENV`'s exact-`"1"`
//! semantics — see `pm_guard::pm_unrestricted`'s doc for why presence alone
//! is deliberately NOT enough there.
//! Test: `disable_hooks_requested_true_when_set`,
//! `pm_unrestricted_requested_true_only_for_exact_one`.

/// Env var name mirroring `bin/tm/commands/misc.rs::DISABLE_HOOKS_ENV`.
const DISABLE_HOOKS_ENV: &str = "TRUSTY_MPM_DISABLE_HOOKS";

/// Env var name mirroring `bin/tm/commands/pm_guard.rs::PM_UNRESTRICTED_ENV`.
const PM_UNRESTRICTED_ENV: &str = "TRUSTY_MPM_PM_UNRESTRICTED";

/// Whether the CLI's own process has `TRUSTY_MPM_DISABLE_HOOKS` set (any value).
pub fn disable_hooks_requested() -> bool {
    std::env::var_os(DISABLE_HOOKS_ENV).is_some()
}

/// Whether the CLI's own process has `TRUSTY_MPM_PM_UNRESTRICTED` set to
/// exactly `"1"`.
pub fn pm_unrestricted_requested() -> bool {
    std::env::var(PM_UNRESTRICTED_ENV)
        .map(|v| v == "1")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // SAFETY: these tests mutate process-global env vars; serialize them
    // against each other with a lock (mirrors the convention used elsewhere
    // in this crate for env-var tests) so they never race under `cargo test`'s
    // default parallel execution.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn disable_hooks_requested_true_when_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var(DISABLE_HOOKS_ENV);
        }
        assert!(!disable_hooks_requested());
        unsafe {
            std::env::set_var(DISABLE_HOOKS_ENV, "");
        }
        assert!(
            disable_hooks_requested(),
            "presence alone (even empty) must count, mirroring var_os semantics"
        );
        unsafe {
            std::env::remove_var(DISABLE_HOOKS_ENV);
        }
    }

    #[test]
    fn pm_unrestricted_requested_true_only_for_exact_one() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var(PM_UNRESTRICTED_ENV);
        }
        assert!(!pm_unrestricted_requested());
        unsafe {
            std::env::set_var(PM_UNRESTRICTED_ENV, "true");
        }
        assert!(
            !pm_unrestricted_requested(),
            "only the exact value \"1\" must count"
        );
        unsafe {
            std::env::set_var(PM_UNRESTRICTED_ENV, "1");
        }
        assert!(pm_unrestricted_requested());
        unsafe {
            std::env::remove_var(PM_UNRESTRICTED_ENV);
        }
    }
}
