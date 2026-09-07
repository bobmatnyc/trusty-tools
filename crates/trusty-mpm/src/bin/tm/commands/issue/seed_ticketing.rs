//! Seeding the `agents.ticketing` block into the runtime config (#7067).
//!
//! Why: `tm issue seed-config` writes the lifecycle half of the ticketing
//! standard (`issue-state.yaml`). The other half — the milestone and project
//! rules #7067 added — lives in `~/.trusty-tools/trusty-mpm/config.yaml`, a
//! DIFFERENT file. Printing a block for the operator to paste left the seeded
//! standard half-applied; this module writes it.
//!
//! What: [`ticketing_config_path`] resolves the same path
//! [`trusty_mpm::core::trusty_tools_config::TrustyToolsConfig::load`] reads,
//! by calling the loader's own `trusty_common::crate_config` resolver rather
//! than rebuilding the path. [`seed_ticketing_block`] then creates that file
//! with [`TICKETING_BLOCK_TEMPLATE`], appends the template textually, or
//! leaves the file alone — reporting which as a [`TicketingSeedOutcome`]. The
//! append is textual on purpose: parsing and re-serialising would reorder keys
//! and strip every comment the operator wrote.
//!
//! Test: `seed_creates_the_config_file_with_the_ticketing_block`,
//! `seed_appends_and_preserves_every_existing_byte`,
//! `seed_leaves_an_existing_ticketing_block_untouched`,
//! `seed_refuses_to_append_under_an_existing_agents_block`,
//! `a_seeded_file_resolves_to_the_builtin_ticketing_defaults`,
//! `the_seeded_path_is_the_one_the_loader_reads`,
//! `seed_writes_atomically_and_never_in_place`,
//! `only_the_refusal_outcome_exits_nonzero`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::trusty_tools_config::{CRATE_NAME, TICKETING_BLOCK_TEMPLATE};

/// What [`seed_ticketing_block`] did to the config file.
///
/// Why: the caller prints what happened, and "already present" must be
/// distinguishable from "written" — an operator who has tuned the standard
/// needs to be told their values were left alone, not that a seed succeeded.
/// What: the three write outcomes plus [`Self::AgentsBlockPresent`], the
/// refusal case (see [`seed_ticketing_block`] for why it cannot append).
/// Test: asserted by every test in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TicketingSeedOutcome {
    /// The file did not exist; it was created holding only the block.
    Created,
    /// The file existed with no `agents:` key; the block was appended to it.
    Appended,
    /// The file already declares `agents.ticketing`; nothing was written.
    AlreadyPresent,
    /// The file declares `agents:` WITHOUT `ticketing:`; nothing was written.
    AgentsBlockPresent,
}

/// Resolve the config file `TrustyToolsConfig::load` actually reads.
///
/// Why: the block is only useful in the file the runtime loads. Deriving the
/// path here a second time would let the two drift; delegating to the
/// loader's own resolver cannot.
/// What: `trusty_common::crate_config::crate_config_path(CRATE_NAME)` —
/// `~/.trusty-tools/trusty-mpm/config.yaml`. `None` only when the home
/// directory cannot be resolved.
/// Test: `the_seeded_path_is_the_one_the_loader_reads`.
pub(crate) fn ticketing_config_path() -> Option<PathBuf> {
    trusty_common::crate_config::crate_config_path(CRATE_NAME)
}

/// Write [`TICKETING_BLOCK_TEMPLATE`] into `path`, without clobbering it.
///
/// Why: seeding must be safe to re-run and must never overwrite a value an
/// operator set. Everything already on disk is preserved byte for byte.
/// What: absent file → create it holding the template. Present file with no
/// `agents:` key → append the template after a blank line, leaving every
/// prior byte intact. Present `agents.ticketing` → no write. Present
/// `agents:` without `ticketing:` → no write either: appending would put a
/// SECOND top-level `agents:` key in the file, and `serde_yaml` rejects a
/// duplicate mapping key, so `load_or_default` would discard the operator's
/// whole config back to defaults. The presence check is a read-only
/// `serde_yaml::Value` parse; a file that does not parse is an error, never a
/// blind append.
/// Test: the five write/refusal tests named in the module doc.
pub(crate) fn seed_ticketing_block(path: &Path) -> anyhow::Result<TicketingSeedOutcome> {
    let existing = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    anyhow::anyhow!("failed to create config dir {}: {e}", parent.display())
                })?;
            }
            write_config(path, TICKETING_BLOCK_TEMPLATE)?;
            return Ok(TicketingSeedOutcome::Created);
        }
        Err(e) => return Err(anyhow::anyhow!("failed to read {}: {e}", path.display())),
    };

    let doc: serde_yaml::Value = serde_yaml::from_str(&existing).map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid YAML ({e}) — fix it, then re-run `tm issue seed-config`",
            path.display()
        )
    })?;
    match doc.get("agents") {
        Some(agents) if agents.get("ticketing").is_some() => {
            return Ok(TicketingSeedOutcome::AlreadyPresent);
        }
        Some(_) => return Ok(TicketingSeedOutcome::AgentsBlockPresent),
        None => {}
    }

    let mut appended = existing;
    if !appended.is_empty() {
        if !appended.ends_with('\n') {
            appended.push('\n');
        }
        appended.push('\n');
    }
    appended.push_str(TICKETING_BLOCK_TEMPLATE);
    write_config(path, &appended)?;
    Ok(TicketingSeedOutcome::Appended)
}

/// Write `contents` to `path` atomically, naming the path on failure.
///
/// Why: this is the operator's real config. A `std::fs::write` truncates the
/// target and then fills it, so a kill or a full disk mid-write leaves a
/// half-file where a working config was — the exact loss the whole
/// never-clobber design above exists to prevent.
/// What: delegates to the workspace's one atomic config write, which lands a
/// sibling temp file and renames it over the target, so the target is never
/// opened for writing and a failed write leaves it byte-identical.
/// Test: `seed_writes_atomically_and_never_in_place`.
fn write_config(path: &Path, contents: &str) -> anyhow::Result<()> {
    // #7067: temp-and-rename, never a truncating in-place write.
    trusty_common::crate_config::save_raw_at(path, contents)
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))
}

/// The process exit disposition for a [`TicketingSeedOutcome`].
///
/// Why: three of the four outcomes end with the block on disk; the fourth
/// leaves the standard half-applied and needs the operator to paste it by
/// hand. Returning `Ok(())` for all four made those cases indistinguishable to
/// a script — a provisioning run that seeded nothing looked like one that
/// seeded everything.
/// What: `Created` / `Appended` / `AlreadyPresent` → `Ok`, so `tm issue
/// seed-config` exits 0. `AgentsBlockPresent` → `Err`, so it exits nonzero,
/// carrying the one-line reason. The printing is the caller's; this is only
/// the disposition, so the two cannot disagree about which arm exits.
/// Test: `only_the_refusal_outcome_exits_nonzero`.
pub(crate) fn seed_outcome_result(
    outcome: TicketingSeedOutcome,
    path: &Path,
) -> anyhow::Result<()> {
    match outcome {
        TicketingSeedOutcome::Created
        | TicketingSeedOutcome::Appended
        | TicketingSeedOutcome::AlreadyPresent => Ok(()),
        TicketingSeedOutcome::AgentsBlockPresent => Err(anyhow::anyhow!(
            "the agents.ticketing block was NOT written to {} — add the block above \
             under the existing `agents:` key by hand",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use trusty_mpm::core::trusty_tools_config::{
        ResolvedTicketing, TrustyToolsConfig, resolve_ticketing,
    };

    /// A config file with comments and a hand-set key — the bytes an append
    /// must leave untouched.
    const OPERATOR_CONFIG: &str = "\
# my notes, which a parse-and-reserialize would delete
auto_resume: true

workspace_root_template: ~/work
";

    fn config_at(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(".trusty-tools/trusty-mpm/config.yaml")
    }

    #[test]
    fn seed_creates_the_config_file_with_the_ticketing_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = config_at(&tmp);
        assert!(!path.exists());

        let outcome = seed_ticketing_block(&path).expect("seeds");

        assert_eq!(outcome, TicketingSeedOutcome::Created);
        let written = std::fs::read_to_string(&path).expect("reads back");
        assert_eq!(written, TICKETING_BLOCK_TEMPLATE);
    }

    #[test]
    fn seed_appends_and_preserves_every_existing_byte() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = config_at(&tmp);
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
        std::fs::write(&path, OPERATOR_CONFIG).expect("seed file");

        let outcome = seed_ticketing_block(&path).expect("seeds");

        assert_eq!(outcome, TicketingSeedOutcome::Appended);
        let written = std::fs::read_to_string(&path).expect("reads back");
        // Byte-for-byte: the original prefix survives, comments included.
        assert!(
            written.starts_with(OPERATOR_CONFIG),
            "existing bytes were rewritten:\n{written}"
        );
        assert!(written.ends_with(TICKETING_BLOCK_TEMPLATE), "{written}");
        // The result is still one valid config, with both halves readable.
        let cfg: TrustyToolsConfig = serde_yaml::from_str(&written).expect("parses");
        assert_eq!(cfg.auto_resume, Some(true));
        assert_eq!(
            resolve_ticketing(&cfg).expect("resolves"),
            ResolvedTicketing::default()
        );
    }

    #[test]
    fn seed_leaves_an_existing_ticketing_block_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = config_at(&tmp);
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
        let tuned = "agents:\n  ticketing:\n    milestone_required: false\n";
        std::fs::write(&path, tuned).expect("seed file");

        let outcome = seed_ticketing_block(&path).expect("seeds");

        assert_eq!(outcome, TicketingSeedOutcome::AlreadyPresent);
        assert_eq!(
            std::fs::read_to_string(&path).expect("reads back"),
            tuned,
            "an operator's tuned value was overwritten"
        );
    }

    #[test]
    fn seed_refuses_to_append_under_an_existing_agents_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = config_at(&tmp);
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("mkdir");
        // `agents:` present, `ticketing:` absent. Appending here would make a
        // duplicate top-level key and cost the operator the whole file.
        let other_agent = "agents:\n  someday: {}\n";
        std::fs::write(&path, other_agent).expect("seed file");

        let outcome = seed_ticketing_block(&path).expect("seeds");

        assert_eq!(outcome, TicketingSeedOutcome::AgentsBlockPresent);
        assert_eq!(
            std::fs::read_to_string(&path).expect("reads back"),
            other_agent
        );
    }

    #[test]
    fn a_seeded_file_resolves_to_the_builtin_ticketing_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = config_at(&tmp);
        seed_ticketing_block(&path).expect("seeds");

        let raw = std::fs::read_to_string(&path).expect("reads back");
        let cfg: TrustyToolsConfig = serde_yaml::from_str(&raw).expect("parses");
        assert_eq!(
            resolve_ticketing(&cfg).expect("resolves"),
            ResolvedTicketing::default(),
            "seeding must not change the standard"
        );
    }

    /// The write must never truncate the operator's config in place.
    ///
    /// Phase 1 pins the visible half: after a successful append the sibling
    /// temp file is gone, so nothing is left for a directory reader to mistake
    /// for a config. Phase 2 pins the half that matters: a read-only parent
    /// directory blocks creating that sibling while leaving the existing file
    /// writable via `write(2)`, so a truncating in-place write SUCCEEDS there
    /// and an atomic one fails — the seed must fail and leave every byte.
    /// Skipped when the process can create files in a read-only directory
    /// anyway (running as root), since the failure cannot be provoked there.
    #[cfg(unix)]
    #[test]
    fn seed_writes_atomically_and_never_in_place() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = config_at(&tmp);
        let dir = path.parent().expect("has a parent").to_path_buf();
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(&path, OPERATOR_CONFIG).expect("seed file");

        assert_eq!(
            seed_ticketing_block(&path).expect("seeds"),
            TicketingSeedOutcome::Appended
        );
        let siblings: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("reads dir")
            .map(|e| e.expect("entry").path())
            .collect();
        assert_eq!(siblings, vec![path.clone()], "a .tmp sibling survived");

        // Phase 2: the same file, now with an `agents:`-free body the seed
        // would append to again, under a directory that refuses new files.
        std::fs::write(&path, OPERATOR_CONFIG).expect("reset");
        let restore = std::fs::metadata(&dir).expect("stat").permissions();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let root_can_still_write = std::fs::write(dir.join("probe"), b"x").is_ok();
        let result = if root_can_still_write {
            std::fs::remove_file(dir.join("probe")).ok();
            None
        } else {
            Some(seed_ticketing_block(&path))
        };
        std::fs::set_permissions(&dir, restore).expect("restore");

        let Some(result) = result else {
            return; // running as root: the write cannot be made to fail here.
        };
        assert!(result.is_err(), "the write was expected to fail");
        assert_eq!(
            std::fs::read_to_string(&path).expect("reads back"),
            OPERATOR_CONFIG,
            "a failed write modified the operator's config in place"
        );
    }

    /// The refusal outcome is the only one that exits nonzero, so a script can
    /// tell "nothing was written" from "the block is on disk".
    #[test]
    fn only_the_refusal_outcome_exits_nonzero() {
        let path = Path::new("/home/bob/.trusty-tools/trusty-mpm/config.yaml");

        for ok in [
            TicketingSeedOutcome::Created,
            TicketingSeedOutcome::Appended,
            TicketingSeedOutcome::AlreadyPresent,
        ] {
            assert!(
                seed_outcome_result(ok, path).is_ok(),
                "{ok:?} must exit 0 — the block is on disk"
            );
        }

        let err = seed_outcome_result(TicketingSeedOutcome::AgentsBlockPresent, path)
            .expect_err("the refusal must exit nonzero");
        let msg = err.to_string();
        assert!(msg.contains("NOT written"), "{msg}");
        assert!(msg.contains(&path.display().to_string()), "{msg}");
    }

    /// The whole point of #7067's fix: the file we write is the file the
    /// runtime loader reads. A path derived independently would pass every
    /// other test in this module and still seed the wrong file.
    ///
    /// The home directory is INJECTED as `crate_config_path_at`'s `base`
    /// rather than repointed via `$HOME` — #5544 hard-bans a `$HOME` write in
    /// this bin target, and `env_isolation_tests.rs` fails the build on one.
    /// `crate_config_path_at` and `load_at` are the hermetic cores
    /// `crate_config_path` and `TrustyToolsConfig::load` delegate to, so this
    /// exercises the production layout and the production parse.
    #[test]
    fn the_seeded_path_is_the_one_the_loader_reads() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = trusty_common::crate_config::crate_config_path_at(tmp.path(), CRATE_NAME);
        // The layout the loader computes is the layout we seed into.
        assert_eq!(path, config_at(&tmp));
        // And the production accessor is that resolver, not a hand-built path.
        assert_eq!(
            ticketing_config_path(),
            trusty_common::crate_config::crate_config_path(CRATE_NAME)
        );

        assert_eq!(
            seed_ticketing_block(&path).expect("seeds"),
            TicketingSeedOutcome::Created
        );

        // Read it back through the loader's own parse, not a local one.
        let loaded: TrustyToolsConfig = trusty_common::crate_config::load_at(&path)
            .expect("the loader reads it")
            .expect("the file is there");
        assert!(
            loaded
                .agents
                .as_ref()
                .and_then(|a| a.ticketing.as_ref())
                .is_some(),
            "the loader did not see the seeded block"
        );
        assert_eq!(
            resolve_ticketing(&loaded).expect("resolves"),
            ResolvedTicketing::default()
        );
    }
}
