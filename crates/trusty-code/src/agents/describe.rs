//! `agents.describe` — the effective configuration, provenance, and warnings
//! for ONE agent (#2074, epic #2892).
//!
//! Why: `agents.list` answers "what could I dispatch?" with four fields per row
//! — name, tier, description, model. An operator debugging drift needs the next
//! question answered: which file on disk is this, did Trusty Code write it, has
//! anyone edited it since, and what is actually in the prompt. Slice 1 put a
//! file, a checksum, and an [`Origin`] on disk for every shipped role
//! (`crate::agents::deploy`); this module is what reads them back out.
//!
//! What: [`describe_agent`] resolves one name through
//! [`crate::agents::resolve_agent`] — the SAME disk-wins/embedded-fallback/
//! plugin chain dispatch uses, never a second copy of it — and pairs the
//! resolved config with the deployed-agent ledger sitting beside the file.
//! [`load_ledger`] reads that ledger once per request, and
//! [`disk_entry_has_warnings`] lets `agents.list` reuse the same judgement for
//! its per-row `has_warnings` flag without a second ledger parse.
//!
//! ## What this surface deliberately does NOT report
//!
//! Trusty Code has no capability-grant model yet: an agent's declared authority
//! is the flat `tools.allowed` allowlist
//! ([`crate::agents::config::ToolsConfig`]) and nothing else. #2074's
//! write/shell/network/credential/delegation grants are a later slice, so this
//! payload reports the allowlist that exists and carries NO `grants` key.
//! An absent field is the honest answer; a fabricated one would tell an
//! operator the runtime enforces something it does not.
//!
//! Test: `crate::agents::describe::tests::*`.
//!
//! [`Origin`]: trusty_agents_common::agents::manifest::Origin

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use trusty_agents_common::agents::manifest::{AgentManifest, MANIFEST_FILE, ManifestLoad, Origin};
// #4698: the file's own `provenance:` claim, reported beside the ledger's origin.
use trusty_agents_common::agents::metadata::agent_metadata_from_str;
use trusty_agents_common::agents::provenance::Provenance;

use crate::jsonrpc::RpcError;
use crate::paths::TRUSTY_CODE_DIRNAME;

use super::deploy::AGENTS_DIRNAME;
use super::{ResolveAgentError, resolve_agent};

/// The deployed-agent ledger's state for one agents directory.
///
/// Why: `AgentManifest::load_checked` reports a MISSING ledger and an EMPTY one
/// identically (`Ok`), and the two mean different things here — a missing
/// ledger in Trusty Code's own roster directory is an anomaly worth a warning,
/// while an empty one is a deploy that tracked nothing. Testing the file itself
/// is what separates them, exactly as `crate::agents::deploy::
/// roster_manifest_status` already does for `tcode paths show`.
/// What: absent, parsed, or unreadable-with-detail.
/// Test: `absent_ledger_in_the_native_roster_dir_warns`,
/// `corrupt_ledger_is_reported_as_a_warning`.
pub(crate) enum LedgerState {
    /// No ledger file exists in this directory.
    Absent,
    /// The ledger parsed.
    Ok(AgentManifest),
    /// The ledger exists but could not be read; carries the underlying detail.
    Corrupt(String),
}

/// Read the deployed-agent ledger in `dir` once.
///
/// Why: `agents.list` describes up to a whole roster in one call. Loading the
/// ledger per row would re-parse the same JSON once per agent; hoisting it to
/// the caller keeps the listing one parse regardless of roster size.
/// What: distinguishes an absent file from an empty ledger by testing the file,
/// then defers to `AgentManifest::load_checked` for the parse.
/// Test: `absent_ledger_in_the_native_roster_dir_warns`,
/// `deployed_pristine_agent_reports_framework_origin_and_no_warnings`.
pub(crate) fn load_ledger(dir: &Path) -> LedgerState {
    if !dir.join(MANIFEST_FILE).exists() {
        return LedgerState::Absent;
    }
    match AgentManifest::load_checked(dir) {
        ManifestLoad::Ok(manifest) => LedgerState::Ok(manifest),
        ManifestLoad::Corrupt(detail) => LedgerState::Corrupt(detail),
    }
}

/// What the ledger records about one agent file, plus anything wrong with it.
///
/// Why: the JSON shape and the warning list are derived from the same three
/// facts (is the ledger readable, does it track this file, does the file still
/// match its recorded checksum), so they are computed together rather than by
/// two passes that could disagree.
/// What: a stable `manifest` token plus the ledger entry's fields when there is
/// one, and the human-readable warnings those facts imply.
/// Test: `deployed_pristine_agent_reports_framework_origin_and_no_warnings`,
/// `hand_edited_agent_reports_a_checksum_mismatch_and_warns`,
/// `untracked_disk_file_warns_that_trusty_code_did_not_write_it`.
struct DiskProvenance {
    /// `"absent"`, `"present"`, or `"corrupt"`.
    manifest: &'static str,
    /// The ledger's recorded origin, when it tracks this file.
    origin: Option<Origin>,
    /// `Origin::is_framework_owned` for that origin.
    framework_owned: Option<bool>,
    /// `"match"` or `"mismatch"`, when a checksum comparison was possible.
    checksum: Option<&'static str>,
    /// The ledger's RFC3339 deploy timestamp, when recorded.
    deployed_at: Option<String>,
    /// The resolved `extends:` chain the deploy composed, base-first.
    source_chain: Vec<String>,
    /// The `provenance:` the FILE declares about itself (#4698), independent of
    /// the ledger. `None` when the file declares none or could not be read — an
    /// absent field means user-authored, per #4698's design note.
    declared_provenance: Option<Provenance>,
    /// Everything an operator should know that is not simply "this is fine".
    warnings: Vec<String>,
}

impl DiskProvenance {
    /// The wire shape, with every absent fact rendered as an explicit `null`.
    ///
    /// Why: a client reading `provenance.origin` must be able to tell "not
    /// recorded" from "key missing because the server is older" — so the keys
    /// are always present and it is the VALUES that go null.
    /// What: a fixed seven-key object. `warnings` is not included here; the
    /// caller merges them into the response's single top-level `warnings` list
    /// so a client has one place to look.
    fn to_json(&self) -> Value {
        json!({
            "manifest": self.manifest,
            "origin": self.origin.map(origin_token),
            "framework_owned": self.framework_owned,
            "checksum": self.checksum,
            "deployed_at": self.deployed_at,
            "source_chain": self.source_chain,
            // #4698: the file's own claim, beside the ledger's. A client
            // comparing the two sees a hand-edit the checksum alone would not
            // explain; the ledger stays authoritative either way.
            "declared_provenance": self.declared_provenance.map(Provenance::as_str),
        })
    }
}

/// The stable lowercase token for an [`Origin`].
///
/// Why: the manifest's own serde rendering is lowercase; matching it by hand
/// here keeps the JSON-RPC payload identical to the on-disk ledger's spelling
/// without depending on a `Serialize` round-trip through a `Value`.
/// What: `"bundled"`, `"registry"`, `"user"`.
/// Test: `deployed_pristine_agent_reports_framework_origin_and_no_warnings`.
fn origin_token(origin: Origin) -> &'static str {
    match origin {
        Origin::Bundled => "bundled",
        Origin::Registry => "registry",
        Origin::User => "user",
    }
}

/// The provenance placeholder for an agent with no file on disk.
///
/// Why: an embedded or plugin agent has no ledger entry and never will, which
/// is not the same condition as a missing ledger. Naming it keeps a client from
/// reading `"absent"` as "something is wrong".
/// What: the same seven keys [`DiskProvenance::to_json`] emits, with
/// `manifest: "not-applicable"`.
/// Test: `embedded_agent_has_no_disk_path_and_no_warnings`.
fn not_applicable_provenance() -> Value {
    json!({
        "manifest": "not-applicable",
        "origin": Value::Null,
        "framework_owned": Value::Null,
        "checksum": Value::Null,
        "deployed_at": Value::Null,
        "source_chain": [],
        "declared_provenance": Value::Null,
    })
}

/// Whether `dir` is Trusty Code's OWN `<project>/.trusty-code/agents`.
///
/// Why: a missing ledger means different things in different roots. In the
/// native root it means the roster deploy has not run (or its ledger was
/// deleted) — worth saying. In a `.claude/agents` compatibility root, no ledger
/// is expected at all, and warning there would flag every hand-authored project
/// agent as suspect.
/// What: the last two path components, compared against
/// [`crate::paths::TRUSTY_CODE_DIRNAME`] and
/// [`crate::agents::deploy::AGENTS_DIRNAME`] — the same two names the deploy
/// target is built from, never a re-spelled literal.
/// Test: `absent_ledger_in_the_native_roster_dir_warns`,
/// `absent_ledger_in_a_compat_root_does_not_warn`.
fn is_native_roster_dir(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == AGENTS_DIRNAME)
        && dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == TRUSTY_CODE_DIRNAME)
}

/// Read the ledger's record of `<name>.md` in `dir`, and what is wrong with it.
///
/// Why: this is the whole of #2074's "source provenance" requirement for a
/// disk-backed agent — did Trusty Code write this file, when, from which
/// compose chain, and does its content still match what was written.
/// What: four terminal cases, in order — an unreadable ledger, no ledger, a
/// ledger that does not track this file, and a tracked file whose on-disk bytes
/// either match its recorded checksum or no longer do. Checksum comparison
/// reuses `AgentManifest::checksum_matches`, the same call
/// `crate::agents::deploy::ensure_roster_deployed` selects against, so
/// "diverges" means exactly what the deployer means by it.
/// Test: `deployed_pristine_agent_reports_framework_origin_and_no_warnings`,
/// `hand_edited_agent_reports_a_checksum_mismatch_and_warns`,
/// `untracked_disk_file_warns_that_trusty_code_did_not_write_it`,
/// `corrupt_ledger_is_reported_as_a_warning`.
fn provenance_for(dir: &Path, ledger: &LedgerState, name: &str) -> DiskProvenance {
    let filename = format!("{name}.md");
    let mut provenance = DiskProvenance {
        manifest: "absent",
        origin: None,
        framework_owned: None,
        checksum: None,
        deployed_at: None,
        source_chain: Vec::new(),
        declared_provenance: None,
        warnings: Vec::new(),
    };

    let manifest = match ledger {
        LedgerState::Corrupt(detail) => {
            provenance.manifest = "corrupt";
            provenance.warnings.push(format!(
                "the deployed-agent manifest in {} could not be read ({detail}); the provenance \
                 of '{name}' is unavailable until that file is repaired or deleted",
                dir.display()
            ));
            return provenance;
        }
        LedgerState::Absent => {
            if is_native_roster_dir(dir) {
                provenance.warnings.push(format!(
                    "manifest missing: no deployed-agent manifest in {}, so '{name}' has no \
                     recorded origin or checksum. Start Trusty Code against this project to \
                     materialize the roster.",
                    dir.display()
                ));
            }
            return provenance;
        }
        LedgerState::Ok(manifest) => manifest,
    };

    provenance.manifest = "present";
    let Some(entry) = manifest.managed.get(&filename) else {
        provenance.warnings.push(format!(
            "'{filename}' is not tracked by the deployed-agent manifest in {}; Trusty Code did \
             not write this file, so it has no recorded origin or checksum",
            dir.display()
        ));
        return provenance;
    };

    provenance.origin = Some(entry.origin);
    provenance.framework_owned = Some(entry.origin.is_framework_owned());
    provenance.deployed_at = Some(entry.deployed_at.clone());
    provenance.source_chain = entry.source_chain.clone();

    match std::fs::read_to_string(dir.join(&filename)) {
        Ok(current) if manifest.checksum_matches(&filename, &current) => {
            // #4698: report what the file says about its own author beside what
            // the ledger recorded, so a client sees both records.
            provenance.declared_provenance = agent_metadata_from_str(&current).provenance;
            provenance.checksum = Some("match");
        }
        Ok(current) => {
            provenance.declared_provenance = agent_metadata_from_str(&current).provenance;
            provenance.checksum = Some("mismatch");
            provenance.warnings.push(format!(
                "the disk copy of '{name}' diverges from the bundled roster: its content no \
                 longer matches the checksum the manifest recorded. That file is what runs, and \
                 Trusty Code will not overwrite it."
            ));
        }
        Err(e) => provenance.warnings.push(format!(
            "'{filename}' is tracked by the deployed-agent manifest but could not be read ({e}); \
             its checksum could not be verified"
        )),
    }

    provenance
}

/// Whether `agents.list` should flag this disk row for an operator's attention.
///
/// Why: the listing's `has_warnings` flag and `agents.describe`'s `warnings`
/// array must never disagree — a row flagged clean that describes with three
/// warnings is worse than no flag at all. Both read this one function.
/// What: `true` when [`provenance_for`] produced any warning. A row whose file
/// failed to PARSE is flagged by its caller instead, which is where the parse
/// error is known.
/// Test: `list_flags_a_hand_edited_row_and_leaves_pristine_rows_clean`.
pub(crate) fn disk_entry_has_warnings(dir: &Path, ledger: &LedgerState, name: &str) -> bool {
    !provenance_for(dir, ledger, name).warnings.is_empty()
}

/// The `<dir>/<name>.md` this agent resolves from, when there is one.
///
/// Why: the tier, the reported path, and whether provenance applies all hinge
/// on this single question, so it is asked once.
/// What: `None` for a namespaced `<plugin>:<name>` (plugin agents live under
/// `.claude/plugins/`, not in `dir`) and `None` when no such file exists. The
/// separator rejection is defense in depth — every caller validates the name
/// through `crate::agents::protocol::validate_agent_name` first, which already
/// makes a traversing name unreachable.
/// Test: `embedded_agent_has_no_disk_path_and_no_warnings`,
/// `deployed_pristine_agent_reports_framework_origin_and_no_warnings`.
fn disk_file(dir: &Path, name: &str) -> Option<PathBuf> {
    if name.contains(':') || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    let path = dir.join(format!("{name}.md"));
    path.is_file().then_some(path)
}

/// Describe one agent: effective config, provenance, and warnings.
///
/// Why: #2074's "expose effective instructions, model routing, tools, skills,
/// source provenance, and warnings through agent inspection".
///
/// What, in order:
/// 1. Resolve `name` through [`crate::agents::resolve_agent`], so describe and
///    dispatch can never disagree about which definition wins.
/// 2. Label the tier from where it resolved: `disk_tier` (`"project"` or
///    `"user"`) when a file in `dir` backs it, `"plugin"` for a namespaced
///    name, `"embedded"` otherwise, and `"broken"` when a file exists but
///    failed to parse or compose.
/// 3. Attach the ledger's provenance for a disk-backed agent, or the
///    `"not-applicable"` placeholder for one that has no file.
/// 4. Report the resolved instruction text's length; include the text itself
///    only when `include_instructions` is set, because a composed roster prompt
///    runs to tens of kilobytes and a catalog client does not want it by
///    default.
///
/// A name that resolves nowhere is a typed `-32002 not_found` carrying
/// `resolve_agent`'s own available-agents list — never a panic. A name whose
/// FILE is broken is NOT an error: it returns a normal payload with
/// `tier: "broken"` and the parse failure in `warnings`, because an operator
/// debugging why a name fell back needs to see the reason, not a bare RPC
/// error (the same rule `agents.list` already applies to a broken row).
///
/// Test: `deployed_pristine_agent_reports_framework_origin_and_no_warnings`,
/// `hand_edited_agent_reports_a_checksum_mismatch_and_warns`,
/// `unknown_name_is_a_typed_not_found`,
/// `unparseable_disk_agent_is_broken_with_the_parse_error`,
/// `embedded_agent_has_no_disk_path_and_no_warnings`,
/// `instructions_text_is_omitted_by_default_and_returned_on_request`,
/// `describe_reports_the_tools_allowlist_and_declared_skills`.
pub(crate) fn describe_agent(
    dir: &Path,
    disk_tier: &str,
    name: &str,
    include_instructions: bool,
) -> Result<Value, RpcError> {
    let disk_path = disk_file(dir, name);
    let mut warnings: Vec<String> = Vec::new();

    let (tier, cfg) = match resolve_agent(dir, name) {
        Ok(cfg) => {
            let tier = if disk_path.is_some() {
                disk_tier
            } else if name.contains(':') {
                "plugin"
            } else {
                "embedded"
            };
            (tier, Some(cfg))
        }
        // #2074: a file that exists but will not load is reported, not raised —
        // this is the "why did my override fall back?" case.
        Err(ResolveAgentError::Load { source, .. }) => {
            warnings.push(format!("parse error: {source:#}"));
            ("broken", None)
        }
        Err(e @ ResolveAgentError::NotFound { .. }) => {
            return Err(RpcError::not_found(e.to_string()));
        }
    };

    let provenance = if disk_path.is_some() {
        let mut p = provenance_for(dir, &load_ledger(dir), name);
        warnings.append(&mut p.warnings);
        p.to_json()
    } else {
        not_applicable_provenance()
    };

    let instructions = cfg.as_ref().map(|c| {
        let text = &c.system_prompt.content;
        let mut value = json!({ "length_bytes": text.len() });
        if include_instructions {
            value["text"] = json!(text);
        }
        value
    });

    Ok(json!({
        "name": cfg.as_ref().map_or_else(|| name.to_string(), |c| c.agent.name.clone()),
        "tier": tier,
        "path": disk_path.as_ref().map(|p| p.display().to_string()),
        "role": cfg.as_ref().and_then(|c| c.agent.role.clone()),
        "description": cfg.as_ref().and_then(|c| c.agent.description.clone()),
        "model": cfg.as_ref().and_then(|c| c.agent.model.clone()),
        "tools": cfg.as_ref().map(|c| {
            json!({ "allowed": c.tools.as_ref().and_then(|t| t.allowed.clone()) })
        }),
        "skills": cfg
            .as_ref()
            .map(|c| c.system_prompt.append_skills.clone())
            .unwrap_or_default(),
        "instructions": instructions,
        "provenance": provenance,
        "warnings": warnings,
    }))
}

#[cfg(test)]
#[path = "describe_tests.rs"]
mod tests;
