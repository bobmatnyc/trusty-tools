//! Per-assistant keying for the `MemoryStore` palaces (#7443, epic #7425).
//!
//! Why: both palace-backed stores — [`crate::memory::trusty_backed`] and
//! [`crate::memory::trusty_client`] — used to derive a palace id from the
//! content-type [`Segment`] alone, so `Segment::AgentMemory` resolved to ONE
//! process-global `trusty-agents-mem` palace. The native memory tools
//! (`memory_recall`, `store_memory`, `retrieve_memory`, `list_memory_keys`)
//! address exactly that segment, so the moment a backend is wired in, two
//! assistants served by one process share one drawer. [`MemoryScope`] is the
//! assistant identity that goes into the key, and it is resolved by #7428's
//! [`resolve_palace_plan`] rather than by a second rule of this module's own.
//! What: [`MemoryScope`] is a validated palace-key component;
//! [`MemoryScope::for_assistant`] resolves one from an agent name plus its
//! `[[stores]]` binding, and FAILS when that resolution produces no palace —
//! never a fall-through to the unscoped id. [`palace_id`] is the ONE place
//! either store formats a palace id, scoped or not.
//! Test: `super::scope_tests` — the whole module.

use std::path::{Path, PathBuf};

use crate::assistants::memory::{resolve_palace_plan, resolve_palace_plan_in};
use crate::memory::store::Segment;
use crate::stores::AgentStoreBinding;

/// Why a [`MemoryScope`] could not be produced.
///
/// Why: the native memory tools must refuse to run rather than write into the
/// shared palace, so every failure here is terminal for that tool call — see
/// [`crate::tools::native_memory::open_assistant_memory_backend`].
/// Test: `super::scope_tests::an_unresolvable_agent_name_is_an_error`,
/// `super::scope_tests::a_traversing_palace_id_is_rejected`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MemoryScopeError {
    /// #7428's resolution produced no palace for this agent — it is not a
    /// usable assistant instance id and its binding pins nothing.
    #[error("agent '{agent}' resolves to no memory palace; refusing the shared palace")]
    Unresolved {
        /// The agent name that failed to resolve.
        agent: String,
    },
    /// The resolved palace id cannot be used as a key component.
    #[error("memory palace id {palace:?} is not a usable key component")]
    Unusable {
        /// The offending palace id.
        palace: String,
    },
}

/// One assistant's share of the memory-store keyspace.
///
/// Why: a palace id is joined onto a data root as a directory name by
/// [`crate::memory::trusty_backed`], so an unvalidated component could name
/// somewhere other than a fresh child of that root — `..` escapes it and `.` IS
/// it, which puts every assistant back in one shared directory. Validating once,
/// at construction, means every consumer of a `MemoryScope` holds a value that
/// is already safe to interpolate.
/// What: ONE path segment — a non-blank string of ASCII alphanumerics, `-`, `_`
/// and `.`, which excludes every separator, with the two dot-segments `.` and
/// `..` refused by exact match. A leading dot is only refused when the WHOLE
/// name is a dot-segment, so `..foo` and `.cache` stay legal literal names.
/// Test: `super::scope_tests::a_traversing_palace_id_is_rejected`,
/// `super::scope_tests::two_assistants_key_two_palaces`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MemoryScope(String);

impl MemoryScope {
    /// Wrap an already-resolved palace id.
    ///
    /// Why: the #7428 resolution and this crate's tests both arrive with a
    /// palace id in hand; validation is the only thing left to do.
    /// What: trims, then rejects blank, either dot-segment, and any character
    /// outside `[A-Za-z0-9._-]` (which is what excludes every separator).
    /// Test: `super::scope_tests::a_traversing_palace_id_is_rejected`.
    pub fn new(palace: impl AsRef<str>) -> Result<Self, MemoryScopeError> {
        let trimmed = palace.as_ref().trim();
        // #7443: `..` escapes the data root and `.` names the root itself, which
        // puts every assistant back in one directory — refuse both.
        let usable = !trimmed.is_empty()
            && !matches!(trimmed, "." | "..")
            && trimmed
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !usable {
            return Err(MemoryScopeError::Unusable {
                palace: trimmed.to_string(),
            });
        }
        Ok(Self(trimmed.to_string()))
    }

    /// The scope `name` writes and reads under, per #7428's resolution.
    ///
    /// Why: #7443 must not introduce a second notion of "which assistant is
    /// this" — the key is [`resolve_palace_plan`]'s own palace, so the native
    /// tools land beside the chat path's memories rather than in a parallel
    /// namespace keyed by some other rule.
    /// What: [`resolve_palace_plan`]'s `own` palace, wrapped by
    /// [`Self::new`]. A `PalaceSource::Unresolved` plan is an error, never the
    /// unscoped palace.
    /// Test: `super::scope_tests::an_unresolvable_agent_name_is_an_error`,
    /// `super::scope_tests::the_scope_is_the_7428_palace`.
    pub fn for_assistant(
        name: &str,
        binding: Option<&AgentStoreBinding>,
    ) -> Result<Self, MemoryScopeError> {
        // #7443: the key is #7428's resolved palace, not a new config knob.
        Self::from_plan(name, resolve_palace_plan(name, binding).own)
    }

    /// [`Self::for_assistant`] against explicit agent dirs and assistants root.
    ///
    /// Why/What: the `_in` suffix is this crate's injected-dependency
    /// convention — see [`resolve_palace_plan_in`]. Tests pass tempdirs so a
    /// developer's real `~/.trusty-agents` cannot decide the key.
    /// Test: `super::scope_tests::the_scope_is_the_7428_palace`,
    /// `super::scope_tests::an_unresolvable_agent_name_is_an_error`.
    pub fn for_assistant_in(
        dirs: &[PathBuf],
        root: &Path,
        name: &str,
        binding: Option<&AgentStoreBinding>,
    ) -> Result<Self, MemoryScopeError> {
        Self::from_plan(name, resolve_palace_plan_in(dirs, root, name, binding).own)
    }

    /// Turn a resolved plan's own palace into a scope, or the refusal.
    fn from_plan(name: &str, own: Option<String>) -> Result<Self, MemoryScopeError> {
        let own = own.ok_or_else(|| MemoryScopeError::Unresolved {
            agent: name.to_string(),
        })?;
        Self::new(own)
    }

    /// The validated key component.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The palace id backing `segment` for `scope` — the ONE formatter.
///
/// Why: `trusty_backed` and `trusty_client` each carried their own copy of this
/// format string, and the doc comment on one pointed at the other to keep them
/// in step by hand. One function is what actually keeps them in step, and it is
/// where #7443's assistant component goes in.
/// What: `trusty-agents-{scope}-{prefix}` when scoped; the pre-#7443
/// `trusty-agents-{prefix}` when not, which is what the single-tenant seeders
/// and CLI paths keep using.
/// Test: `super::scope_tests::two_assistants_key_two_palaces`,
/// `super::scope_tests::an_unscoped_palace_id_is_unchanged`.
pub fn palace_id(scope: Option<&MemoryScope>, segment: Segment) -> String {
    match scope {
        Some(scope) => format!("trusty-agents-{}-{}", scope.as_str(), segment.prefix()),
        None => format!("trusty-agents-{}", segment.prefix()),
    }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod scope_tests;
