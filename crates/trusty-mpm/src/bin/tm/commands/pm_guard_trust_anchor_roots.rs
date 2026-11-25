//! The `.trusty-mpm` roots the trust-anchor floor guards (#11821 P3).
//!
//! Why: the allowlist, twin-arming and Architect-launch readers resolve
//! `.trusty-mpm` under the configured config dir (`TRUSTY_MPM_CONFIG_DIR`,
//! `install.toml` `config_dir`), so a floor that guards only `$HOME`'s root
//! leaves the files those readers trust writable.
//! What: [`anchor_roots`] names `$HOME`'s root and the configured one, or
//! refuses when the config dir does not resolve; [`RootAnchors::locate`]
//! resolves one root's config, arming directory and launch directory.
//! Test: `a_configured_config_dir_anchor_write_is_denied`,
//! `a_broken_install_file_denies_every_write`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::architect_launch::{self, ARCHITECT_DIR};
use trusty_mpm::core::twin_arming::ARMED_DIR;

use super::{ANCHOR_ROOT, ArmedAnchors, CONFIG_ANCHOR, live_armed};
use crate::commands::pm_guard_trust_anchor_paths::{FileId, Resolved, file_id, resolve};

/// The `.trusty-mpm` roots under `home`: `$HOME`'s, then the configured
/// config dir's when it differs.
///
/// What: `Err(<home>/.trusty-tools/install.toml)` when the config dir does
/// not resolve (a broken install file, a relative env value); the parent
/// turns it into a deny of every write, as for any anchor location that does
/// not resolve.
/// Test: `a_broken_install_file_denies_every_write`.
pub(super) fn anchor_roots(home: &Path) -> Result<Vec<PathBuf>, PathBuf> {
    // #11821: fail closed; a guessed root would leave the real one writable.
    let base = trusty_mpm::core::config_base::try_config_base_under(home)
        .map_err(|_| trusty_common::install_paths::install_file_path(home))?;
    let own = home.join(ANCHOR_ROOT);
    let configured = base.join(ANCHOR_ROOT);
    Ok(if configured == own {
        vec![own]
    } else {
        vec![own, configured]
    })
}

/// One root's anchors, resolved.
pub(super) struct RootAnchors {
    /// Resolved paths protected with every directory above them.
    pub(super) guarded: Vec<PathBuf>,
    /// Lexical and resolved spellings, for the parent's name list.
    pub(super) paths: Vec<PathBuf>,
    /// The config's identity, when it exists.
    pub(super) ids: Vec<FileId>,
    /// The live arming records, when any.
    pub(super) armed: Option<ArmedAnchors>,
    /// Trees every path under which is refused.
    pub(super) fence: Vec<PathBuf>,
    /// Whether a `*.json` name could be an arming record.
    pub(super) records_live: bool,
}

impl RootAnchors {
    /// Resolve the anchors under one `.trusty-mpm` `root`; `Err(path)` when
    /// the config's own location does not resolve.
    ///
    /// What: an arming directory that does not resolve fences the `twin/`
    /// tree, lexical and resolved, and counts records as live (#8878 finding
    /// 5). The Architect launch directory is always fenced (#8878 ruling A).
    /// Test: `a_blocked_arming_dir_fences_only_the_twin_tree`,
    /// `the_launch_record_dir_is_an_anchor`.
    pub(super) fn locate(root: &Path) -> Result<Self, PathBuf> {
        let lexical = root.join(CONFIG_ANCHOR);
        let Resolved::Path(config) = resolve(&lexical) else {
            return Err(lexical);
        };
        let armed_lexical = root.join(ARMED_DIR);
        let mut guarded = vec![config.clone()];
        let mut paths = vec![lexical, config.clone(), armed_lexical.clone()];
        let (armed, mut fence) = match resolve(&armed_lexical) {
            Resolved::Path(dir) => {
                guarded.push(dir.clone());
                paths.push(dir.clone());
                (live_armed(dir), Vec::new())
            }
            Resolved::Unresolvable => {
                let twin = Path::new(ARMED_DIR).iter().next().unwrap_or_default();
                let resolved_root = config.parent().map(|p| p.join(twin));
                (
                    None,
                    [Some(root.join(twin)), resolved_root]
                        .into_iter()
                        .flatten()
                        .collect(),
                )
            }
        };
        let records_live = armed.is_some() || !fence.is_empty();
        let launch = root.join(architect_launch::ARCHITECT_DIR);
        paths.push(launch.clone());
        match resolve(&launch) {
            Resolved::Path(dir) => {
                guarded.push(dir.clone());
                fence.push(dir.clone());
                paths.push(dir);
            }
            Resolved::Unresolvable => {
                fence.extend(config.parent().map(|p| p.join(ARCHITECT_DIR)));
                fence.push(launch);
            }
        }
        Ok(Self {
            guarded,
            paths,
            ids: file_id(&config).into_iter().collect(),
            armed,
            fence,
            records_live,
        })
    }
}
