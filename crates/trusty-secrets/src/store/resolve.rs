//! In-process `secret://` resolution for `tm secrets exec` (DOC-74 §9.4, §15.8).
//!
//! Why: `exec` resolves in-process, so a value never crosses a socket (§9.4).
//! It resolves one reference, or a whole env map built from `--env` (tier 1)
//! or `--dotenv` (tier 2). Under a Claude Code parent it may inject only keys
//! flagged "agents may use" (§15.8). The caller decides whether the parent
//! chain includes Claude Code; this module only enforces the flag.
//! What: [`resolve_reference`] and [`resolve_env`], with [`EnvEntry`] as
//! input and [`ResolvedVar`] as output. Lookup is [`SecretStore::read`]'s own
//! path: project vault, then owner vault, an explicit reference pins one of
//! those two vaults (#9328), and the backend read is uncached (§15.3, §15.5).
//! Test: `resolve_tests.rs` beside this file.

use std::fmt;

use super::{ScopeSet, SecretStore};
use crate::api::methods::KeyMeta;
use crate::api::{SCHEME, SecretRef, SecretValue, SecretsError, VaultName};

/// The `reference` text of an [`SecretsError::EnvResolution`] whose raw
/// value did not parse. The raw text itself is never reported.
const UNPARSABLE: &str = "an unparsable `secret://` reference";

/// Resolve one reference to its value.
///
/// Why: the single choke point every `exec` path goes through, so the agents
/// flag is enforced in one place (DOC-74 §15.8).
/// What: locates the key in the names-only index (project vault, then owner
/// vault; an explicit `<owner>/KEY` or `<owner>/<repo>/KEY` searches only
/// that vault, which must be one of `scopes`, else
/// [`SecretsError::VaultOutOfScope`] before any read — #9328). With
/// `agent_parent`, a key whose "agents may use" flag is OFF is
/// [`SecretsError::AgentUseRefused`], raised before the value is read. #9070:
/// the flag is the backend's flag item, never the index row's field; a
/// failed flag lookup is an error, never ON. Without `agent_parent` the
/// flag is not looked up. A key with no index row is [`SecretsError::NotFound`] in either
/// mode, and the backend is not read. Otherwise the uncached backend read.
/// Test: `resolve_unscoped_prefers_project_over_owner`,
/// `resolve_explicit_reference_reads_only_its_vault`,
/// `resolve_pinned_reference_outside_the_scopes_is_refused`,
/// `resolve_miss_is_not_found`,
/// `resolve_agent_gate_refuses_flag_off_before_any_read`,
/// `resolve_agent_gate_refuses_a_key_with_no_row_without_reading`,
/// `resolve_agent_gate_allows_a_flagged_key`,
/// `resolve_agent_gate_ignores_a_hand_edited_index_flag`,
/// `resolve_agent_gate_fails_closed_when_the_flag_read_fails`.
pub fn resolve_reference(
    store: &SecretStore,
    scopes: &ScopeSet,
    reference: &SecretRef,
    agent_parent: bool,
) -> Result<SecretValue, SecretsError> {
    if !agent_parent {
        // #9070: no agent parent, no gate, so no flag lookup.
        return store.read(reference, scopes);
    }
    // #7525: judged on the located row, before any read. #9070: the row's
    // flag was filled from the backend by `read_admitted`.
    store.read_admitted(reference, scopes, admit_for_agent)
}

/// Refuse a reference whose key is not flagged "agents may use", reading
/// no value.
///
/// Why: DOC-74 §15.8 — the flag rule covers grant contents (tier 3), so
/// `secrets.grant` refuses such a key when its registrar runs under Claude
/// Code (#9070), by the same rule [`resolve_reference`] applies to a read.
/// What: locates the key as [`resolve_reference`] does and reads its flag
/// from the backend; flag OFF is [`SecretsError::AgentUseRefused`]. A key
/// with no index row is [`SecretsError::NotFound`]; a failed flag lookup
/// is its error, never ON.
/// Test: `grant_by_agent_ancestor_cannot_name_unflagged_key`.
// Only the `server` feature's `secrets.grant` calls it.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) fn require_agent_use(
    store: &SecretStore,
    scopes: &ScopeSet,
    reference: &SecretRef,
) -> Result<(), SecretsError> {
    let (vault, row) = store.locate_flagged(reference, scopes)?;
    admit_for_agent(&vault, &row)
}

/// The agents gate on a located row: flag OFF is a refusal.
fn admit_for_agent(vault: &VaultName, row: &KeyMeta) -> Result<(), SecretsError> {
    if !row.agents_may_use {
        return Err(SecretsError::AgentUseRefused {
            key: row.name.to_string(),
            vault: vault.to_string(),
        });
    }
    Ok(())
}

/// One `NAME=raw` entry of a child env map, before resolution.
///
/// Why: `--env` and `--dotenv` both reduce to an ordered list of entries in
/// which some raw values are `secret://` references.
/// What: the name and raw text, unvalidated; [`resolve_env`] validates.
/// `Debug` shows the name and the raw text's length, never the text.
/// Test: `resolve_sentinel_never_appears_in_output_types`.
#[derive(Clone, PartialEq, Eq)]
pub struct EnvEntry {
    name: String,
    raw: String,
}

impl EnvEntry {
    /// An entry. `raw` is either a literal value or a `secret://` reference.
    pub fn new(name: impl Into<String>, raw: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            raw: raw.into(),
        }
    }

    /// The env variable name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The raw text. Every call site is a deliberate disclosure point.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Whether [`resolve_env`] treats the raw text as a reference.
    ///
    /// What: true when the text, after leading whitespace, starts with
    /// `secret://` in any ASCII case. A near-miss (`SECRET://`, a leading
    /// space) then fails to parse instead of passing through as a literal.
    /// Test: `resolve_env_fails_closed_on_near_miss_references`.
    pub fn is_reference(&self) -> bool {
        self.raw
            .trim_start()
            .get(..SCHEME.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(SCHEME))
    }
}

impl fmt::Debug for EnvEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvEntry")
            .field("name", &self.name)
            .field("raw_chars", &self.raw.chars().count())
            .finish()
    }
}

/// Where a resolved env value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VarSource {
    /// The entry's raw text, passed through unchanged.
    Literal,
    /// The value the reference resolved to.
    Reference(SecretRef),
}

/// One resolved env entry, ready for `Command::env`.
///
/// Why: the result carries values, so its `Debug` must not print them.
/// What: every value, literal or resolved, is a [`SecretValue`], whose
/// `Debug` redacts; read it with [`SecretValue::expose`]. `source` tells the
/// caller which values came from a reference (the ones to scrub from the
/// child's output).
/// Test: `resolve_sentinel_never_appears_in_output_types`.
// #9328: `#[non_exhaustive]`; only this crate builds one.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResolvedVar {
    /// The env variable name.
    pub name: String,
    /// The value to inject.
    pub value: SecretValue,
    /// Literal or resolved.
    pub source: VarSource,
}

/// Resolve every `secret://` reference in an env map, all or nothing.
///
/// Why: a literal `secret://…` string must never reach a child env, and a
/// child must never start with a partial env (DOC-74 §15.8 tier 1 and 2).
/// What: first validates every entry — a POSIX name, no NUL in the value —
/// and parses every reference, before any backend read. Then resolves each
/// reference through [`resolve_reference`] in order; other entries pass
/// through unchanged. Any failure aborts the call with an error naming the
/// entry (position or env name) and the canonical reference, never a value
/// or the raw text. Only a whole-value reference is resolved; `secret://`
/// inside a longer value is literal text.
/// Test: `resolve_env_passes_plain_values_through`,
/// `resolve_env_fails_closed_on_one_bad_reference`,
/// `resolve_env_fails_closed_on_near_miss_references`,
/// `resolve_env_rejects_invalid_names`.
pub fn resolve_env(
    store: &SecretStore,
    scopes: &ScopeSet,
    entries: &[EnvEntry],
    agent_parent: bool,
) -> Result<Vec<ResolvedVar>, SecretsError> {
    let mut planned = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let invalid = |reason| SecretsError::InvalidEnvEntry {
            position: index + 1,
            reason,
        };
        validate_env_name(&entry.name).map_err(invalid)?;
        if entry.raw.contains('\0') {
            return Err(invalid("the value contains a NUL byte"));
        }
        let reference = if entry.is_reference() {
            let parsed =
                SecretRef::parse(&entry.raw).map_err(|source| SecretsError::EnvResolution {
                    name: entry.name.clone(),
                    reference: UNPARSABLE.to_string(),
                    source: Box::new(source),
                })?;
            Some(parsed)
        } else {
            None
        };
        planned.push((entry, reference));
    }
    planned
        .into_iter()
        .map(|(entry, reference)| match reference {
            None => Ok(ResolvedVar {
                name: entry.name.clone(),
                value: SecretValue::new(entry.raw.clone()),
                source: VarSource::Literal,
            }),
            Some(reference) => match resolve_reference(store, scopes, &reference, agent_parent) {
                Ok(value) => Ok(ResolvedVar {
                    name: entry.name.clone(),
                    value,
                    source: VarSource::Reference(reference),
                }),
                Err(source) => Err(SecretsError::EnvResolution {
                    name: entry.name.clone(),
                    reference: reference.to_string(),
                    source: Box::new(source),
                }),
            },
        })
        .collect()
}

/// Check a POSIX env variable name: `[A-Za-z_][A-Za-z0-9_]*`.
///
/// What: the portable set excludes `=`, NUL and whitespace, so a name that
/// passes can never split or truncate an env entry. Errors are fixed reasons.
/// Test: `resolve_env_rejects_invalid_names`.
pub(crate) fn validate_env_name(name: &str) -> Result<(), &'static str> {
    let mut chars = name.chars();
    match chars.next() {
        None => Err("the variable name is empty"),
        Some(first) if !(first.is_ascii_alphabetic() || first == '_') => {
            Err("the variable name must start with an ASCII letter or `_`")
        }
        Some(_) if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') => {
            Err("the variable name may hold only ASCII letters, digits and `_`")
        }
        Some(_) => Ok(()),
    }
}
