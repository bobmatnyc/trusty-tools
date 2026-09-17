//! The `tm doctor` `rust_build_env` row — this machine's Rust build settings.
//!
//! Why (#6868): every tm-provisioned agent worktree gets an empty `target/`, so
//! a dispatched engineer pays a cold full-workspace build before it runs one
//! gate — ~200 s measured on the 16-core dev host on 2026-09-16, against 103 s
//! when `CARGO_TARGET_DIR` pointed at a warm shared directory from another
//! worktree and 17 s from that same path again. Nothing an operator reads said
//! whether the shared directory existed, what job count this host resolves to,
//! or whether sccache was doing anything. This row says all three, and closes
//! with the one line a PM can paste into an engineer brief.
//!
//! What: [`check_rust_build_env`] gathers the facts and [`build_check`] folds
//! them. Gated on the project's detected stack including Rust, via the same
//! marker detection `core::stack_profile` runs — a non-Rust project reports the
//! row as not applicable, never as a warning about a tool it does not use.
//! `Ok` when the shared directory exists and is writable, `Warn` when it does
//! not exist yet (with the fix that creates it), `Fail` only when it exists and
//! cannot be written. Read-only: `tm doctor --fix` owns every write, and NOTHING
//! here or there ever edits `~/.cargo/config.toml`.
//!
//! Test: `doctor_rust_build_env_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::build_env::{BuildEnvError, ResolvedBuildEnv, paste_line, resolve_build_env};
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::disk::human_bytes;

/// Stable check name.
///
/// Why: shared with [`crate::core::build_env_repair::CHECK_NAME`] so a `--fix`
/// line and this row read the same literal.
/// Test: `rust_build_env_ok_when_target_dir_writable`.
pub(super) const CHECK_NAME: &str = "rust_build_env";

/// Wall clock one target-directory measurement may take.
///
/// Why: a warm shared target directory is tens of gigabytes across hundreds of
/// thousands of files, and `tm doctor` is not the place to walk all of it. A
/// walk this bound cuts short reports "at least" rather than a wrong total.
const SIZE_BUDGET: Duration = Duration::from_secs(2);

/// What the probe learned about the shared target directory.
///
/// Why: "missing", "writable", "not writable" and "could not tell" are four
/// different facts and collapsing any pair of them is how a diagnostic lies —
/// in particular, a directory tm could not measure must never read healthy.
/// Test: every `rust_build_env_*` test in `doctor_rust_build_env_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TargetDirState {
    /// Nothing at that path yet. `tm doctor --fix --yes` creates it.
    Missing,
    /// A directory this process can write, and what it holds.
    Writable {
        /// Sum of file lengths beneath it.
        bytes: u64,
        /// A depth or time bound cut the measurement short.
        partial: bool,
    },
    /// The path exists but cannot hold build artifacts; the string says why.
    NotWritable(String),
    /// The probe could not answer; the string says why.
    Unknown(String),
}

/// Whether `~/.cargo/config.toml` wires sccache as the rustc wrapper.
///
/// Why: report-only, always. That file is machine-global for every Rust project
/// on the host, so tm reads it and never writes it.
/// Test: `rust_build_env_warns_when_sccache_requested_but_not_wired`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WrapperState {
    /// `build.rustc-wrapper` names this command.
    Wired(String),
    /// No `build.rustc-wrapper` key, or no file at all.
    NotWired,
    /// The file exists and could not be read or parsed; the string says why.
    Unknown(String),
}

/// Everything the verdict is folded from.
///
/// Why: separating the facts from the fold is what lets every branch be proved
/// without a Rust project, a real config, or an sccache install on the test
/// host.
/// Test: `doctor_rust_build_env_tests.rs`.
#[derive(Debug, Clone)]
pub(super) enum BuildEnvFacts {
    /// Nothing to report: no project, or a stack with no Rust in it.
    NotApplicable(String),
    /// The settings could not be resolved at all. The string is the whole
    /// message, remediation included, because the two producers — an
    /// underivable repo identity and a stack scan that was cut short — need
    /// different advice.
    Undetermined(String),
    /// The settings resolved and the machine was probed.
    Probed {
        /// The resolved values this row reports and the paste line carries.
        env: ResolvedBuildEnv,
        /// What the shared target directory is.
        target_dir: TargetDirState,
        /// Where `sccache` resolved on `PATH`, when it did.
        sccache_on_path: Option<PathBuf>,
        /// What `~/.cargo/config.toml` says.
        wrapper: WrapperState,
    },
}

/// Fold the gathered facts into the row (pure).
///
/// Why: see [`BuildEnvFacts`]. Keeping this pure is also what makes the
/// fail-closed rule checkable: an unmeasurable directory and an unresolvable
/// identity both produce [`CheckStatus::Unknown`], never `Ok`.
/// What: `Ok` for a writable directory, `Warn` for a missing one (naming
/// `tm doctor --fix --yes`), `Fail` for one that exists and cannot be written,
/// `Unknown` when the probe learned nothing. An `sccache: true` config with no
/// wrapper wired raises the row to at least `Warn`, because every brief this
/// machine hands out would then carry `RUSTC_WRAPPER=sccache` against a wrapper
/// that is not there. Every message ends with the paste line.
/// Test: `rust_build_env_skips_on_a_non_rust_project`,
/// `rust_build_env_warns_when_target_dir_missing`,
/// `rust_build_env_ok_when_target_dir_writable`,
/// `rust_build_env_fails_when_target_dir_not_writable`,
/// `rust_build_env_warns_when_sccache_requested_but_not_wired`,
/// `rust_build_env_is_unknown_when_the_identity_cannot_be_derived`,
/// `rust_build_env_is_unknown_when_the_directory_cannot_be_measured`.
pub(super) fn build_check(facts: &BuildEnvFacts) -> DoctorCheck {
    let (env, target_dir, sccache_on_path, wrapper) = match facts {
        BuildEnvFacts::NotApplicable(why) => {
            return DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Ok,
                format!("{why} — shared cargo target directory not applicable (#6868)"),
            );
        }
        BuildEnvFacts::Undetermined(why) => {
            return DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Unknown,
                format!("this machine's Rust build settings are UNDETERMINED: {why} (#6868)"),
            );
        }
        BuildEnvFacts::Probed {
            env,
            target_dir,
            sccache_on_path,
            wrapper,
        } => (env, target_dir, sccache_on_path, wrapper),
    };

    let source = if env.target_dir_configured {
        "from `build.cargo_target_dir`"
    } else {
        "derived from the git remote"
    };
    let (status, dir_detail) = match target_dir {
        TargetDirState::Writable { bytes, partial } => (
            CheckStatus::Ok,
            format!(
                "exists and is writable, holding {}{}",
                human_bytes(*bytes),
                if *partial {
                    " (at least — the measurement was cut short)"
                } else {
                    ""
                }
            ),
        ),
        TargetDirState::Missing => (
            CheckStatus::Warn,
            "DOES NOT EXIST yet — every worktree still builds cold; `tm doctor --fix --yes` \
             creates it"
                .to_string(),
        ),
        TargetDirState::NotWritable(why) => (
            CheckStatus::Fail,
            format!("exists and is NOT writable: {why}"),
        ),
        TargetDirState::Unknown(why) => (
            CheckStatus::Unknown,
            format!("could not be measured: {why}"),
        ),
    };

    let (sccache_status, sccache_detail) = sccache_detail(env.sccache, sccache_on_path, wrapper);
    let status = status.worst(sccache_status);

    DoctorCheck::new(
        CHECK_NAME,
        status,
        format!(
            "cargo target dir {} ({source}) {dir_detail}; build_jobs {}; {sccache_detail}. \
             Paste into an engineer brief: {}",
            env.cargo_target_dir.display(),
            env.build_jobs,
            paste_line(env)
        ),
    )
}

/// The sccache clause and the status it contributes.
///
/// Why: split out so the four-way "requested × wired" matrix is readable, and
/// so the one clause that must always appear — that `--fix` never edits
/// `~/.cargo/config.toml`, and why — cannot be dropped from a branch.
/// What: `Warn` only when the config asked for sccache and no wrapper is wired;
/// `Unknown` when `~/.cargo/config.toml` could not be read AND sccache was
/// requested, since the row then cannot say whether the brief it prints is
/// valid. Everything else is `Ok` and reports.
/// Test: `rust_build_env_warns_when_sccache_requested_but_not_wired`,
/// `rust_build_env_reports_sccache_without_warning_when_not_requested`.
fn sccache_detail(
    requested: bool,
    on_path: &Option<PathBuf>,
    wrapper: &WrapperState,
) -> (CheckStatus, String) {
    let path_clause = match on_path {
        Some(p) => format!("sccache on PATH at {}", p.display()),
        None => "sccache NOT on PATH".to_string(),
    };
    let wired_clause = match wrapper {
        WrapperState::Wired(cmd) => format!("`build.rustc-wrapper` = `{cmd}`"),
        WrapperState::NotWired => "`build.rustc-wrapper` NOT wired".to_string(),
        WrapperState::Unknown(why) => format!("`build.rustc-wrapper` UNREADABLE: {why}"),
    };
    // Report-only, in every branch. Stated here rather than in the repair so
    // the operator reading the row learns it without running `--fix`.
    let policy = "`~/.cargo/config.toml` is read, never written — it is machine-global for \
                  every Rust project on this host";

    let status = match (requested, wrapper) {
        (false, _) | (true, WrapperState::Wired(_)) => CheckStatus::Ok,
        (true, WrapperState::Unknown(_)) => CheckStatus::Unknown,
        (true, WrapperState::NotWired) => CheckStatus::Warn,
    };
    let asked = if requested {
        "config asks for sccache"
    } else {
        "config does not ask for sccache"
    };
    let remedy = if status == CheckStatus::Warn {
        " — set `build.rustc-wrapper = \"sccache\"` there yourself, or set `build.sccache: \
         false`"
    } else {
        ""
    };
    (
        status,
        format!("{asked}, {path_clause}, {wired_clause}{remedy} ({policy})"),
    )
}

/// Probe this machine and report its Rust build environment.
///
/// Why: the one line in this module that touches the filesystem, git, and
/// `PATH`, so [`build_check`] above stays pure.
/// What: gates on `project_dir`'s detected stack containing `rust-engineer`
/// (a truncated detection is UNKNOWN, never "not Rust" — fail closed), resolves
/// the `build:` section read from `home`'s own crate config against the
/// project's `origin` remote, then probes the target directory, `PATH`, and
/// `~/.cargo/config.toml`.
/// Test: the fold's branches are covered by `doctor_rust_build_env_tests.rs`;
/// this wrapper is exercised by `tm doctor` itself and by
/// `doctor_report_includes_rust_build_env_row`.
pub(super) fn check_rust_build_env(project_dir: Option<&Path>, home: &Path) -> DoctorCheck {
    build_check(&gather(project_dir, home))
}

/// [`check_rust_build_env`]'s fact-gathering half.
///
/// Why: named separately so the wrapper stays one line and the gathering can be
/// read against the fold it feeds.
/// What: see [`check_rust_build_env`].
/// Test: `rust_build_env_skips_on_a_non_rust_project` drives this against a
/// temp project directory.
pub(super) fn gather(project_dir: Option<&Path>, home: &Path) -> BuildEnvFacts {
    let Some(project_dir) = project_dir else {
        return BuildEnvFacts::NotApplicable("no project directory supplied".to_string());
    };
    match crate::core::build_env::project_is_rust(project_dir) {
        Some(true) => {}
        Some(false) => {
            return BuildEnvFacts::NotApplicable(
                "this project's detected stack does not include Rust".to_string(),
            );
        }
        // Fail closed: a scan cut short has not shown this project is free of
        // Rust, so the row must not read as a clean skip.
        None => {
            return BuildEnvFacts::Undetermined(
                "stack detection was cut short before it could say whether this project is Rust, \
                 so the build settings were never resolved"
                    .to_string(),
            );
        }
    }

    let config = trusty_common::crate_config::load_at::<
        crate::core::trusty_tools_config::TrustyToolsConfig,
    >(&trusty_common::crate_config::crate_config_path_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    ))
    .ok()
    .flatten();
    let identity = trusty_common::github_path::derive_github_path(project_dir);
    let env = match resolve_build_env(
        config.as_ref().and_then(|c| c.build.as_ref()),
        home,
        identity.as_ref(),
        crate::core::build_env::host_cores(),
    ) {
        Ok(env) => env,
        Err(e @ BuildEnvError::NoRepoIdentity) => {
            return BuildEnvFacts::Undetermined(format!(
                "{e}. Set `build.cargo_target_dir` in \
                 `~/.trusty-tools/trusty-mpm/config.yaml`"
            ));
        }
    };

    let target_dir = probe_target_dir(&env.cargo_target_dir);
    BuildEnvFacts::Probed {
        target_dir,
        sccache_on_path: trusty_common::bin_resolve::resolve_binary("sccache"),
        wrapper: read_rustc_wrapper(&home.join(".cargo").join("config.toml")),
        env,
    }
}

/// Classify the shared target directory.
///
/// Why: writability is probed by WRITING — mode bits say only whether somebody
/// may write, which is the distinction the `stop_spool` row already learned the
/// hard way. The probe file never outlives the check.
/// What: `Missing` for an absent path; `NotWritable` for a non-directory or a
/// directory that refuses the probe file; otherwise `Writable` with the bytes a
/// budget-bounded walk found.
/// Test: `rust_build_env_warns_when_target_dir_missing`,
/// `rust_build_env_ok_when_target_dir_writable`,
/// `rust_build_env_fails_when_target_dir_not_writable` — each drives this
/// function against a real temp path alongside the fold's own branch.
fn probe_target_dir(dir: &Path) -> TargetDirState {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return TargetDirState::Missing,
        Err(e) => return TargetDirState::Unknown(e.to_string()),
        Ok(_) if !dir.is_dir() => {
            return TargetDirState::NotWritable("the path is not a directory".to_string());
        }
        Ok(_) => {}
    }
    let probe = dir.join(".tm-doctor-write-probe");
    if let Err(e) = std::fs::write(&probe, b"") {
        return TargetDirState::NotWritable(e.to_string());
    }
    let _ = std::fs::remove_file(&probe);

    let mut index = crate::disk::size_index::DirSizeIndex::new();
    match index.measure_within(dir, Some(SIZE_BUDGET)) {
        Ok(size) => TargetDirState::Writable {
            bytes: size.bytes,
            partial: size.truncated || !size.unreadable.is_empty(),
        },
        Err(e) => TargetDirState::Unknown(e.to_string()),
    }
}

/// Read `build.rustc-wrapper` out of a cargo config, without writing anything.
///
/// Why: report-only is the whole posture for this file — see the module doc.
/// What: `NotWired` for an absent file or an absent key; `Unknown` for a file
/// that exists and cannot be read or parsed, since a config tm could not read
/// may well wire a wrapper.
/// Test: `wrapper_is_not_wired_when_the_file_is_absent`,
/// `wrapper_reads_the_configured_command`,
/// `wrapper_is_unknown_for_a_malformed_file`.
fn read_rustc_wrapper(cargo_config: &Path) -> WrapperState {
    let raw = match std::fs::read_to_string(cargo_config) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return WrapperState::NotWired,
        Err(e) => return WrapperState::Unknown(e.to_string()),
    };
    let doc = match raw.parse::<toml::Value>() {
        Ok(doc) => doc,
        Err(e) => return WrapperState::Unknown(e.to_string()),
    };
    match doc
        .get("build")
        .and_then(|b| b.get("rustc-wrapper"))
        .and_then(toml::Value::as_str)
    {
        Some(cmd) => WrapperState::Wired(cmd.to_string()),
        None => WrapperState::NotWired,
    }
}

#[cfg(test)]
#[path = "doctor_rust_build_env_tests.rs"]
mod tests;
