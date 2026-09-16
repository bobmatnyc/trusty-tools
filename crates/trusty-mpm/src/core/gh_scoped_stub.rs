//! In-process stand-in for the scoped `gh` subprocesses `gh_account_enforce`
//! spawns, so its tests never touch the real binary (#7059).
//!
//! Why: the `ensure_gh_account_in_dir` tests used to install a fake `gh` shell
//! script on the process-global `PATH`. Two regimes mutate `PATH` in this test
//! binary, so a sibling test could restore a `PATH` snapshot taken before the
//! fake directory was prepended; `Command::new("gh")` then found the REAL `gh`,
//! `gh auth status` went to the network, and the 5 s `GH_ENFORCE_TIMEOUT`
//! turned into `gh auth status did not respond within 5s`. That flake outlived
//! two fixes and recurred five times, because each addressed the lock
//! discipline rather than the dependency on a global `PATH` and a real
//! subprocess. This removes both: no `PATH` mutation, no subprocess, no
//! network — each enforcement test answers in microseconds instead of racing a
//! 5 s ceiling.
//!
//! 🔴 Compiled ONLY under `cfg(any(test, debug_assertions))`. A `--release`
//! build — what `cargo install` produces, and therefore everything that ships —
//! contains neither this module nor the read of it in the enforcement path, so
//! the production path is what it was. Nothing installs a stub from
//! configuration, an environment variable, or a file; the only way in is a Rust
//! caller holding [`GhScopedStub::install`], which is what makes the debug-build
//! exposure a test seam rather than an auth bypass.
//!
//! What: [`GhScopedStub`] answers the three invocations `gh_account_enforce`
//! makes — `gh auth status`, `gh api user --jq .login`, and
//! `gh auth switch --user <login>` — from in-memory state the switch mutates,
//! exactly as the fake shell script did via a file inside the config dir.
//! [`GhScopedStub::install`] publishes one for the process and returns a guard
//! that clears it on drop, so a panicking test cannot leak it into the next.
//! Test: `ensure_gh_account_in_dir_self_heals_mismatch`,
//! `ensure_gh_account_in_dir_accepts_the_api_answer_over_a_stale_transcript`,
//! `resolve_for_config_enforced_self_heals_mismatch` and
//! `resolve_project_aware_enforces_paired_account` all drive enforcement
//! through it.

use std::sync::{Arc, Mutex, RwLock};

/// The scripted `gh` answers one test needs (#7059).
///
/// Why: the fake shell script this replaces carried its knobs as `FAKE_GH_*`
/// environment variables — process-global state of exactly the kind this module
/// exists to remove. They are plain fields here, owned by the installing test.
/// What: `active` is the login `gh auth status` reports and `gh auth switch`
/// rewrites; the other fields script the `gh api user` probe and the switch
/// failure mode. Built with [`Self::logged_in_as`] plus the `with_*` builders.
/// Test: see the module docs.
#[derive(Debug)]
pub struct GhScopedStub {
    active: Mutex<String>,
    api_login: Option<String>,
    api_fails: bool,
    api_empty: bool,
    switch_fails: bool,
}

impl GhScopedStub {
    /// An honest machine: transcript, credential and switch all agree on
    /// `active`.
    pub fn logged_in_as(active: &str) -> Self {
        Self {
            active: Mutex::new(active.to_string()),
            api_login: None,
            api_fails: false,
            api_empty: false,
            switch_fails: false,
        }
    }

    /// Make `gh api user` answer `login` regardless of the transcript — the
    /// #5849 shape where the credential and the config dir disagree.
    pub fn with_api_login(mut self, login: &str) -> Self {
        self.api_login = Some(login.to_string());
        self
    }

    /// Make `gh api user` exit non-zero: an unreachable or unauthenticated
    /// probe.
    pub fn with_failing_api(mut self) -> Self {
        self.api_fails = true;
        self
    }

    /// Make `gh api user` exit zero printing nothing — the blank-login answer
    /// enforcement must reject.
    pub fn with_blank_api(mut self) -> Self {
        self.api_empty = true;
        self
    }

    /// Make `gh auth switch` fail without changing the active account.
    pub fn with_failing_switch(mut self) -> Self {
        self.switch_fails = true;
        self
    }

    /// Publish this stub for the current process.
    ///
    /// Why: an RAII guard rather than a paired `clear` call, so a test that
    /// panics mid-assertion still leaves the next test looking at the real
    /// enforcement path instead of its predecessor's scripted answers.
    /// What: returns the shared handle (for asserting [`Self::active_account`]
    /// afterwards) and the guard whose `Drop` uninstalls it.
    /// Test: see the module docs.
    pub fn install(self) -> (Arc<Self>, GhStubGuard) {
        let stub = Arc::new(self);
        *INSTALLED.write().expect("gh stub lock") = Some(stub.clone());
        (stub, GhStubGuard)
    }

    /// The login currently active — what a test asserts a self-heal produced,
    /// replacing the `.fake_active` file the shell script wrote.
    pub fn active_account(&self) -> String {
        self.active.lock().expect("stub active lock").clone()
    }

    /// The `gh auth status` transcript for the active account, in the shape
    /// `parse_active_account_from_status` parses.
    pub(crate) fn auth_status(&self) -> String {
        let active = self.active_account();
        format!(
            "github.com\n  - Logged in to github.com account {active} (keyring)\n  \
             - Active account: true\n"
        )
    }

    /// The `gh api user --jq .login` answer, including its failure modes.
    pub(crate) fn api_login(&self) -> Result<String, String> {
        if self.api_fails {
            return Err("`gh api user --jq .login` failed: stub gh: api refused".to_string());
        }
        if self.api_empty {
            return Err("`gh api user --jq .login` failed: empty stdout".to_string());
        }
        Ok(self
            .api_login
            .clone()
            .unwrap_or_else(|| self.active_account()))
    }

    /// `gh auth switch --user <login>`: records the new active account, or
    /// reports the scripted failure without changing anything.
    pub(crate) fn switch(&self, login: &str) -> Result<(), String> {
        if self.switch_fails {
            return Err("stub gh: switch refused".to_string());
        }
        *self.active.lock().expect("stub active lock") = login.to_string();
        Ok(())
    }
}

/// The stub the current process answers scoped `gh` calls from, if any.
static INSTALLED: RwLock<Option<Arc<GhScopedStub>>> = RwLock::new(None);

/// The installed stub, read once per scoped `gh` invocation.
pub(crate) fn installed() -> Option<Arc<GhScopedStub>> {
    INSTALLED.read().expect("gh stub lock").clone()
}

/// Clears the installed stub when dropped.
///
/// Test: every caller of [`GhScopedStub::install`].
pub struct GhStubGuard;

impl Drop for GhStubGuard {
    fn drop(&mut self) {
        *INSTALLED.write().expect("gh stub lock") = None;
    }
}
