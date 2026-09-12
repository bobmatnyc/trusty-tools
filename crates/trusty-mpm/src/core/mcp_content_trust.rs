//! Content equivalence between a project-declared MCP server and one the
//! operator already has (#7672).
//!
//! Why: #7422 gated `<cwd>/.mcp.json` and `[session] mcp_servers` on
//! [`crate::core::project_trust::is_project_trusted`] because both surfaces ship
//! WITH the clone, so an entry spelling `{"command":"sh","args":["-c","curl … |
//! sh"]}` would execute unattended against a hostile repository. That gate is
//! correct about arbitrary content and wrong about the ordinary case: nearly
//! every real `.mcp.json` entry names a server the operator installed
//! themselves, so the warning fired on projects that were asking for nothing
//! new. The owner ruling (2026-09-12) is TRUST BY CONTENT — an entry whose
//! EXECUTABLE SPEC already exists outside the repository is not a new grant,
//! because running it changes nothing the operator has not already accepted.
//!
//! THE REPOSITORY SUPPLIES THE CLAIM, NEVER THE EVIDENCE. The known set is
//! built only from out-of-repo sources: the trusty-* framework builtins
//! ([`builtin_server_entry`]) and the operator's own user-scope registry — the
//! `mcpServers` map of the tm-managed `.claude.json` that `tm mcp add` writes.
//! A repository can name those, never define them.
//!
//! REGISTERING A SERVER IS NOT SHARING IT (PR #7692 review). A registry entry
//! contributes to the known set only when the operator has marked it shareable
//! with projects — [`crate::core::mcp_share`], set by
//! `tm mcp add --share-with-projects` or `tm mcp share <name>`. Without that
//! flag an exact spec match is still UNKNOWN, because this workspace's own
//! convention ships credential-bearing servers with an EMPTY `env` (the secret
//! arrives from the ambient environment or a `tm-secrets` exec wrapper), so a
//! spec comparison cannot distinguish the operator's real server from a
//! hostile repository's copy of its fully public shape. The builtins stay
//! unconditionally matchable: their command is framework-controlled.
//!
//! A MATCHING NAME IS NEVER SUFFICIENT, and a reserved builtin name is
//! stricter still: an entry called `trusty-memory` matches only evidence under
//! THAT name — the canonical `trusty-memory` builtin, or the operator's own
//! `trusty-memory` registry entry. A spoof therefore cannot borrow some
//! unrelated registered server's content to shadow a framework name, while a
//! real declaration that adds an `env` block the builtin lacks still matches
//! the operator's own registration of it.
//!
//! FAIL CLOSED ON EVERY DOUBT. A malformed entry, a field this module does not
//! model, a non-string arg or env value, an unresolvable command, an unreadable
//! registry — each yields UNKNOWN, which is exactly today's behavior.
//!
//! What: [`KnownServers::from_registry`] normalizes the known set once per
//! launch; [`KnownServers::classify`] normalizes one candidate and returns a
//! [`Verdict`] — `Known`, `UnsharedMatch` (equal to a registry entry the
//! operator has not shared, so the warning can name the `tm mcp share` that
//! would load it), or `Unknown`.
//! Test: `mcp_content_trust_tests.rs`, plus the end-to-end classification in
//! `session_mcp_scope_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::core::mcp_config::{BUILTIN_MANAGED_MCP_SERVERS, builtin_server_entry};

/// The `mcpServers` entry keys that describe a stdio server's execution.
///
/// Why: an entry carrying a key this module does not model could change what
/// runs in a way the comparison never saw, so an unmodelled key is UNKNOWN
/// rather than ignored.
/// What: the stdio and remote key sets, each including the `type` discriminant.
const STDIO_KEYS: &[&str] = &["type", "command", "args", "env"];
/// Remote entry keys — see [`STDIO_KEYS`].
const REMOTE_KEYS: &[&str] = &["type", "url", "headers"];

/// A command as this module compares it.
///
/// Why: two spellings of one binary (`trusty-memory` on `PATH` and
/// `/usr/local/bin/trusty-memory`) are the same executable, so the comparison
/// resolves before it compares. Resolution can legitimately fail — a server the
/// operator has registered but not yet installed — and the ruling allows an
/// identical unresolved spelling to match, but never a resolved path against an
/// unresolved name: that pair is exactly the case where the two could differ.
/// What: `Resolved` holds the canonicalized absolute path; `Unresolved` holds
/// the raw spelling.
/// Test: `identical_spellings_match_when_resolution_fails`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    /// The command resolved to a real executable.
    Resolved(PathBuf),
    /// Resolution failed; only an identical spelling can match.
    Unresolved(String),
}

/// One MCP server's executable spec, normalized for equality.
///
/// Why: JSON equality would reject an entry that differs only in key order, an
/// absent-versus-empty `env`, or an absent `type` — none of which changes what
/// runs. Normalizing first makes the comparison about execution, not encoding.
/// What: a stdio subprocess (resolved command, args in order, env as a sorted
/// key→value map) or a remote endpoint (transport, URL, headers as a sorted
/// map).
/// Test: `mcp_content_trust_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Spec {
    /// A local subprocess speaking MCP over stdio.
    Stdio {
        /// The resolved (or unresolvable) executable.
        command: Command,
        /// Arguments, compared in order and verbatim.
        args: Vec<String>,
        /// Environment keys and values, both compared.
        env: BTreeMap<String, String>,
    },
    /// A remote http or sse endpoint.
    Remote {
        /// `http` or `sse` — a different transport is a different server.
        transport: String,
        /// The endpoint URL, compared verbatim.
        url: String,
        /// Request headers, keys and values both compared.
        headers: BTreeMap<String, String>,
    },
}

/// What one project-declared entry was found to be.
///
/// Why: "not known" has two operator-actionable shapes, and collapsing them
/// would lose the one hint that actually helps — an entry that already equals a
/// registry server needs `tm mcp share`, not `tm project trust`.
/// What: `Known` loads; `UnsharedMatch(name)` and `Unknown` do not.
/// Test: `an_unshared_registry_match_is_reported_not_granted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Content-equivalent to a builtin or a shared registry entry — it loads.
    Known,
    /// Equal to the named registry entry, which the operator has not shared.
    UnsharedMatch(String),
    /// Matches nothing the operator has.
    Unknown,
}

/// What a project declaration may be measured against.
///
/// Why: normalizing the known set once per launch keeps the per-entry cost to
/// one candidate normalization, instead of re-resolving every builtin and
/// registry command for every entry in the file.
/// What: `builtins` maps each framework builtin name to its canonical spec,
/// always matchable; `shared` and `unshared` map each registry name to its
/// spec, split by the operator's [`crate::core::mcp_share`] decision. Only
/// `builtins` and `shared` can produce [`Verdict::Known`].
/// Test: `mcp_content_trust_tests.rs`.
pub struct KnownServers {
    /// Framework builtin name → its canonical, framework-controlled spec.
    builtins: BTreeMap<&'static str, Spec>,
    /// Registry name → spec, for entries the operator shared with projects.
    shared: BTreeMap<String, Spec>,
    /// Registry name → spec, for entries the operator has NOT shared.
    unshared: BTreeMap<String, Spec>,
}

impl KnownServers {
    /// Normalize the builtins and the operator's registry into a known set.
    ///
    /// Why: see the module doc — these two sources are the whole of the
    /// evidence, and neither is written by the repository being classified.
    /// What: takes the registry as an already-read `mcpServers` map (the caller
    /// owns the non-quarantining read, so a launch never renames the file that
    /// also holds OAuth state) and the `shared` names from
    /// [`crate::core::mcp_share`]. An entry that will not normalize is skipped,
    /// which can only ever shrink the known set.
    /// Test: `a_shared_registry_entry_makes_an_identical_declaration_known`,
    /// `an_empty_registry_knows_only_the_builtins`.
    pub fn from_registry(registry: &Map<String, Value>, shared: &BTreeSet<String>) -> Self {
        let builtins = BUILTIN_MANAGED_MCP_SERVERS
            .iter()
            .filter_map(|name| {
                builtin_server_entry(name)
                    .as_ref()
                    .and_then(normalize)
                    .map(|spec| (*name, spec))
            })
            .collect();
        let mut shared_specs: BTreeMap<String, Spec> = BTreeMap::new();
        let mut unshared_specs: BTreeMap<String, Spec> = BTreeMap::new();
        for (name, entry) in registry {
            let Some(spec) = normalize(entry) else {
                continue;
            };
            if shared.contains(name) {
                shared_specs.insert(name.clone(), spec);
            } else {
                unshared_specs.insert(name.clone(), spec);
            }
        }
        Self {
            builtins,
            shared: shared_specs,
            unshared: unshared_specs,
        }
    }

    /// What is this project-declared entry, relative to what the operator has?
    ///
    /// Why: this is the KNOWN/UNKNOWN decision itself. It answers only about
    /// content — the name decides which evidence applies, never whether the
    /// entry is acceptable.
    /// What: [`Verdict::Unknown`] for any entry that will not normalize. Under a
    /// framework builtin name, evidence must be under that SAME name — the
    /// canonical builtin, or the operator's registry entry of that name — so no
    /// unrelated server's spec can shadow a reserved name. Under any other
    /// name, any builtin or shared registry spec may match. An entry equal to an
    /// UNSHARED registry spec is [`Verdict::UnsharedMatch`], which does not
    /// load.
    /// Test: `a_matching_name_alone_is_not_enough`,
    /// `a_builtin_name_matches_only_evidence_under_that_name`,
    /// `an_unshared_registry_match_is_reported_not_granted`.
    pub fn classify(&self, name: &str, entry: &Value) -> Verdict {
        let Some(candidate) = normalize(entry) else {
            return Verdict::Unknown;
        };
        if let Some(canonical) = self.builtins.get(name) {
            if *canonical == candidate || self.shared.get(name) == Some(&candidate) {
                return Verdict::Known;
            }
            if self.unshared.get(name) == Some(&candidate) {
                return Verdict::UnsharedMatch(name.to_owned());
            }
            return Verdict::Unknown;
        }
        if self.builtins.values().any(|spec| *spec == candidate)
            || self.shared.values().any(|spec| *spec == candidate)
        {
            return Verdict::Known;
        }
        match self.unshared.iter().find(|(_, spec)| **spec == candidate) {
            Some((matched, _)) => Verdict::UnsharedMatch(matched.clone()),
            None => Verdict::Unknown,
        }
    }
}

/// Reduce one `mcpServers` entry to its executable spec.
///
/// Why: the single normalization both sides of every comparison pass through,
/// so a difference in encoding can never read as a difference in execution, and
/// a shape this module does not model can never read as a match.
/// What: `None` for a non-object entry, an unmodelled key, a `type` that is not
/// `stdio`/`http`/`sse`, a missing `command`/`url`, or a non-string arg, env
/// value or header value. An entry carrying `url` (or an `http`/`sse` type)
/// normalizes as remote, defaulting an absent type to `http`; everything else
/// normalizes as stdio, where an absent type means `stdio`.
/// Test: `an_unmodelled_key_is_never_known`, `a_malformed_entry_is_never_known`.
fn normalize(entry: &Value) -> Option<Spec> {
    let obj = entry.as_object()?;
    let declared = match obj.get("type") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) => Some(t.as_str()),
        Some(_) => return None,
    };

    if obj.contains_key("url") || matches!(declared, Some("http" | "sse")) {
        if !keys_within(obj, REMOTE_KEYS) {
            return None;
        }
        let transport = declared.unwrap_or("http");
        if transport != "http" && transport != "sse" {
            return None;
        }
        return Some(Spec::Remote {
            transport: transport.to_owned(),
            url: obj.get("url")?.as_str()?.to_owned(),
            headers: string_map(obj.get("headers"))?,
        });
    }

    if !keys_within(obj, STDIO_KEYS) {
        return None;
    }
    if declared.is_some_and(|t| t != "stdio") {
        return None;
    }
    let args = match obj.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect::<Option<Vec<String>>>()?,
        Some(_) => return None,
    };
    Some(Spec::Stdio {
        command: resolve_command(obj.get("command")?.as_str()?),
        args,
        env: string_map(obj.get("env"))?,
    })
}

/// Does this entry carry only keys the comparison models?
///
/// Why: see [`STDIO_KEYS`] — an unmodelled key fails closed.
fn keys_within(obj: &Map<String, Value>, allowed: &[&str]) -> bool {
    obj.keys().all(|key| allowed.contains(&key.as_str()))
}

/// Read an optional string→string JSON object as a sorted map.
///
/// Why: `env` and `headers` share one shape and one failure rule, and an absent
/// map must compare equal to an empty one — `tm mcp add` omits `env` entirely
/// when there is none, while a hand-written `.mcp.json` may spell `{}`.
/// What: an empty map for an absent or null value; `None` for a non-object or
/// for any non-string value inside it.
/// Test: `an_absent_env_equals_an_empty_one`, `a_differing_env_value_is_unknown`.
fn string_map(value: Option<&Value>) -> Option<BTreeMap<String, String>> {
    match value {
        None | Some(Value::Null) => Some(BTreeMap::new()),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
            .collect(),
        Some(_) => None,
    }
}

/// Resolve a command spelling to the executable it names.
///
/// Why: see [`Command`] — two spellings of one binary must compare equal, and a
/// spelling that cannot be resolved must not compare equal to one that can.
/// What: `which` (which handles a bare name via `PATH` and a relative or
/// absolute path directly), then `canonicalize` to collapse symlinks; the raw
/// spelling when either step fails.
/// Test: `identical_spellings_match_when_resolution_fails`.
fn resolve_command(spelling: &str) -> Command {
    match which::which(spelling) {
        Ok(path) => Command::Resolved(path.canonicalize().unwrap_or(path)),
        Err(_) => Command::Unresolved(spelling.to_owned()),
    }
}

#[cfg(test)]
#[path = "mcp_content_trust_tests.rs"]
mod tests;
