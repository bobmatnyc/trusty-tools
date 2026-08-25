//! Creating an engagement without a terminal (#6159).
//!
//! Why: `crate::cli::bootstrap::cold_start` was the only thing in the crate
//! that ever wrote an `engagement.toml`, and it asks for the OpenRouter key on
//! `/dev/tty`. Every capability after it needs that file — `add repo` refuses
//! with [`AuditError::NoEngagementConfig`] without one — so the README's
//! documented sequence could not be completed by a scripted or CI caller at
//! all, and the observed workaround was allocating a pty with
//! `script -q /dev/null`. This is the same write with the prompt removed.
//!
//! A separate module rather than another method on `Session` because
//! `session.rs` reached the 500-SLOC production cap; the split is along the one
//! line that was available, which is this capability.
//!
//! What: [`InitReport`] and [`init`]. The write itself is
//! [`bootstrap::create_engagement`], shared with the interactive path — the
//! crate keeps exactly ONE writer of a plaintext key into a config.
//!
//! Test: `super::session_tests::{init_writes_an_engagement_without_a_terminal,
//! init_over_an_existing_engagement_changes_nothing,
//! init_without_a_key_in_the_environment_refuses}`.

use std::path::{Path, PathBuf};

use crate::cli::bootstrap::{self, PinResolver};
use crate::config::{EngagementConfig, SecretKey};
use crate::error::AuditError;
use crate::run::ENV_INFERENCE_CREDENTIAL;
use crate::workdir::WorkDir;

/// What [`init`] found or wrote.
///
/// Why: `created` is the whole reason this is a struct rather than a path. The
/// capability is idempotent, so a caller can put `trusty-audit init` at the top
/// of a script and run it repeatedly — and the two cases have to be
/// distinguishable, because one of them means the key in the environment did
/// NOT become the key the engagement runs on.
/// What: the config's path, whether this call wrote it, and the tool versions
/// the engagement is pinned to. The pins are empty when nothing was written,
/// since this call did not choose them.
/// Test: see the module docs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct InitReport {
    /// Where the engagement config is.
    pub path: PathBuf,
    /// True when this call wrote it; false when one was already there.
    pub created: bool,
    /// Crate name and version per pinned tool: tga, trusty-search,
    /// trusty-analyze, trusty-review, in that order.
    pub pins: Vec<(String, String)>,
}

/// Write the first `engagement.toml` at `config_path`.
///
/// # Postconditions
/// On `Ok` with `created`, an `engagement.toml` exists at `config_path`,
/// owner-readable only, carrying `credential` and an exact version per tool. On
/// `Ok` without `created`, nothing was written and the file that was already
/// there is untouched — including its key, which is why the report
/// distinguishes the two. On `Err`, nothing was written.
///
/// What: `credential` is whatever the front end resolved, which for
/// `CredentialNeed::Environment` is `OPENROUTER_API_KEY` and nothing else.
/// Tools are NOT installed here: `init` is the file, `install` is the download,
/// and a CI caller wants to decide when it pays for the second.
/// Test: see the module docs.
///
/// # Errors
///
/// [`AuditError::NoCredentialSource`] when nothing exported a key — there is no
/// config to fall back on and no terminal to ask on, so this is the end of the
/// line rather than a prompt. [`AuditError::PinsUnresolved`] when the release
/// list could not be read, and [`AuditError::EngagementNotCreated`] when the
/// file could not be written.
pub(super) async fn init(
    config_path: &Path,
    work: &WorkDir,
    credential: Option<&SecretKey>,
    pins: &PinResolver,
) -> Result<InitReport, AuditError> {
    // Idempotent: `trusty-audit init` at the top of a script runs on every
    // invocation, so a second call must be a no-op rather than a rewrite that
    // replaces the key the engagement has been running on.
    if EngagementConfig::load_if_present(config_path)?.is_some() {
        return Ok(InitReport {
            path: config_path.to_path_buf(),
            created: false,
            pins: Vec::new(),
        });
    }

    let key = credential.ok_or_else(|| AuditError::NoCredentialSource {
        env: ENV_INFERENCE_CREDENTIAL,
        config: config_path.to_path_buf(),
    })?;

    // The working directory first, so the config lands beside a layout that
    // exists — `workdir` is otherwise a step a scripted caller has to remember
    // before this one.
    work.create()?;
    let pins = pins.resolve().await?;
    bootstrap::create_engagement(config_path, key, &pins)?;

    Ok(InitReport {
        path: config_path.to_path_buf(),
        created: true,
        pins: vec![
            ("tga".to_owned(), pins.tga.version().to_owned()),
            (
                "trusty-search".to_owned(),
                pins.trusty_search.version().to_owned(),
            ),
            (
                "trusty-analyze".to_owned(),
                pins.trusty_analyze.version().to_owned(),
            ),
            (
                "trusty-review".to_owned(),
                pins.trusty_review.version().to_owned(),
            ),
        ],
    })
}
