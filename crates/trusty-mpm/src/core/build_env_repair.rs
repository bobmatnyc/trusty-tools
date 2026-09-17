//! `tm doctor --fix` for the `rust_build_env` row (#6868).
//!
//! Why: the row's two findings are both one-line fixes an operator should not
//! have to perform by hand — the shared target directory does not exist yet,
//! and the `build:` section has never been written, so the values the row
//! reports live nowhere the operator can edit them. This is the arm that closes
//! both.
//!
//! What: [`repair_rust_build_env`] emits at most three [`RepairStep`]s — create
//! [`ResolvedBuildEnv::cargo_target_dir`] with its parents, SEED the `build:`
//! section into `~/.trusty-tools/trusty-mpm/config.yaml` when and only when no
//! top-level `build` key is present, and a standing REFUSAL for
//! `~/.cargo/config.toml`.
//!
//! Three rules bound it, and they are why it is this small:
//!
//! 1. **Never overwrite an existing key.** The seed is skipped entirely when
//!    the document already carries `build:`, however partial that section is.
//!    The repair appends TEXT rather than re-serialising
//!    [`crate::core::trusty_tools_config::TrustyToolsConfig`],
//!    because that loader is lenient by design (#5207) and a round-trip would
//!    silently drop every key the schema does not define, plus every comment.
//! 2. **`~/.cargo/config.toml` is never edited.** That file is machine-global
//!    for every Rust project on the host, not just tm's, so writing
//!    `build.rustc-wrapper` there changes builds nobody ran through tm. The
//!    refusal is reported with its reason rather than left silent — the same
//!    convention [`crate::core::doctor_repair::refuse_legacy_sources`] uses.
//! 3. **Fail closed, and verify from disk.** A config file that does not parse
//!    is REFUSED, never appended to; and an applied write is read back before
//!    it is reported applied, so a silent no-op cannot render as a repair.
//!
//! Test: `build_env_repair_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::build_env::{BUILD_SECTION_KEY, ResolvedBuildEnv, default_build_section_yaml};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};

/// The `tm doctor` check every step here answers.
///
/// Why: one literal, shared with `daemon::doctor_rust_build_env::CHECK_NAME`,
/// so a `--fix` line and the row that reported it read the same name.
/// What: `rust_build_env`.
/// Test: `rust_build_env_repair_steps_name_the_check`.
pub const CHECK_NAME: &str = "rust_build_env";

/// Why `--fix` will not wire sccache for the operator.
///
/// Why: an operator who sees no sccache step needs to know it was a decision,
/// not an omission. Naming the string makes it assertable.
/// What: one sentence, used verbatim in the refused step.
/// Test: `rust_build_env_repair_refuses_to_wire_the_cargo_config`.
pub const CARGO_CONFIG_REFUSAL: &str = "`~/.cargo/config.toml` is machine-global for every Rust project on this host, not just \
     tm's — wiring `build.rustc-wrapper` there changes builds tm never ran, so it stays an \
     operator decision";

/// Create the shared target directory and seed the `build:` section.
///
/// Why: see the module doc. `--fix` is the only surface between launches where
/// a machine-level build setting can be established, so it is where the two
/// repairable halves of the `rust_build_env` row get fixed.
/// What: up to three steps, in this order — the directory, the config seed, and
/// the standing `~/.cargo/config.toml` refusal (emitted only when the config
/// asked for sccache, since there is nothing to refuse otherwise). A directory
/// that already exists and a document that already carries `build:` each
/// produce NO step, which is what makes a second run report nothing to repair.
/// `config_path` is threaded in rather than resolved here so every test is
/// hermetic.
/// Test: `rust_build_env_repair_creates_dir_and_writes_defaults_once`,
/// `rust_build_env_repair_is_idempotent`,
/// `rust_build_env_repair_never_overwrites_existing_build_keys`,
/// `rust_build_env_repair_refuses_an_unparseable_config`,
/// `rust_build_env_repair_refuses_to_wire_the_cargo_config`,
/// `rust_build_env_repair_reports_an_uncreatable_directory_as_failed`.
pub fn repair_rust_build_env(
    config_path: &Path,
    env: &ResolvedBuildEnv,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let mut steps = Vec::new();
    if let Some(step) = target_dir_step(&env.cargo_target_dir, mode) {
        steps.push(step);
    }
    if let Some(step) = config_seed_step(config_path, env, mode) {
        steps.push(step);
    }
    if env.sccache {
        steps.push(RepairStep {
            check: CHECK_NAME,
            path: cargo_config_path_for(config_path),
            what: "wire `build.rustc-wrapper = \"sccache\"`".to_string(),
            status: StepStatus::Refused(CARGO_CONFIG_REFUSAL.to_string()),
        });
    }
    steps
}

/// Where `~/.cargo/config.toml` lives, relative to the trusty-tools config.
///
/// Why: the refusal step must name a real path, and the repair is hermetic —
/// it never calls `dirs::home_dir()`. The trusty-tools config already sits at
/// `<home>/.trusty-tools/trusty-mpm/config.yaml`, so the home directory is
/// three levels up from it.
/// What: `<home>/.cargo/config.toml`, falling back to the bare relative path
/// when `config_path` is too shallow to contain a home directory.
/// Test: `rust_build_env_repair_refuses_to_wire_the_cargo_config`.
fn cargo_config_path_for(config_path: &Path) -> PathBuf {
    let home = config_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent);
    match home {
        Some(home) => home.join(".cargo").join("config.toml"),
        None => PathBuf::from(".cargo/config.toml"),
    }
}

/// The directory half — one step, or none when it already exists.
///
/// Why: idempotence is what lets an operator run `--fix` without first working
/// out whether it is needed, so "already there" must be NO step rather than a
/// step reporting a creation that did not happen.
/// What: `None` when `dir` is an existing directory. Otherwise a step that
/// creates it with parents; a path that exists and is NOT a directory is
/// REFUSED rather than replaced, because tm never deletes what it did not
/// write.
/// Test: `rust_build_env_repair_creates_dir_and_writes_defaults_once`,
/// `rust_build_env_repair_is_idempotent`,
/// `rust_build_env_repair_reports_an_uncreatable_directory_as_failed`.
fn target_dir_step(dir: &Path, mode: RepairMode) -> Option<RepairStep> {
    if dir.is_dir() {
        return None;
    }
    let what = "create the shared cargo target directory".to_string();
    if dir.exists() {
        return Some(RepairStep {
            check: CHECK_NAME,
            path: dir.to_path_buf(),
            what,
            status: StepStatus::Refused(
                "the path exists and is not a directory — tm does not remove what it did not \
                 write"
                    .to_string(),
            ),
        });
    }
    if mode == RepairMode::DryRun {
        return Some(RepairStep {
            check: CHECK_NAME,
            path: dir.to_path_buf(),
            what,
            status: StepStatus::Planned,
        });
    }
    let status = match std::fs::create_dir_all(dir) {
        // Verified from disk, never from the `Ok` alone.
        Ok(()) if dir.is_dir() => StepStatus::Applied { backup: None },
        Ok(()) => StepStatus::Failed(
            "create_dir_all reported success but the directory is not there".to_string(),
        ),
        Err(e) => StepStatus::Failed(e.to_string()),
    };
    Some(RepairStep {
        check: CHECK_NAME,
        path: dir.to_path_buf(),
        what,
        status,
    })
}

/// The config half — seed `build:` when, and only when, it is absent.
///
/// Why: rule 1 of the module doc. Detection is by TOP-LEVEL KEY rather than by
/// field, so a section carrying only `build_jobs` is still "present" and keeps
/// every value the operator chose, including the ones they left unset on
/// purpose.
/// What: `None` when the document already has a `build` key. A missing file is
/// created (with parents) carrying only the seeded block. An existing file is
/// APPENDED to, after a byte-identical backup, with a leading newline when the
/// file does not already end in one. A file that does not parse as a YAML
/// mapping is REFUSED — appending to a document tm cannot read risks producing
/// a second document it can read even less.
/// Test: `rust_build_env_repair_creates_dir_and_writes_defaults_once`,
/// `rust_build_env_repair_never_overwrites_existing_build_keys`,
/// `rust_build_env_repair_refuses_an_unparseable_config`,
/// `rust_build_env_repair_seeds_an_absent_config_file`.
fn config_seed_step(
    config_path: &Path,
    env: &ResolvedBuildEnv,
    mode: RepairMode,
) -> Option<RepairStep> {
    let existing = match std::fs::read_to_string(config_path) {
        Ok(raw) => Some(raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Some(RepairStep {
                check: CHECK_NAME,
                path: config_path.to_path_buf(),
                what: seed_description(),
                status: StepStatus::Refused(format!("config file is unreadable: {e}")),
            });
        }
    };

    if let Some(raw) = &existing
        && !raw.trim().is_empty()
    {
        match crate::core::config_keys::yaml_document(raw) {
            Some(doc) if doc.get(BUILD_SECTION_KEY).is_some() => return None,
            Some(_) => {}
            None => {
                return Some(RepairStep {
                    check: CHECK_NAME,
                    path: config_path.to_path_buf(),
                    what: seed_description(),
                    status: StepStatus::Refused(
                        "the config file does not parse as a YAML mapping — fix it by hand \
                         before `--fix` appends to it"
                            .to_string(),
                    ),
                });
            }
        }
    }

    if mode == RepairMode::DryRun {
        return Some(RepairStep {
            check: CHECK_NAME,
            path: config_path.to_path_buf(),
            what: seed_description(),
            status: StepStatus::Planned,
        });
    }

    Some(RepairStep {
        check: CHECK_NAME,
        path: config_path.to_path_buf(),
        what: seed_description(),
        status: apply_seed(config_path, existing.as_deref(), env),
    })
}

/// One line describing the seed, identical in both modes.
///
/// Why: [`RepairStep`]'s contract is that `what` does not depend on the mode,
/// so the preview and the apply read the same.
/// Test: `rust_build_env_repair_creates_dir_and_writes_defaults_once`.
fn seed_description() -> String {
    format!("seed the `{BUILD_SECTION_KEY}:` defaults (no `{BUILD_SECTION_KEY}` key present)")
}

/// Perform the seed and verify it back off disk.
///
/// Why: a write that returned `Ok` has not been shown to have landed — a
/// repair that reports applied without reading the file back is the failure
/// mode `doctor_repair_scope`'s module doc names.
/// What: backs up an existing file first, appends the block (with a separating
/// newline when needed), then re-reads and re-parses the file and confirms the
/// `build` key is now present.
/// Test: `rust_build_env_repair_creates_dir_and_writes_defaults_once`,
/// `rust_build_env_repair_seeds_an_absent_config_file`.
fn apply_seed(config_path: &Path, existing: Option<&str>, env: &ResolvedBuildEnv) -> StepStatus {
    let backup = match existing {
        Some(raw) => {
            let path = backup_path(config_path);
            if let Err(e) = std::fs::write(&path, raw) {
                return StepStatus::Failed(format!("could not back up the config file: {e}"));
            }
            Some(path)
        }
        None => {
            if let Some(parent) = config_path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                return StepStatus::Failed(format!("could not create the config directory: {e}"));
            }
            None
        }
    };

    let mut next = existing.unwrap_or_default().to_string();
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    if !next.is_empty() {
        next.push('\n');
    }
    next.push_str(&default_build_section_yaml(env));

    if let Err(e) = std::fs::write(config_path, &next) {
        return StepStatus::Failed(e.to_string());
    }
    match std::fs::read_to_string(config_path)
        .ok()
        .as_deref()
        .and_then(crate::core::config_keys::yaml_document)
    {
        Some(doc) if doc.get(BUILD_SECTION_KEY).is_some() => StepStatus::Applied { backup },
        _ => StepStatus::Failed(
            "the config file was written but no `build` key reads back from it".to_string(),
        ),
    }
}

/// `<config.yaml>.bak-<unix_nanos>`, beside the file it copies.
///
/// Why: the same `<file>.bak-<nanos>` convention
/// [`crate::core::agent_reset`] uses, so an operator finds backups in one shape
/// across every repair.
/// Test: `rust_build_env_repair_creates_dir_and_writes_defaults_once`.
fn backup_path(target: &Path) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = target
        .file_name()
        .map_or_else(|| "config.yaml".to_string(), |n| n.to_string_lossy().into());
    target.with_file_name(format!("{name}.bak-{ts}"))
}

#[cfg(test)]
#[path = "build_env_repair_tests.rs"]
mod tests;
