//! Start the service or supervisor of one member `tctl install` just placed.
//!
//! Why (#11587): the start is one call, so `install_all` can run it inside the
//! sign gate (`InstallSigner::sign_then_start`): signed first, started only
//! after a sign that did not fail. Moved out of `install.rs` unchanged
//! otherwise, which also keeps that file under the SLOC cap.
//! What: [`start_member`] runs Phase 7 (the trusty-mpm supervisor) or Phase 7b
//! (a shared daemon's `service install`) and returns `(service_ok, detail)`.
//! Test: `macos_signing::install_step::tests::every_service_start_goes_through_the_sign_gate`;
//! the bootstrap decisions in `install_tests.rs`.

use std::path::Path;

use trusty_progress::{LiveChecklist, Narrator};

use super::install_report::{
    plans_mpm_supervisor_bootstrap, plans_service_bootstrap, InstallOutcome,
};
use crate::commands::service_bootstrap::bootstrap_member_service;
use crate::commands::stable_set::StableMember;

/// Where `start_member` reports, and the run's service flags.
pub(super) struct StartCtx<'a> {
    /// The live checklist is on screen (interactive stderr).
    pub live: bool,
    /// The plain narrator, used when `live` is false.
    pub narr: &'a Narrator,
    /// The live checklist, used when `live` is true.
    pub checklist: &'a LiveChecklist,
    /// `--no-service` / `TCTL_NO_SERVICE_BOOTSTRAP` did not opt out.
    pub service_enabled: bool,
    /// `--force` (the supervisor downgrade guard).
    pub force: bool,
}

impl StartCtx<'_> {
    /// One note, in the error register when `failure`, else the info one.
    pub(super) fn say(&self, failure: bool, msg: &str) {
        let prefix = if failure { "error" } else { "info" };
        if self.live {
            self.checklist.note(&format!("{prefix}: {msg}"));
        } else if failure {
            let _ = self.narr.error(msg);
        } else {
            let _ = self.narr.info(msg);
        }
    }
}

/// Whether [`start_member`] would start anything for `m`; a member with no
/// service is not reported as "service not started" when its sign fails.
pub(super) fn plans_start(m: &StableMember, service_enabled: bool) -> bool {
    (m.crate_name == "trusty-mpm" && plans_mpm_supervisor_bootstrap(m, service_enabled))
        || plans_service_bootstrap(m, service_enabled)
}

/// Bootstrap `m`'s supervisor or service from the binary at `installed`.
///
/// What: trusty-mpm gets its supervisor plist (#3527: behind the same
/// `--no-service` decision as every daemon; #3554: `installed` is the version
/// source; #8415: a failed plist write or reload fails the install). A shared
/// daemon gets `<binary> service install` from the CONCRETE path (#2556,
/// #4964 Phase 0.2), classified by `BootstrapAction::is_failure` (#4470).
/// Anything else is `service_not_attempted`.
pub(super) fn start_member(m: &StableMember, installed: &Path, ctx: &StartCtx) -> (bool, String) {
    if m.crate_name == "trusty-mpm" {
        if plans_mpm_supervisor_bootstrap(m, ctx.service_enabled) {
            let verdict = crate::commands::plist_bootstrap::supervisor_bootstrap_verdict(
                ctx.force, installed,
            );
            ctx.say(verdict.is_failure(), &verdict.note());
            return verdict.service_outcome();
        }
        ctx.say(
            false,
            "trusty-mpm supervisor bootstrap skipped (--no-service / TCTL_NO_SERVICE_BOOTSTRAP)",
        );
    }
    if plans_service_bootstrap(m, ctx.service_enabled) {
        let action = bootstrap_member_service(&m.binary, Some(installed));
        let note = action.note(&m.binary);
        let is_failure = action.is_failure();
        ctx.say(is_failure, &note);
        return (!is_failure, note);
    }
    InstallOutcome::service_not_attempted()
}
