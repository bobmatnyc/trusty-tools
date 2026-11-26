//! The #11899 skill rules, independent of git and the runner.
//!
//! Why: every rule of DOC-93 §2-§8 lives in [`load_skills`], so the tests
//! exercise each rule against a scripted [`BaseTree`].
//! What: see [`load_skills`].
//! Test: `every_skip_reason_is_recorded_and_named_in_the_body`,
//! `over_cap_skill_is_skipped_and_named`, `apply_to_gates_the_skill_on_changed_paths`.

use globset::{GlobBuilder, GlobSetBuilder};
use trusty_common::integrity::Sha256Digest;

use super::git_store::{BaseTree, BlobRead};
use super::{
    MAX_SKILL_ENTRIES, SKILL_CAP_BYTES, SkillRequest, TOTAL_CAP_BYTES, render, skill_format,
};
use crate::config::review_skills::SkillEntry;
use crate::identifier::is_valid_identifier;
use crate::models::{LoadedSkill, RepoSkillsRecord, SkipKind, SkippedSkill};

/// Where the skills can be read from for this review.
pub(crate) enum SkillOrigin<'a> {
    /// The base commit is known and present in `tree`.
    Base {
        base_sha: String,
        tree: &'a dyn BaseTree,
    },
    /// The PR head is a fork: read nothing (DOC-93 RS-21).
    Fork { base_sha: String },
    /// No readable base commit; carries the reason.
    NoBase(String),
}

/// Apply the #11899 rules to `entries`; returns the prompt block (empty when
/// nothing loaded) and the record.
///
/// Why: DOC-93 RS-4/RS-27 — every skill problem is a named skip, never a
/// review failure.
/// What: per entry, in order — an entry the config parse already skipped;
/// bare-identifier name (RS-5); first-wins duplicate (RS-9); `apply_to`
/// against `changed` (RS-7, RS-8); then, only with a readable base, the blob
/// at `.claude/skills/<name>/SKILL.md` (RS-17) capped at [`SKILL_CAP_BYTES`]
/// (RS-23), a regular file (RS-18), UTF-8 (RS-14), free of the block
/// delimiter, the Agent Skills checks (RS-11 to RS-15), and the running
/// [`TOTAL_CAP_BYTES`] (RS-24). Every entry lands in `loaded` or `skipped`.
/// Test: `every_skip_reason_is_recorded_and_named_in_the_body`,
/// `over_cap_skill_is_skipped_and_named`, `fork_loads_no_skill_and_says_so`.
pub(crate) fn load_skills(
    entries: &[SkillEntry],
    origin: &SkillOrigin<'_>,
    changed: &[String],
) -> (String, RepoSkillsRecord) {
    let mut record = RepoSkillsRecord::default();
    let mut bodies: Vec<(String, String, String)> = Vec::new();
    let mut total = 0usize;
    let mut seen: Vec<String> = Vec::new();
    // #11899: at most MAX_SKILL_ENTRIES entries are processed.
    for entry in entries.iter().take(MAX_SKILL_ENTRIES) {
        let req = match entry {
            Ok(req) => req,
            Err(skipped) => {
                record.skipped.push(skipped.clone());
                continue;
            }
        };
        match load_one(req, origin, changed, &mut seen, &mut total) {
            Ok((sha256, bytes, body)) => {
                record.loaded.push(LoadedSkill {
                    name: req.name.clone(),
                    sha256: sha256.clone(),
                    bytes,
                });
                bodies.push((req.name.clone(), sha256, body));
            }
            Err((kind, reason)) => record.skipped.push(SkippedSkill {
                name: req.name.clone(),
                kind,
                reason,
            }),
        }
    }
    // #11899: one summary entry names the entries past the cap.
    if entries.len() > MAX_SKILL_ENTRIES {
        record.skipped.push(SkippedSkill {
            name: format!("entries {}-{}", MAX_SKILL_ENTRIES + 1, entries.len()),
            kind: SkipKind::OverEntryCap,
            reason: format!("only the first {MAX_SKILL_ENTRIES} entries are processed"),
        });
    }
    match origin {
        SkillOrigin::Base { base_sha, .. } => record.base_sha = base_sha.clone(),
        SkillOrigin::Fork { base_sha } => {
            record.base_sha = base_sha.clone();
            record.fork = true;
        }
        SkillOrigin::NoBase(_) => {}
    }
    let prompt: Vec<render::PromptSkill<'_>> = bodies
        .iter()
        .map(|(name, sha256, body)| render::PromptSkill { name, sha256, body })
        .collect();
    (render::prompt_block(&record.base_sha, &prompt), record)
}

type Skip = (SkipKind, String);

/// One request: `(sha256, bytes, body)` when it loads, else the skip.
fn load_one(
    req: &SkillRequest,
    origin: &SkillOrigin<'_>,
    changed: &[String],
    seen: &mut Vec<String>,
    total: &mut usize,
) -> Result<(String, usize, String), Skip> {
    let name = req.name.as_str();
    // #11899: the same bare-identifier gate as review templates (#2995).
    if !is_valid_identifier(name) {
        return Err((
            SkipKind::InvalidName,
            "name is not a bare identifier".into(),
        ));
    }
    if seen.iter().any(|s| s == name) {
        return Err((
            SkipKind::Duplicate,
            "the name appeared in an earlier entry".into(),
        ));
    }
    seen.push(name.to_string());
    match applies(req.apply_to.as_deref(), changed) {
        Ok(true) => {}
        Ok(false) => {
            let why = "no changed file matches apply_to".into();
            return Err((SkipKind::NotApplicable, why));
        }
        Err(e) => return Err((SkipKind::InvalidGlob, format!("apply_to glob {e}"))),
    }
    let tree = match origin {
        SkillOrigin::Base { tree, .. } => *tree,
        SkillOrigin::Fork { .. } => {
            let why = "the pull request comes from a fork; repository skills are not read";
            return Err((SkipKind::Fork, why.into()));
        }
        SkillOrigin::NoBase(reason) => return Err((SkipKind::NoBase, reason.clone())),
    };
    let path = format!(".claude/skills/{name}/SKILL.md");
    let over_cap = |n: usize| {
        let why = format!("{n} bytes is over the {SKILL_CAP_BYTES}-byte per-skill cap");
        (SkipKind::OverSkillCap, why)
    };
    let bytes = match tree.read(&path, SKILL_CAP_BYTES) {
        BlobRead::Bytes(b) if b.len() > SKILL_CAP_BYTES => return Err(over_cap(b.len())),
        BlobRead::Bytes(b) => b,
        BlobRead::TooLarge(n) => return Err(over_cap(n)),
        BlobRead::Missing => {
            return Err((
                SkipKind::NotFound,
                format!("{path} is absent at the base commit"),
            ));
        }
        BlobRead::NotRegular(mode) => {
            let why = format!("{path} or a directory above it is not a regular file (mode {mode})");
            return Err((SkipKind::Unreadable, why));
        }
        BlobRead::Error(e) => return Err((SkipKind::Unreadable, e)), // #11899: the store names its source
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Err((SkipKind::Unreadable, "SKILL.md is not UTF-8".into()));
    };
    // #11899: refuse any text that could read as a fence marker, lookalikes included.
    if let Some(why) = render::fence_lookalike(text) {
        return Err((SkipKind::Unreadable, format!("SKILL.md {why}")));
    }
    let doc =
        skill_format::parse_skill(name, text).map_err(|e| (SkipKind::InvalidFrontmatter, e))?;
    if *total + bytes.len() > TOTAL_CAP_BYTES {
        let why = format!("adding it would pass the {TOTAL_CAP_BYTES}-byte total cap");
        return Err((SkipKind::OverTotalCap, why));
    }
    *total += bytes.len();
    Ok((
        Sha256Digest::of_bytes(&bytes).to_string(),
        bytes.len(),
        doc.body,
    ))
}

/// Whether a skill with `apply_to` applies to a PR changing `changed`.
///
/// What: `None` or an empty list applies to the whole PR (DOC-93 RS-7). Globs
/// are globset globs with `literal_separator`: `*` and `?` stay within one
/// path segment, `**` crosses segments (RS-8).
fn applies(apply_to: Option<&[String]>, changed: &[String]) -> Result<bool, String> {
    let Some(globs) = apply_to.filter(|g| !g.is_empty()) else {
        return Ok(true);
    };
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        let compiled = GlobBuilder::new(glob)
            .literal_separator(true)
            .build()
            .map_err(|e| format!("{glob:?} does not parse: {e}"))?;
        builder.add(compiled);
    }
    let set = builder.build().map_err(|e| e.to_string())?;
    Ok(changed.iter().any(|path| set.is_match(path)))
}
