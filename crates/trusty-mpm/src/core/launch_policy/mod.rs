//! Per-owner launch config: discovery, schema, `extends`, last-good (#8988).
//!
//! Why: one operator works for several owners (bobmatnyc, bob-duetto,
//! duettoresearch) whose sessions should load different MCP servers. Owner
//! state lives outside every repository, so a clone cannot write it.
//! What (PR 1a — no launch effect: argv, env and settings of every launch are
//! unchanged): [`resolve`] maps a cwd to its owner ([`owner::owner_for`]),
//! reads `<repos_root>/<owner>/.trusty-mpm-owner.toml` and its `extends` chain
//! ([`compose::resolve_chain`]), and answers one [`Outcome`]. A bad file falls
//! back to the last good policy on record ([`last_good`]) and never refuses
//! anything; with none on record the owner layer is skipped. A DELETED file
//! turns the layer off even when a last-good record exists — deleting it is the
//! operator's off switch. [`resolve_and_record`] also stores a good chain as
//! last-good; the launch carrier (PR 1b) is its caller. The `owner_config`
//! doctor row reads [`resolve`] and writes nothing.
//! Test: `mod_tests.rs` (every fallback arm), plus each submodule's tests and
//! `tests/owner_config_8988.rs`.

pub mod compose;
pub mod floor;
pub mod last_good;
pub mod owner;
pub mod schema;

use std::path::{Path, PathBuf};

use compose::{OwnerPolicy, ResolvedChain, owner_file_path, resolve_chain};
use last_good::{LastGood, read_last_good, record_last_good};
use schema::OwnerPolicyError;

/// What [`resolve`] reads from.
///
/// Why: tests pass both roots explicitly so none mutates process env.
#[derive(Debug, Clone)]
pub struct OwnerInputs<'a> {
    /// The session's working directory.
    pub cwd: &'a Path,
    /// The managed repos root ([`crate::core::repo_paths::repos_root`]).
    pub repos_root: &'a Path,
    /// tm's state root (`~/.trusty-tools/trusty-mpm`).
    pub state_root: &'a Path,
}

/// The owner layer's state for one cwd.
///
/// Test: `mod_tests.rs`.
#[derive(Debug)]
pub enum Outcome {
    /// The cwd belongs to no owner.
    NoOwner,
    /// The owner has no file: the layer is off. `retained` is a last-good
    /// record still on disk, reported but never used.
    NoFile {
        /// A last-good record left from an earlier file.
        retained: Option<LastGood>,
    },
    /// The file and its chain are good.
    Loaded(ResolvedChain),
    /// The file is bad; the last-good policy stands in.
    Fallback {
        /// Why the file is bad.
        error: OwnerPolicyError,
        /// The record in use.
        last_good: LastGood,
    },
    /// The file is bad and no usable last-good exists: the layer is skipped.
    Skipped {
        /// Why the file is bad.
        error: OwnerPolicyError,
        /// Why a last-good record on disk could not be used, if one exists.
        last_good_error: Option<String>,
    },
}

/// The owner layer's resolution for one cwd.
#[derive(Debug)]
pub struct OwnerResolution {
    /// The resolved owner, if any.
    pub owner: Option<String>,
    /// The owner file path, when there is an owner.
    pub file: Option<PathBuf>,
    /// What the layer does.
    pub outcome: Outcome,
}

impl OwnerResolution {
    /// The policy the layer applies, if any.
    ///
    /// Test: `a_bad_file_with_a_last_good_uses_it_with_the_floor_reapplied`.
    pub fn effective(&self) -> Option<&OwnerPolicy> {
        match &self.outcome {
            Outcome::Loaded(chain) => Some(&chain.policy),
            Outcome::Fallback { last_good, .. } => Some(&last_good.policy),
            Outcome::NoOwner | Outcome::NoFile { .. } | Outcome::Skipped { .. } => None,
        }
    }
}

/// Resolve the owner layer for `inputs.cwd`. Read-only.
///
/// Why: see the module doc.
/// What: no owner → [`Outcome::NoOwner`]; absent file → [`Outcome::NoFile`];
/// good chain → [`Outcome::Loaded`]; bad chain → [`Outcome::Fallback`] when a
/// readable last-good exists, else [`Outcome::Skipped`]. Never fails.
/// Test: every test in `mod_tests.rs`.
pub fn resolve(inputs: &OwnerInputs<'_>) -> OwnerResolution {
    let Some(owner) = owner::owner_for(inputs.cwd, inputs.repos_root) else {
        return OwnerResolution {
            owner: None,
            file: None,
            outcome: Outcome::NoOwner,
        };
    };
    let file = owner_file_path(inputs.repos_root, &owner);
    let outcome = match resolve_chain(inputs.repos_root, &owner) {
        Ok(Some(chain)) => Outcome::Loaded(chain),
        // #8988: a deleted file is "off", never last-good.
        Ok(None) => Outcome::NoFile {
            retained: read_last_good(inputs.state_root, &owner).ok().flatten(),
        },
        Err(error) => {
            tracing::error!(owner = %owner, file = %file.display(), %error, "owner launch config is INVALID");
            match read_last_good(inputs.state_root, &owner) {
                Ok(Some(last_good)) => Outcome::Fallback { error, last_good },
                Ok(None) => Outcome::Skipped {
                    error,
                    last_good_error: None,
                },
                Err(reason) => Outcome::Skipped {
                    error,
                    last_good_error: Some(reason),
                },
            }
        }
    };
    OwnerResolution {
        owner: Some(owner),
        file: Some(file),
        outcome,
    }
}

/// [`resolve`], then store a good chain as the owner's last-good record.
///
/// Why: the launch path (PR 1b) is the one place the policy is applied, so it
/// is the one place a good policy becomes the fallback.
/// What: a failed write is logged and never changes the outcome. A bad file
/// never writes, so it cannot overwrite the record it falls back to.
/// Test: `a_good_file_is_recorded_as_last_good`,
/// `a_bad_file_never_overwrites_the_last_good_record`.
pub fn resolve_and_record(inputs: &OwnerInputs<'_>) -> OwnerResolution {
    let resolution = resolve(inputs);
    if let (Some(owner), Outcome::Loaded(chain)) = (&resolution.owner, &resolution.outcome)
        && let Err(error) = record_last_good(inputs.state_root, owner, chain)
    {
        tracing::warn!(owner = %owner, %error, "could not record the last good owner policy");
    }
    resolution
}

/// tm's state root, `~/.trusty-tools/trusty-mpm`; `None` without a home dir.
pub fn state_root() -> Option<PathBuf> {
    trusty_common::crate_config::crate_config_dir(crate::core::trusty_tools_config::CRATE_NAME)
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
