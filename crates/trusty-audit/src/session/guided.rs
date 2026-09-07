//! The pre-sweep flow: what the engagement still needs, and what to do next.
//!
//! Why: the epic (#5477) fixes an order — pick repositories, then install
//! tooling, then sweep — and a recipient who double-clicked the binary has to
//! be told where in it they are. Naming the next step as data lets the CLI
//! print it and the Tauri shell highlight it from one decision.
//!
//! What: [`guided`], which reads the manifest, the engagement config and the
//! registry, installs the pinned set when the flow has reached that step, and
//! answers a [`GuidedStatus`]. It decides nothing the CLI could not; it only
//! decides it once, in the library.
//!
//! A separate module rather than another method on [`Session`], for the reason
//! [`super::init`] gives: `session.rs` sits at the 500-SLOC production cap
//! (#5563).
//!
//! Test: `super::session_tests::guided_asks_for_repositories_before_tools`,
//! `super::session_tests::guided_asks_for_tools_once_repositories_are_known`,
//! `super::session_tests::guided_is_ready_once_repositories_and_tools_are_both_in_place`.

use crate::config::EngagementConfig;
use crate::error::AuditError;
use crate::manifest::AuditManifest;
use crate::registry::{self, TargetKind};
use crate::run;
use crate::tools::{self, InstalledTool, RequiredTool};

use super::{GuidedStatus, NextStep, Session};

/// Where the engagement stands, and the one step to take next.
///
/// # Postconditions
/// The returned [`NextStep`] is derived from state read in this call, and the
/// tool list reports what is on disk AFTER any install this call performed.
///
/// What: the checks run in the epic's own order, so a missing repository set
/// outranks a missing binary.
/// Test: see the module docs.
pub(super) async fn guided(session: &Session) -> Result<GuidedStatus, AuditError> {
    session.work.create()?;
    let manifest = AuditManifest::load_if_present(&session.manifest_path)?;
    // #5979: read ONCE here rather than twice below — the flow's repository
    // check and its auto-install both need it, and reading the file twice
    // lets the two disagree about an engagement edited in between.
    let config = EngagementConfig::load_if_present(&session.config_path)?;

    // #5502: the epic's pre-sweep order is repo selection, then tooling —
    // so a missing repository set outranks a missing binary.
    //
    // #5885: the REGISTRY counts, not only the manifest. `SelectRepositories`
    // tells the operator to run `add`, and `add` writes the registry — so
    // reading only the manifest left the flow repeating that instruction
    // after they had done it, with the manifest not written until a sweep
    // finishes. Either record means the operator has named an engagement.
    //
    // #5896 review: REPOSITORIES, not any target. `Registry::targets` mixes
    // repositories and boards, so `taudit add board jira:ACME` against an
    // otherwise empty registry skipped `SelectRepositories`, triggered a
    // real multi-tool download through `auto_install_tools`, and reported
    // `ReadyForRun` over an engagement with nothing to sweep. A board is not
    // a unit of the sweep — `crate::chain::split_targets` says the same.
    let repos_known = manifest
        .as_ref()
        .is_some_and(|m| !m.repositories.is_empty())
        || registry::engagement_targets(config.as_ref(), &session.work)?
            .iter()
            .any(|target| target.kind() == TargetKind::Repo);

    // #5797: install at the point the flow would otherwise have printed
    // "now go run `install`", and not one step earlier. Repository selection
    // comes first, so a working directory with nothing chosen yet reports
    // its state without this process reaching the network — the operator
    // has not committed to an engagement here.
    let installed = if session.auto_install && repos_known {
        auto_install_tools(session, config.as_ref()).await?
    } else {
        None
    };

    let tools = tools::status(&session.work)?;
    let missing: Vec<RequiredTool> = tools
        .iter()
        .filter(|s| !s.installed)
        .map(|s| s.tool)
        .collect();

    // #5499: a finished sweep that audited something is the last state the
    // guided flow can advance from — without this the flow's final word is
    // "run the sweep", and the recipient is left holding a working directory
    // with no instruction to send anything back.
    //
    // #5494: FINISHED, not merely recorded. A checkpoint left by a sweep
    // that died names audited repositories too, and pointing at the return
    // package there would send a partial engagement instead of resuming.
    let audited = run::read_progress(&session.work)?.is_some_and(|progress| {
        progress.complete && progress.repos.iter().any(|r| r.result.succeeded())
    });

    let next = if audited {
        NextStep::ReturnPackage
    } else if !repos_known {
        NextStep::SelectRepositories
    } else if !missing.is_empty() {
        NextStep::InstallTools(missing)
    } else {
        NextStep::ReadyForRun
    };

    Ok(GuidedStatus {
        root: session.work.root().to_path_buf(),
        manifest,
        tools,
        installed,
        next,
    })
}

/// Install the pinned set for the guided flow, when there is a set to pin to.
///
/// Why: #5797. The guided flow runs against a working directory that may
/// carry no engagement config — it is the flow you enter before anything is
/// set up — and that case has to keep reporting rather than fail. There are
/// no pins without a config, and installing without pins means installing
/// whatever is current, which is the #5454 defect. So an absent config
/// declines to install and the flow names the step, exactly as before.
///
/// A config that is PRESENT and unreadable or malformed is not that case and
/// propagates: the caller's `load_if_present` tolerates only absence.
/// What: takes the config [`guided`] already read, so the two cannot see
/// different files. `Ok(None)` when there is no config or the set was already
/// satisfied; `Ok(Some(installed))` naming what this call placed.
/// Test: `super::session_tests::guided_without_a_config_still_names_the_step`,
/// `super::session_tests::guided_propagates_a_malformed_config`.
async fn auto_install_tools(
    session: &Session,
    config: Option<&EngagementConfig>,
) -> Result<Option<Vec<InstalledTool>>, AuditError> {
    let Some(config) = config else {
        return Ok(None);
    };
    tools::ensure(&session.work, &config.tools, &session.progress).await
}
