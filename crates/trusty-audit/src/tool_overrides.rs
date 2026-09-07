//! The one documented way to run the chain on a locally built binary (#6132).
//!
//! Why: every tool this client drives is resolved from the working directory's
//! own `tools/` area, at the version `engagement.toml` pins and the version
//! record vouches for. That is the whole point of [`crate::tools`], and it is
//! also why a merged-but-unpublished fix could not be exercised through the
//! chain at all — the #6131 verification ran published tga 3.2.0 while 3.3.0 sat
//! merged, because there was nowhere to say "use this build instead".
//!
//! `TRUSTY_REVIEW_BIN` / `TRUSTY_SEARCH_BIN` / `TRUSTY_ANALYZE_BIN` looked like
//! that override and are not: `crate::run`'s child spawn SETS them from the pinned
//! paths on every child, so an inherited value is clobbered before any child
//! reads it. They are internal plumbing between this process and its children.
//!
//! What: four `TRUSTY_AUDIT_*_BIN` variables, one per [`RequiredTool`], each
//! naming an absolute path to an executable file. Precedence is explicit
//! override, then the pinned copy — and only those two. There is no third
//! branch: a variable naming a path that cannot run REFUSES the run rather than
//! falling back to the pin, because an override that quietly did not take is the
//! #5454 version-skew class wearing the operator's own intent.
//!
//! An override also excuses its tool from install ([`crate::tools::unsatisfied`])
//! — an unpublished pin is exactly the case this exists for, and demanding a
//! download of the version being replaced would refuse the run it enables.
//!
//! ## Provenance is not optional
//!
//! A run driven by a local binary must never read as a pinned one. [`ToolOverrides::record`]
//! writes what this run resolved into `state/`[`RECORD_FILE`], and
//! [`crate::index_report::recorded_tools`] stamps [`MARKER`] into that tool's row
//! of every `index.md` Versions table — the sweep's and the return package's.
//!
//! Test: `override_tests`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AuditError;
use crate::tools::RequiredTool;
use crate::workdir::{Area, WorkDir};

/// The word an overridden tool's provenance is stamped with, everywhere.
///
/// Why: a reader skimming a Versions table needs one token that cannot be
/// mistaken for a version, and a test needs one string to assert on rather than
/// a sentence that may be reworded.
pub const MARKER: &str = "OVERRIDDEN";

/// File under `state/` recording the overrides a run resolved.
pub const RECORD_FILE: &str = "tool-overrides.toml";

/// One tool an operator pointed somewhere other than the pinned copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolOverride {
    /// The crate whose pinned binary was replaced.
    pub crate_name: String,
    /// The environment variable that replaced it.
    pub variable: String,
    /// The binary that ran instead.
    pub binary: PathBuf,
}

/// The `state/tool-overrides.toml` document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OverrideRecord {
    #[serde(default)]
    overrides: Vec<ToolOverride>,
}

/// Every override in force, each proven runnable before it was accepted.
///
/// Why: resolved ONCE per invocation and handed down, the same shape the
/// inference selection and the investigation budget have (`crate::run`). A
/// second read could disagree with the first, and the thing that would disagree
/// is which binary produced the report.
/// What: zero to four entries, in [`RequiredTool::ALL`] order. Empty is the
/// ordinary case and means every tool comes from its pin.
/// Test: `override_tests::an_unset_environment_overrides_nothing`,
/// `override_tests::an_executable_override_is_resolved`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOverrides {
    entries: Vec<ToolOverride>,
}

impl ToolOverrides {
    /// Resolve every override the given environment declares, or refuse.
    ///
    /// Why: takes the lookup rather than reading the process environment, so
    /// every rule below is provable without `std::env::set_var` — `unsafe` in
    /// edition 2024 and racy in a parallel test binary. Same shape as
    /// `crate::run::sweep_with_env`.
    ///
    /// # Postconditions
    /// On `Ok`, every entry's `binary` is an absolute path to an executable
    /// file that existed when this ran. On `Err`, no override is in force and
    /// the caller must not run: there is no partial acceptance, because a set
    /// where one variable took and another did not is the state an operator
    /// cannot reason about.
    ///
    /// What: one pass over [`RequiredTool::ALL`], reading each tool's
    /// [`RequiredTool::override_env`]. An unset or empty value is not an
    /// override; anything else must pass [`validated`].
    /// Test: `override_tests::an_unset_environment_overrides_nothing`,
    /// `override_tests::an_executable_override_is_resolved`,
    /// `override_tests::an_override_naming_a_missing_path_is_refused`.
    ///
    /// # Errors
    ///
    /// [`AuditError::ToolOverride`] naming the first variable whose path cannot
    /// be run.
    pub fn resolve<F>(lookup: F) -> Result<Self, AuditError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let mut entries = Vec::new();
        for tool in RequiredTool::ALL {
            let variable = tool.override_env();
            // Empty is unset: an exported-but-blank variable is far more often a
            // shell accident than a deliberate path, and the rule matches
            // `grounding::daemons::socket_from_override`.
            let Some(value) = lookup(variable).filter(|v| !v.is_empty()) else {
                continue;
            };
            entries.push(ToolOverride {
                crate_name: tool.crate_name().to_owned(),
                variable: variable.to_owned(),
                binary: validated(variable, PathBuf::from(value))?,
            });
        }
        Ok(Self { entries })
    }

    /// [`Self::resolve`], against this process's own environment.
    ///
    /// # Errors
    ///
    /// Exactly [`Self::resolve`]'s.
    pub fn from_environment() -> Result<Self, AuditError> {
        Self::resolve(|name| std::env::var(name).ok())
    }

    /// The binary this run uses for `tool`, or `None` when its pin stands.
    #[must_use]
    pub fn path_of(&self, tool: RequiredTool) -> Option<&Path> {
        self.entries
            .iter()
            .find(|e| e.crate_name == tool.crate_name())
            .map(|e| e.binary.as_path())
    }

    /// What a report says in place of a version for this crate, if it is overridden.
    ///
    /// Why: the source string is built HERE rather than at each index producer,
    /// so the sweep's table and the return package's cannot word the same fact
    /// differently — the reason [`crate::index_report::recorded_tools`] exists
    /// at all.
    /// What: `None` for a tool running from its pin. Otherwise a line leading
    /// with [`MARKER`] and naming both the variable and the path.
    /// Test: `override_tests::an_overridden_tool_states_its_provenance`.
    #[must_use]
    pub fn provenance_of(&self, crate_name: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|e| e.crate_name == crate_name)
            .map(|e| {
                format!(
                    "{MARKER} — {} named {}, a local binary this client did not install or verify",
                    e.variable,
                    e.binary.display()
                )
            })
    }

    /// Whether nothing is overridden, i.e. every tool runs from its pin.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Read what the last run recorded, or nothing when it recorded none.
    ///
    /// # Errors
    ///
    /// [`AuditError::Read`] when the file exists but cannot be read, and
    /// [`AuditError::Parse`] when it is not a valid record. A malformed record
    /// is an error rather than a shrug, for [`crate::tools::read_record`]'s
    /// reason: it would otherwise read as "nothing was overridden", which is the
    /// state a tampered-with record wants to look like.
    pub fn read(work: &WorkDir) -> Result<Self, AuditError> {
        let path = record_path(work);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(AuditError::Read { path, source }),
        };
        let record: OverrideRecord = toml::from_str(&text).map_err(|source| AuditError::Parse {
            path,
            what: "tool override record",
            source: Box::new(source),
        })?;
        Ok(Self {
            entries: record.overrides,
        })
    }

    /// Record what this run resolved, so its reports can state it.
    ///
    /// Why: the deliverable is read on a different machine, in a different
    /// process, from a shell that never exported these variables. A report that
    /// asked the environment at render time would describe THAT shell rather
    /// than the run — so the fact is written down where every index producer
    /// already reads its tool versions from.
    /// What: overwrites `state/`[`RECORD_FILE`] with the whole set, and DELETES
    /// it when nothing is overridden — a stale file would have a clean pinned
    /// run confessing to an override it did not use.
    /// Test: `override_tests::a_recorded_set_round_trips`,
    /// `override_tests::recording_nothing_clears_an_earlier_claim`.
    ///
    /// # Errors
    ///
    /// [`AuditError::WorkDir`] when the record cannot be written or removed.
    pub fn record(&self, work: &WorkDir) -> Result<(), AuditError> {
        let path = record_path(work);
        if self.entries.is_empty() {
            return match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(AuditError::WorkDir { path, source }),
            };
        }
        let record = OverrideRecord {
            overrides: self.entries.clone(),
        };
        // Infallible in practice: owned strings and paths, no map keys.
        let text = toml::to_string_pretty(&record).map_err(|e| AuditError::WorkDir {
            path: path.clone(),
            source: std::io::Error::other(e),
        })?;
        std::fs::write(&path, text).map_err(|source| AuditError::WorkDir { path, source })
    }
}

/// Where the override record lives.
#[must_use]
pub fn record_path(work: &WorkDir) -> PathBuf {
    work.path(Area::State).join(RECORD_FILE)
}

/// Prove a path can actually be run, or say exactly what is wrong with it.
///
/// Why: fail closed. Every reason below would otherwise surface hours later as
/// a child that could not be started, or — worse — as a silent fall back to the
/// pin, which is the one outcome this feature must never produce.
///
/// Absoluteness is checked because `crate::run`'s child spawn runs the
/// child with `current_dir` set to the repository checkout, so a relative path
/// would resolve against a directory the operator never typed it in.
///
/// What: absolute, present, a regular file, and — on unix — carrying an execute
/// bit. Each failure names the variable so the operator knows which of the four
/// to fix.
/// Test: `override_tests::an_override_naming_a_missing_path_is_refused`,
/// `override_tests::a_relative_override_is_refused`,
/// `override_tests::a_non_executable_override_is_refused`.
///
/// # Errors
///
/// [`AuditError::ToolOverride`] naming `variable`, the path, and the reason.
fn validated(variable: &'static str, path: PathBuf) -> Result<PathBuf, AuditError> {
    let refuse = |reason: &'static str| AuditError::ToolOverride {
        variable,
        path: path.clone(),
        reason,
    };
    if !path.is_absolute() {
        return Err(refuse("is not an absolute path"));
    }
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(refuse("does not exist")),
        Err(_) => return Err(refuse("cannot be read")),
    };
    if !meta.is_file() {
        return Err(refuse("is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(refuse("is not executable"));
        }
    }
    Ok(path)
}

#[cfg(test)]
mod override_tests {
    use super::*;

    /// An environment that names one override for `tool`, and nothing else.
    fn only(tool: RequiredTool, value: &str) -> impl Fn(&str) -> Option<String> + use<'_> {
        let wanted = tool.override_env();
        move |name: &str| (name == wanted).then(|| value.to_owned())
    }

    /// An executable stub at `path`, so `validated` has something to accept.
    fn executable(path: &Path) {
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
    }

    /// The ordinary case: nothing exported, so every tool runs from its pin.
    #[test]
    fn an_unset_environment_overrides_nothing() {
        let resolved = ToolOverrides::resolve(|_| None).expect("nothing to refuse");
        assert!(resolved.is_empty());
        assert_eq!(resolved.path_of(RequiredTool::Tga), None);
        // An exported-but-blank variable is the same as unset, not a refusal.
        let blank = ToolOverrides::resolve(|_| Some(String::new())).expect("blank is unset");
        assert!(blank.is_empty());
    }

    /// #6132: the feature itself — a locally built binary the chain will run.
    #[cfg(unix)]
    #[test]
    fn an_executable_override_is_resolved() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let local = tmp.path().join("tga");
        executable(&local);

        let resolved =
            ToolOverrides::resolve(only(RequiredTool::Tga, &local.display().to_string()))
                .expect("an executable file is accepted");
        assert_eq!(resolved.path_of(RequiredTool::Tga), Some(local.as_path()));
        // Only the tool that was named: the other three still come from the pin.
        assert_eq!(resolved.path_of(RequiredTool::TrustyReview), None);
    }

    /// 🔴 The fail-closed rule. A variable naming nothing must refuse the run,
    /// never fall back to the pinned copy — a run the operator believes is
    /// testing their build while it tests the published one is worse than no
    /// override at all.
    #[test]
    fn an_override_naming_a_missing_path_is_refused() {
        let err = ToolOverrides::resolve(only(RequiredTool::TrustyReview, "/nonexistent/tr"))
            .expect_err("a missing path cannot be run");
        let AuditError::ToolOverride {
            variable, reason, ..
        } = err
        else {
            panic!("expected ToolOverride, got {err:?}");
        };
        assert_eq!(variable, "TRUSTY_AUDIT_REVIEW_BIN");
        assert_eq!(reason, "does not exist");
    }

    /// The child runs with `current_dir` set to a checkout, so a relative path
    /// would resolve somewhere the operator never typed it.
    #[test]
    fn a_relative_override_is_refused() {
        let err = ToolOverrides::resolve(only(RequiredTool::TrustySearch, "target/debug/ts"))
            .expect_err("a relative path cannot be trusted");
        let AuditError::ToolOverride {
            variable, reason, ..
        } = err
        else {
            panic!("expected ToolOverride, got {err:?}");
        };
        assert_eq!(variable, "TRUSTY_AUDIT_SEARCH_BIN");
        assert_eq!(reason, "is not an absolute path");
    }

    /// A path that exists but cannot be executed fails HERE, not as an
    /// unexplained spawn failure per repository an hour later.
    #[cfg(unix)]
    #[test]
    fn a_non_executable_override_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plain = tmp.path().join("trusty-analyze");
        std::fs::write(&plain, b"not a program").expect("write");

        let err = ToolOverrides::resolve(only(
            RequiredTool::TrustyAnalyze,
            &plain.display().to_string(),
        ))
        .expect_err("a non-executable file cannot be run");
        let AuditError::ToolOverride { reason, .. } = err else {
            panic!("expected ToolOverride, got {err:?}");
        };
        assert_eq!(reason, "is not executable");
    }

    /// The provenance a report states, and the marker it can be found by.
    #[cfg(unix)]
    #[test]
    fn an_overridden_tool_states_its_provenance() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let local = tmp.path().join("tga");
        executable(&local);

        let resolved =
            ToolOverrides::resolve(only(RequiredTool::Tga, &local.display().to_string()))
                .expect("resolves");
        let stated = resolved.provenance_of("tga").expect("tga is overridden");
        assert!(stated.starts_with(MARKER), "{stated}");
        assert!(stated.contains("TRUSTY_AUDIT_TGA_BIN"), "{stated}");
        assert!(stated.contains(&local.display().to_string()), "{stated}");
        assert_eq!(resolved.provenance_of("trusty-review"), None);
    }

    /// The record is read on a machine that never exported the variable, so it
    /// must carry everything a report needs to state.
    #[cfg(unix)]
    #[test]
    fn a_recorded_set_round_trips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        work.create().expect("create");
        let local = tmp.path().join("tga");
        executable(&local);

        let resolved =
            ToolOverrides::resolve(only(RequiredTool::Tga, &local.display().to_string()))
                .expect("resolves");
        resolved.record(&work).expect("records");

        assert_eq!(ToolOverrides::read(&work).expect("reads back"), resolved);
    }

    /// 🔴 A clean pinned run must not inherit an earlier run's confession.
    #[cfg(unix)]
    #[test]
    fn recording_nothing_clears_an_earlier_claim() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));
        work.create().expect("create");
        let local = tmp.path().join("tga");
        executable(&local);

        ToolOverrides::resolve(only(RequiredTool::Tga, &local.display().to_string()))
            .expect("resolves")
            .record(&work)
            .expect("records");
        ToolOverrides::default().record(&work).expect("clears");

        assert!(!record_path(&work).exists());
        assert!(ToolOverrides::read(&work).expect("reads").is_empty());
    }
}
