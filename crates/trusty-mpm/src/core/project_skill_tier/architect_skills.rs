//! The Architect's shipped skills, deployed into its project tier at launch (#9540).
//!
//! Why: `tm fleet init` seeds `tm-fleet-check` and `tm-context-refresh` into
//! the Architect project once. Nothing wrote them again, so a deleted copy
//! stayed gone and a framework update never reached an untouched copy. They
//! must reach the Architect only (rulings df-D21, df-D22): the managed skill
//! tier is shared by every PM and outranks the project tier, so the one valid
//! destination is the Architect's own `<project>/.claude/skills`.
//! What: [`SKILLS`] is the one table of shipped bytes (`tm fleet init` reads
//! it too). [`deploy_for_launch`] runs at every launch, resume and relaunch;
//! for a positive Architect profile it calls [`deploy_architect_skills`], and
//! for anything else it writes nothing. A copy that differs from the last
//! deployed copy is project-custom and is never overwritten. [`health`] is
//! the read-only verdict `tm fleet status` reports.
//! Test: `architect_skills_tests.rs`,
//! `session_launch::skills::architect_9540_tests`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One skill shipped to the Architect: its stem and its `SKILL.md` bytes.
#[derive(Debug, Clone, Copy)]
pub struct ArchitectSkill {
    /// Directory name under `.claude/skills/`.
    pub stem: &'static str,
    /// The shipped `SKILL.md` text.
    pub contents: &'static str,
    /// sha256 hex of every earlier shipped `SKILL.md`: a live copy equal to one
    /// of these, with no deploy record, is an untouched tm deploy.
    pub previous: &'static [&'static str],
}

/// `tm-fleet-check`, the Architect's per-pass fleet check.
pub const FLEET_CHECK: ArchitectSkill = ArchitectSkill {
    stem: "tm-fleet-check",
    contents: include_str!("../../assets/architect/skills/tm-fleet-check.md"),
    // Every earlier shipped text (git history of the asset). Edit the asset
    // and the old hash joins this list, or a `tm fleet init` seed of the old
    // text reads as project-custom; `the_shipped_history_is_pinned_*` trips.
    previous: &[
        "3de7f63492d03ef3d7b0ca613c04ef977b82f4349e9e3d379240c323f6b91db6",
        "dc87f24d8905d63671ecb1deed6a832dcbbde8469161b6873b7d982d7352cdae",
        "a485db4f6b3fc33fba3a880a0f56b903da5433ae78474c341b96e8002f0c8726",
    ],
};

/// `tm-context-refresh`, the Architect's context refresh.
pub const CONTEXT_REFRESH: ArchitectSkill = ArchitectSkill {
    stem: "tm-context-refresh",
    contents: include_str!("../../assets/architect/skills/tm-context-refresh.md"),
    previous: &[
        "e44d87a0715798a89ee20ab450ac4ff2b320d274c3ba3569c6894ec0277d883c",
        "922ed43c92abc1288a1053b1180e9cc6e0b1a1b259b5d51a163d6a94b7663e8f",
    ],
};

/// Every skill the Architect receives. #9132 moves the bytes to `content/`.
pub const SKILLS: &[ArchitectSkill] = &[FLEET_CHECK, CONTEXT_REFRESH];

/// The deploy record, beside the skills in the project tier.
///
/// Holds, per stem, the sha256 of the bytes tm last wrote or adopted, which
/// is what "unchanged since the last deploy" is measured against.
pub const RECORD_FILE: &str = ".trusty-mpm-architect-skills.json";

/// The Architect project tier: `<project_dir>/.claude/skills`.
pub fn project_tier(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("skills")
}

/// Where `stem`'s `SKILL.md` lives under `skills_dir`.
pub fn skill_path(skills_dir: &Path, stem: &str) -> PathBuf {
    skills_dir.join(stem).join("SKILL.md")
}

/// One skill's state in the project tier, against the shipped bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillState {
    /// The live copy is byte-equal to the shipped one.
    Current,
    /// No copy exists.
    Missing,
    /// The live copy is the one tm last deployed, and the shipped bytes moved.
    Stale,
    /// The live copy differs from both: project-custom, never overwritten.
    Custom,
    /// The copy could not be read.
    Unreadable(String),
}

/// What one [`deploy_architect_skills`] run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeployReport {
    /// Stems whose `SKILL.md` was written.
    pub written: Vec<String>,
    /// Stems left alone because the live copy is project-custom.
    pub kept_custom: Vec<String>,
    /// One line per failure; the launch continues.
    pub errors: Vec<String>,
}

/// The on-disk deploy record.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    /// Stem -> sha256 hex of the bytes last deployed or adopted.
    deployed: BTreeMap<String, String>,
}

/// Lowercase sha256 hex of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Read the record; an absent one is empty.
///
/// An unreadable or malformed record is also empty, plus the error line it
/// returns: with no record a differing copy counts as project-custom, so
/// nothing is lost, and the caller must not trust the shipped history either.
fn load_record(skills_dir: &Path) -> (Record, Option<String>) {
    let path = skills_dir.join(RECORD_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return (Record::default(), None);
        }
        Err(err) => {
            let why = format!("cannot read {}: {err}", path.display());
            return (Record::default(), Some(why));
        }
    };
    match serde_json::from_str(&raw) {
        Ok(record) => (record, None),
        Err(err) => {
            let why = format!("malformed {}: {err}", path.display());
            (Record::default(), Some(why))
        }
    }
}

/// Classify the copy at `path` against `skill` and the recorded hash.
///
/// With no record entry, a copy equal to an earlier shipped text is an
/// untouched deploy ([`ArchitectSkill::previous`]) unless `trust_history` is
/// false, which is the case when the record itself could not be read.
fn classify(
    path: &Path,
    skill: &ArchitectSkill,
    recorded: Option<&String>,
    trust_history: bool,
) -> SkillState {
    match std::fs::read(path) {
        Ok(live) if live == skill.contents.as_bytes() => SkillState::Current,
        Ok(live) => {
            let hash = sha256_hex(&live);
            let untouched = match recorded {
                Some(recorded) => *recorded == hash,
                None => trust_history && skill.previous.contains(&hash.as_str()),
            };
            if untouched {
                SkillState::Stale
            } else {
                SkillState::Custom
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => SkillState::Missing,
        Err(err) => SkillState::Unreadable(format!("{}: {err}", path.display())),
    }
}

/// `path` with its longest existing prefix canonicalised.
///
/// The tier directory may not exist yet, so a plain `canonicalize` would fail
/// on exactly the first deploy.
fn resolve_lenient(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(cur) {
            return tail.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_owned());
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// An error line when `tier` is, or sits under, one of `reserved`.
///
/// Why: the Architect's skills never go to the managed tier or `~/.claude/skills`
/// (df-D21, df-D22); a symlinked `.claude`, or `$HOME` itself in the allow-list,
/// would send the write there.
/// What: compares real paths, so a symlink anywhere in `tier` is followed.
/// Test: `a_tier_resolving_onto_a_reserved_dir_is_refused`.
fn reserved_conflict(tier: &Path, reserved: &[PathBuf]) -> Option<String> {
    let real = resolve_lenient(tier);
    reserved.iter().find_map(|dir| {
        let real_reserved = resolve_lenient(dir);
        real.starts_with(&real_reserved).then(|| {
            format!(
                "{} resolves onto {}, a tier shared skills deploy to; the Architect's                  skills are never written there",
                tier.display(),
                real_reserved.display()
            )
        })
    })
}

/// The tiers an Architect skill must never reach, from the framework layout.
fn reserved_tiers_of(fw: &crate::core::paths::FrameworkPaths) -> Vec<PathBuf> {
    vec![fw.skill_deploy_dir(), fw.claude_skills_dir()]
}

/// Deploy [`SKILLS`] into `project_dir`'s project tier, unless that tier
/// resolves onto one of `reserved`.
///
/// Why/What: see [`deploy_skills_into`]; this is it with the shipped table and
/// the reserved-tier refusal, which becomes the report's one error line.
/// Test: `an_absent_skill_is_written_byte_equal`,
/// `a_tier_resolving_onto_a_reserved_dir_is_refused`.
pub fn deploy_architect_skills(project_dir: &Path, reserved: &[PathBuf]) -> DeployReport {
    let tier = project_tier(project_dir);
    if let Some(why) = reserved_conflict(&tier, reserved) {
        return DeployReport {
            errors: vec![why],
            ..DeployReport::default()
        };
    }
    deploy_skills_into(&tier, SKILLS)
}

/// Write each of `skills` into `skills_dir` where it is absent or unchanged
/// since the last deploy; never overwrite a project-custom copy.
///
/// Why: df-D22 — an edited copy is the project's and wins; a framework update
/// must still refresh a copy nobody touched.
/// What: per skill, [`SkillState::Missing`] and [`SkillState::Stale`] are
/// written atomically and recorded; [`SkillState::Current`] is adopted into the
/// record with no file write; [`SkillState::Custom`] is skipped with one warn
/// line; [`SkillState::Unreadable`] and any write failure become an error line.
/// The record is rewritten only when it changed, so a second run with nothing
/// changed writes no file. With no record entry, a copy equal to an earlier
/// shipped text is [`SkillState::Stale`]; a record that cannot be read or parsed
/// is an error line and turns that rule off.
/// Test: `an_absent_skill_is_written_byte_equal`,
/// `a_prior_shipped_seed_with_no_record_is_refreshed_and_recorded`,
/// `other_bytes_with_no_record_stay_custom_even_with_a_history`,
/// `a_malformed_record_keeps_a_differing_copy_and_names_the_record`,
/// `a_record_write_failure_is_an_error_and_the_row_does_not_say_current`,
/// `an_edited_copy_is_kept_and_reported`,
/// `an_untouched_copy_is_refreshed_when_the_shipped_bytes_move`,
/// `a_seeded_copy_with_no_record_and_other_bytes_is_custom`,
/// `a_byte_equal_seed_is_adopted_then_refreshed`,
/// `a_second_run_writes_nothing`, `a_blocked_skill_dir_is_an_error_line`.
pub fn deploy_skills_into(skills_dir: &Path, skills: &[ArchitectSkill]) -> DeployReport {
    let mut report = DeployReport::default();
    let (mut record, record_error) = load_record(skills_dir);
    let trust_history = record_error.is_none();
    report.errors.extend(record_error);
    let mut record_changed = false;
    for skill in skills {
        let path = skill_path(skills_dir, skill.stem);
        let shipped = skill.contents.as_bytes();
        let shipped_hash = sha256_hex(shipped);
        match classify(&path, skill, record.deployed.get(skill.stem), trust_history) {
            SkillState::Current => {
                if record.deployed.get(skill.stem) != Some(&shipped_hash) {
                    record.deployed.insert(skill.stem.to_owned(), shipped_hash);
                    record_changed = true;
                }
            }
            SkillState::Missing | SkillState::Stale => {
                match trusty_common::atomic_file::write_atomic(&path, shipped) {
                    Ok(()) => {
                        record.deployed.insert(skill.stem.to_owned(), shipped_hash);
                        record_changed = true;
                        report.written.push(skill.stem.to_owned());
                    }
                    Err(err) => report
                        .errors
                        .push(format!("cannot write {}: {err}", path.display())),
                }
            }
            SkillState::Custom => {
                // #9540: df-D22 — the project's edit wins; never back up and replace.
                tracing::warn!(
                    path = %path.display(),
                    "architect skill {} differs from the last deployed copy: kept as \
                     project-custom, not refreshed",
                    skill.stem
                );
                report.kept_custom.push(skill.stem.to_owned());
            }
            SkillState::Unreadable(why) => report.errors.push(format!("cannot read {why}")),
        }
    }
    if record_changed {
        let path = skills_dir.join(RECORD_FILE);
        let body = serde_json::to_vec_pretty(&record).unwrap_or_default();
        if let Err(err) = trusty_common::atomic_file::write_atomic(&path, &body) {
            report
                .errors
                .push(format!("cannot write {}: {err}", path.display()));
        }
    }
    report
}

/// Each of `skills`' state in `skills_dir`, read-only, plus the record's load
/// error when it has one.
///
/// Test: `health_names_each_skill_that_is_not_current`,
/// `the_skills_row_names_a_malformed_record`.
pub fn inspect(
    skills_dir: &Path,
    skills: &[ArchitectSkill],
) -> (Vec<(&'static str, SkillState)>, Option<String>) {
    let (record, record_error) = load_record(skills_dir);
    let states = skills
        .iter()
        .map(|skill| {
            let path = skill_path(skills_dir, skill.stem);
            let recorded = record.deployed.get(skill.stem);
            (
                skill.stem,
                classify(&path, skill, recorded, record_error.is_none()),
            )
        })
        .collect();
    (states, record_error)
}

/// The `tm fleet status` verdict for `project_dir`: `(ok, detail)`.
///
/// Why: a deploy runs fail-open, so its failure must show somewhere a person
/// reads; df-D22 also asks that a kept custom copy be named.
/// What: `ok` only when every [`SKILLS`] entry is [`SkillState::Current`], the
/// record loads, and the tier is not a reserved one; otherwise the detail
/// names each problem. Writes nothing.
/// Test: `health_names_each_skill_that_is_not_current`,
/// `a_blocked_skill_dir_is_an_error_line`,
/// `the_skills_row_names_a_malformed_record`,
/// `the_skills_row_names_a_refused_reserved_tier`,
/// `the_skills_row_names_a_missing_or_custom_skill`.
pub fn health(project_dir: &Path) -> (bool, String) {
    let fw = crate::core::paths::FrameworkPaths::default();
    health_with(project_dir, &reserved_tiers_of(&fw))
}

/// [`health`] against an explicit `reserved` list.
pub fn health_with(project_dir: &Path, reserved: &[PathBuf]) -> (bool, String) {
    let tier = project_tier(project_dir);
    if let Some(why) = reserved_conflict(&tier, reserved) {
        return (false, format!("deploy refused: {why}"));
    }
    let (states, record_error) = inspect(&tier, SKILLS);
    let mut problems: Vec<String> = states
        .iter()
        .filter_map(|(stem, state)| {
            let why = match state {
                SkillState::Current => return None,
                SkillState::Missing => "missing".to_owned(),
                SkillState::Stale => "stale (the next Architect launch refreshes it)".to_owned(),
                SkillState::Custom => "project-custom copy kept, not refreshed".to_owned(),
                SkillState::Unreadable(why) => format!("unreadable: {why}"),
            };
            Some(format!("{stem}: {why}"))
        })
        .collect();
    problems.extend(record_error.map(|err| format!("deploy record: {err}")));
    if problems.is_empty() {
        (true, format!("{} Architect skills current", states.len()))
    } else {
        (false, problems.join("; "))
    }
}

/// Deploy the Architect's skills when `project_dir` launches as the Architect.
///
/// Why: #9540 — the launch, resume and relaunch paths each call this, so a
/// deleted copy returns and an untouched copy follows a framework update. It
/// resolves the profile itself from `fw_root`'s user config, the same inputs
/// the launch resolves, so no caller's signature changes.
/// What: returns `None` and writes nothing unless
/// [`crate::core::session_profile::resolve`] says Architect (fail closed: a
/// PM, a refused request and an unreadable config all deploy nothing).
/// Otherwise runs [`deploy_architect_skills`], logs what it wrote and each
/// error, and returns the report. It never fails the caller.
/// Test: `an_architect_launch_writes_both_skills_byte_equal`,
/// `a_pm_launch_writes_no_architect_skill_anywhere`,
/// `a_deleted_copy_returns_on_the_next_launch_and_on_sync_assets`,
/// `ensure_managed_config_dir_restores_a_deleted_architect_skill`,
/// `a_deploy_error_does_not_fail_the_launch`.
pub fn deploy_for_launch(fw_root: &Path, project_dir: &Path) -> Option<DeployReport> {
    use crate::core::session_profile;
    // Cheap pre-check: a PM project never loads the user config here.
    if !session_profile::requested(project_dir).is_architect() {
        return None;
    }
    let config = crate::core::config::MpmConfig::load(fw_root);
    if !session_profile::resolve(project_dir, &config).is_architect() {
        return None;
    }
    let fw = crate::core::paths::FrameworkPaths::from_root(fw_root);
    let report = deploy_architect_skills(project_dir, &reserved_tiers_of(&fw));
    if !report.written.is_empty() {
        tracing::info!(
            project_dir = %project_dir.display(),
            written = %report.written.join(", "),
            "architect skills deployed into the project tier"
        );
    }
    for err in &report.errors {
        tracing::error!(
            project_dir = %project_dir.display(),
            "architect skills not deployed: {err}"
        );
    }
    Some(report)
}

#[cfg(test)]
#[path = "architect_skills_tests.rs"]
mod tests;
