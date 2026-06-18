//! Harness-manifest schema (HR-2 / DOC-17).
//!
//! Why: today `prepare_session` provisions a harness from compile-time-bundled
//! assets alone — there is no declarative description of *what* a harness gets.
//! HR-2 makes provisioning manifest-driven: a single TOML document describes the
//! agent set, skills, instruction layers, output style, MCP servers, and model
//! tiers a harness receives, so the runner can resolve it from a configurable
//! catalog instead of hard-coding the set in Rust. TOML is chosen to match the
//! existing `config.toml` convention (`crate::core::config`).
//! What: [`HarnessManifest`] is the top-level deserialization target. Every field
//! is optional so a higher-precedence layer can override only the keys it cares
//! about (see [`HarnessManifest::merge`]); the compiled-in default
//! ([`super::default::default_manifest`]) supplies the floor. A `version` field
//! makes the schema forward-compatible so HR-3 can hash/compare manifests.
//! Test: `manifest_roundtrip`, `manifest_partial_parse`,
//! `manifest_merge_overrides`, `selection_matches` in this module's tests.

use serde::{Deserialize, Serialize};

/// The manifest schema version this build understands.
///
/// Why: HR-3 will hash and compare manifests to detect stale catalog content; a
/// version field lets a future build reject or migrate an incompatible manifest
/// instead of silently mis-parsing it.
/// What: the integer written into a default manifest's `version` field and
/// compared on load.
/// Test: `default_manifest_carries_current_version`.
pub const MANIFEST_VERSION: u32 = 1;

/// A declarative description of everything a provisioned harness receives.
///
/// Why: a single value the resolver materializes (project override beats user
/// config beats catalog manifest beats compiled-in default) and `prepare_session`
/// consumes, replacing the implicit "deploy whatever is bundled" behavior with an
/// explicit, overridable contract.
/// What: optional sections for the agent set, skills, instruction layers, the
/// output style id, MCP servers, and model tiers. Absent sections mean "inherit
/// from the lower-precedence layer"; the compiled-in default fills any that are
/// still absent after all layers merge. `Default` yields an all-`None` manifest
/// used as the merge identity.
/// Test: `manifest_roundtrip`, `manifest_partial_parse`, `manifest_merge_overrides`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HarnessManifest {
    /// Schema version. Absent in a partial override layer; the compiled-in
    /// default always sets it to [`MANIFEST_VERSION`].
    pub version: Option<u32>,

    /// `[agents]` — which agents the harness deploys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<AgentSet>,

    /// `[skills]` — which skills the harness deploys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<SkillSet>,

    /// `[instructions]` — instruction layers folded into the launch prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<InstructionLayers>,

    /// `[style]` — the active output-style id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<StyleSelection>,

    /// `[mcp]` — which MCP servers the harness injects into `.mcp.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpServers>,

    /// `[models]` — model-tier defaults for the harness's agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<ModelTiers>,
}

/// `[agents]` — the agent set a harness deploys.
///
/// Why: a harness may want only a subset of the bundled/catalog agents (e.g. a
/// Rust-only project drops the JS engineers). Include/exclude lists express that
/// without enumerating every agent.
/// What: `include` is a list of agent names or glob patterns (`*` wildcard); an
/// empty or absent `include` means "all available agents". `exclude` removes
/// names/patterns from the included set and always wins over `include`. `source`
/// selects where the agent *source files* come from (`bundled` or `catalog`).
/// Test: `selection_matches`, `agent_set_source_default_is_bundled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentSet {
    /// Agent names or glob patterns to deploy. Empty → all available.
    #[serde(default)]
    pub include: Vec<String>,
    /// Agent names or glob patterns to drop. Wins over `include`.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Where the agent source files are read from.
    #[serde(default)]
    pub source: ContentSource,
}

/// `[skills]` — the skill set a harness deploys.
///
/// Why: like agents, a harness may enable only some skills. Skills carry no
/// inheritance so the selection is a plain include/exclude filter.
/// What: `include`/`exclude` are name-or-glob lists with the same semantics as
/// [`AgentSet`]; `source` selects the source directory (`bundled` or `catalog`).
/// Test: `selection_matches`, `skill_set_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillSet {
    /// Skill names or glob patterns to deploy. Empty → all available.
    #[serde(default)]
    pub include: Vec<String>,
    /// Skill names or glob patterns to drop. Wins over `include`.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Where the skill source files are read from.
    #[serde(default)]
    pub source: ContentSource,
}

/// Where a harness reads agent/skill *source* content from.
///
/// Why: HR-2 must let a manifest point provisioning at either the compile-time
/// bundled assets (the default, regression-safe path) or the synced catalog
/// (`~/.trusty-mpm/catalog/repo/.claude/…`). Encoding the choice in the manifest
/// keeps `prepare_session` from guessing.
/// What: `Bundled` (the framework's own source dirs) or `Catalog` (the
/// CatalogSync checkout). `Default` is `Bundled` so an absent value reproduces
/// today's behavior.
/// Test: `agent_set_source_default_is_bundled`, `content_source_roundtrip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContentSource {
    /// Compile-time bundled framework assets (the default).
    #[default]
    Bundled,
    /// The CatalogSync checkout under `~/.trusty-mpm/catalog/`.
    Catalog,
}

/// `[instructions]` — the instruction layers folded into the launch prompt.
///
/// Why: DOC-17 names "instruction layers (system, contextual, domain-specific)"
/// as part of what a harness gets. Today the layers are fixed in code; the
/// manifest records which optional layers are enabled so the set is declarative
/// and forward-compatible (HR-3 hashes it).
/// What: boolean toggles for the optional layers. The non-overridable PM floor
/// (`BASE_PM`) is always applied regardless of these flags — they only gate the
/// *optional* contextual/domain layers. Absent flags inherit from the lower
/// layer; the default enables the standard set.
/// Test: `instruction_layers_roundtrip`, `manifest_merge_overrides`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InstructionLayers {
    /// Include the framework system instructions layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<bool>,
    /// Include the contextual (project `CLAUDE.md`) layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contextual: Option<bool>,
    /// Include the domain-specific delegation-authority layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<bool>,
}

impl InstructionLayers {
    /// Field-by-field merge: each `Some` toggle in `higher` wins, `None` inherits.
    ///
    /// Why: `[instructions]` is a struct of independent toggles; whole-section
    /// replacement would let a partial higher-layer override (e.g. only
    /// `domain = false`) silently reset the other layers to `None`. Merging per
    /// field preserves the lower layer's toggles the higher layer did not mention.
    /// What: for each of `system`/`contextual`/`domain`, takes `higher`'s value
    /// when it is `Some`, else falls through to `self`'s value.
    /// Test: `instruction_layers_field_merge`.
    #[must_use]
    pub fn merge(self, higher: InstructionLayers) -> InstructionLayers {
        InstructionLayers {
            system: higher.system.or(self.system),
            contextual: higher.contextual.or(self.contextual),
            domain: higher.domain.or(self.domain),
        }
    }
}

/// `[style]` — the active output-style selection.
///
/// Why: DOC-17 lists the output style (professional/teaching/research) as a
/// harness-level choice. A manifest can set the default style; an explicit
/// `--style` flag still overrides it at launch (HR-4 precedence is preserved).
/// What: an optional active style id matching a bundled style
/// (`crate::core::bundle::OUTPUT_STYLES`). Absent → inherit; the default sets the
/// professional id.
/// Test: `style_selection_roundtrip`, `manifest_merge_overrides`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StyleSelection {
    /// The active output-style id (e.g. `"trusty-mpm-teacher"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
}

/// `[mcp]` — which MCP servers the harness injects.
///
/// Why: DOC-17 lists "MCP servers (static and dynamic)" as part of provisioning.
/// Today `prepare_session` unconditionally injects `trusty-memory` and
/// `trusty-search`; the manifest makes those toggleable so a harness can opt out.
/// What: boolean toggles for the two built-in servers. Absent → inherit; the
/// default enables both, reproducing today's behavior.
/// Test: `mcp_servers_roundtrip`, `default_manifest_enables_both_mcp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct McpServers {
    /// Inject the `trusty-memory` MCP server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusty_memory: Option<bool>,
    /// Inject the `trusty-search` MCP server (pinned to the project index).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusty_search: Option<bool>,
}

impl McpServers {
    /// Field-by-field merge: each `Some` toggle in `higher` wins, `None` inherits.
    ///
    /// Why: `[mcp]` is a struct of independent toggles. The old whole-section
    /// replacement meant a partial override like `[mcp] trusty_search = false`
    /// reset `trusty_memory` back to `None` (losing a lower layer's explicit
    /// `true`). Merging per field keeps the unmentioned toggle's lower-layer value.
    /// What: for each of `trusty_memory`/`trusty_search`, takes `higher`'s value
    /// when it is `Some`, else falls through to `self`'s value.
    /// Test: `manifest_merge_mcp_field_level`.
    #[must_use]
    pub fn merge(self, higher: McpServers) -> McpServers {
        McpServers {
            trusty_memory: higher.trusty_memory.or(self.trusty_memory),
            trusty_search: higher.trusty_search.or(self.trusty_search),
        }
    }
}

/// `[models]` — model-tier defaults for the harness.
///
/// Why: DOC-17 lists "model tiers/overrides" as part of a manifest. HR-1 already
/// derives a model from each agent's `resource_tier` at deploy time; this section
/// lets a manifest pin the canonical model id for each tier so a harness can, for
/// example, route `intensive` to a specific Opus build.
/// What: an optional model id (or alias) per tier. Absent → inherit; the default
/// leaves all `None` so HR-1's built-in tier mapping applies.
/// Test: `model_tiers_roundtrip`, `manifest_merge_overrides`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelTiers {
    /// Model id/alias for the `lightweight` tier (HR-1: haiku).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lightweight: Option<String>,
    /// Model id/alias for the `standard` tier (HR-1: sonnet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standard: Option<String>,
    /// Model id/alias for the `high` tier (HR-1: sonnet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<String>,
    /// Model id/alias for the `intensive` tier (HR-1: opus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensive: Option<String>,
}

impl HarnessManifest {
    /// Parse a manifest from a TOML string.
    ///
    /// Why: every layer (project override, user config, catalog) is a TOML file;
    /// a single typed parse entry point keeps the resolver simple.
    /// What: delegates to `toml::from_str`. A partial document (only some
    /// sections present) parses cleanly because every field is optional.
    /// Test: `manifest_partial_parse`, `manifest_roundtrip`.
    pub fn from_toml(raw: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(raw)
    }

    /// Serialize this manifest to a TOML string.
    ///
    /// Why: round-trip support lets the resolver (and future HR-3 hashing) emit a
    /// canonical manifest, and makes the schema testable.
    /// What: delegates to `toml::to_string`.
    /// Test: `manifest_roundtrip`.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string(self)
    }

    /// Overlay `higher` onto `self`, returning the merged manifest.
    ///
    /// Why: precedence resolution is layer-by-layer — a higher-precedence layer
    /// overrides only the keys it sets, leaving the rest of `self` intact. This
    /// is the core of the NORMATIVE precedence
    /// (project override > user config > catalog > default).
    /// What: each section merges according to its documented mode:
    ///
    /// - **Field-by-field** (`[mcp]`, `[instructions]`): these are small structs
    ///   of independent `Option<bool>` toggles, so a higher layer's `Some` wins
    ///   *per field* while a `None` field inherits the lower layer. This means a
    ///   partial override like `[mcp] trusty_search = false` no longer resets the
    ///   other toggle (e.g. `trusty_memory`) back to `None` (see [`McpServers::merge`],
    ///   [`InstructionLayers::merge`]).
    /// - **Whole-section replacement** (`[agents]`, `[skills]`, `[style]`,
    ///   `[models]`): a `Some` section in `higher` replaces the whole section in
    ///   `self`. This is intentional for the list-like include/exclude sections,
    ///   where a higher layer states a complete agent/skill set rather than
    ///   field-merging two lists; `[style]`/`[models]` follow the same simple rule.
    ///
    /// `version` is a scalar: `higher` wins when set.
    /// Test: `manifest_merge_overrides`, `manifest_merge_keeps_lower_when_absent`,
    /// `manifest_merge_mcp_field_level`.
    #[must_use]
    pub fn merge(self, higher: HarnessManifest) -> HarnessManifest {
        HarnessManifest {
            version: higher.version.or(self.version),
            // Whole-section replacement (list-like / scalar sections).
            agents: higher.agents.or(self.agents),
            skills: higher.skills.or(self.skills),
            style: higher.style.or(self.style),
            models: higher.models.or(self.models),
            // Field-by-field merge (independent toggle structs).
            instructions: merge_optional(self.instructions, higher.instructions, |lo, hi| {
                lo.merge(hi)
            }),
            mcp: merge_optional(self.mcp, higher.mcp, |lo, hi| lo.merge(hi)),
        }
    }
}

/// Merge two `Option<T>` sections, folding field-by-field when both are present.
///
/// Why: the field-level sections (`[mcp]`, `[instructions]`) need "higher wins
/// per field, lower inherited" semantics, but only when BOTH layers set the
/// section; when only one is present that one wins wholesale. Factoring the
/// `None`-handling here keeps [`HarnessManifest::merge`] readable.
/// What: returns `higher.merge(lower)` when both are `Some`; otherwise the single
/// present value (or `None`). `f` performs the per-field fold as `f(lower, higher)`.
/// Test: covered via `manifest_merge_mcp_field_level` and
/// `instruction_layers_field_merge`.
fn merge_optional<T>(lower: Option<T>, higher: Option<T>, f: impl FnOnce(T, T) -> T) -> Option<T> {
    match (lower, higher) {
        (Some(lo), Some(hi)) => Some(f(lo, hi)),
        (lo, hi) => hi.or(lo),
    }
}

/// Whether `name` is selected by an include/exclude filter.
///
/// Why: both [`AgentSet`] and [`SkillSet`] share the same include/exclude/glob
/// semantics; factoring the rule here keeps the two consumers consistent and
/// independently testable.
/// What: returns `true` when `name` matches the `include` set (an empty include
/// list means "match all") AND does not match the `exclude` set. Matching is by
/// exact name or a trailing/leading `*` glob via [`glob_matches`].
/// Test: `selection_matches`.
pub fn selection_matches(name: &str, include: &[String], exclude: &[String]) -> bool {
    let included = include.is_empty() || include.iter().any(|p| glob_matches(p, name));
    let excluded = exclude.iter().any(|p| glob_matches(p, name));
    included && !excluded
}

/// Match `name` against a simple glob `pattern` supporting any number of `*`.
///
/// Why: manifests express agent/skill sets compactly (`"*-engineer"`,
/// `"rust-*"`, `"*-engineer-*"`); a tiny matcher avoids pulling in a glob crate
/// for the one wildcard. The earlier `split_once('*')` implementation only split
/// on the FIRST `*`, so a multi-`*` pattern (`"*-engineer-*"`, `"a*b*c"`) was
/// mis-parsed — the leftover `*` was treated as a literal in the suffix, silently
/// producing wrong matches instead of the documented behavior. This version
/// handles an arbitrary number of `*`.
/// What: splits `pattern` on every `*` into literal segments and matches them in
/// order against `name`. A leading `*` (empty first segment) means "no prefix
/// anchor"; a trailing `*` (empty last segment) means "no suffix anchor". With no
/// `*` the pattern is an exact equality check; a bare `*` matches everything.
/// Interior segments must appear in order, left to right, without overlapping.
/// Test: `glob_matching`.
fn glob_matches(pattern: &str, name: &str) -> bool {
    // Fast paths: no wildcard → exact equality; a bare `*` → match all.
    if !pattern.contains('*') {
        return pattern == name;
    }
    if pattern == "*" {
        return true;
    }

    // Split on every `*`. `segments` are the literal runs between wildcards;
    // empty leading/trailing segments encode "no anchor" on that side.
    let segments: Vec<&str> = pattern.split('*').collect();
    let anchored_start = !segments.first().is_none_or(|s| s.is_empty());
    let anchored_end = !segments.last().is_none_or(|s| s.is_empty());

    // Walk the non-empty segments left-to-right, consuming `name` as we match.
    let mut rest = name;
    let non_empty: Vec<&str> = segments.iter().copied().filter(|s| !s.is_empty()).collect();
    for (idx, seg) in non_empty.iter().enumerate() {
        let is_first = idx == 0;
        let is_last = idx == non_empty.len() - 1;

        if is_first && anchored_start {
            // The first segment must be a prefix of `name`.
            match rest.strip_prefix(seg) {
                Some(tail) => rest = tail,
                None => return false,
            }
        } else if is_last && anchored_end {
            // The final segment must be a suffix of whatever remains.
            return rest.ends_with(seg);
        } else {
            // A floating interior (or unanchored) segment: find its next
            // occurrence and advance past it.
            match rest.find(seg) {
                Some(pos) => rest = &rest[pos + seg.len()..],
                None => return false,
            }
        }
    }

    // All segments matched. If the end was anchored we already returned on the
    // last segment; reaching here means the end was unanchored (trailing `*`),
    // which matches any remaining tail.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip() {
        // A fully-populated manifest must survive a serialize → parse round-trip
        // unchanged, proving the schema is self-consistent.
        let manifest = HarnessManifest {
            version: Some(MANIFEST_VERSION),
            agents: Some(AgentSet {
                include: vec!["rust-engineer".into(), "qa".into()],
                exclude: vec!["*-ops".into()],
                source: ContentSource::Catalog,
            }),
            skills: Some(SkillSet {
                include: vec!["example-skill".into()],
                exclude: vec![],
                source: ContentSource::Bundled,
            }),
            instructions: Some(InstructionLayers {
                system: Some(true),
                contextual: Some(true),
                domain: Some(false),
            }),
            style: Some(StyleSelection {
                active: Some("trusty-mpm-teacher".into()),
            }),
            mcp: Some(McpServers {
                trusty_memory: Some(true),
                trusty_search: Some(false),
            }),
            models: Some(ModelTiers {
                lightweight: Some("haiku".into()),
                standard: None,
                high: None,
                intensive: Some("opus".into()),
            }),
        };

        let toml = manifest.to_toml().expect("serialize");
        let parsed = HarnessManifest::from_toml(&toml).expect("parse");
        assert_eq!(manifest, parsed);
    }

    #[test]
    fn manifest_partial_parse() {
        // A manifest that sets only one section must parse, with every other
        // section left `None` so the merge keeps the lower layer's values.
        let toml = r#"
version = 1

[style]
active = "trusty-mpm-research"
"#;
        let parsed = HarnessManifest::from_toml(toml).expect("partial parse");
        assert_eq!(parsed.version, Some(1));
        assert_eq!(
            parsed.style.as_ref().and_then(|s| s.active.as_deref()),
            Some("trusty-mpm-research")
        );
        assert!(parsed.agents.is_none());
        assert!(parsed.skills.is_none());
        assert!(parsed.mcp.is_none());
    }

    #[test]
    fn manifest_merge_overrides() {
        // A higher-precedence layer must replace only the sections it sets and
        // leave the rest of the lower layer intact.
        let lower = HarnessManifest {
            version: Some(1),
            agents: Some(AgentSet {
                include: vec!["a".into()],
                ..AgentSet::default()
            }),
            style: Some(StyleSelection {
                active: Some("trusty-mpm".into()),
            }),
            mcp: Some(McpServers {
                trusty_memory: Some(true),
                trusty_search: Some(true),
            }),
            ..HarnessManifest::default()
        };
        let higher = HarnessManifest {
            style: Some(StyleSelection {
                active: Some("trusty-mpm-teacher".into()),
            }),
            ..HarnessManifest::default()
        };

        let merged = lower.merge(higher);
        // style replaced by the higher layer
        assert_eq!(
            merged.style.as_ref().and_then(|s| s.active.as_deref()),
            Some("trusty-mpm-teacher")
        );
        // agents + mcp kept from the lower layer (higher left them None)
        assert_eq!(
            merged.agents.as_ref().map(|a| a.include.clone()),
            Some(vec!["a".to_string()])
        );
        assert_eq!(
            merged.mcp.as_ref().and_then(|m| m.trusty_search),
            Some(true)
        );
    }

    #[test]
    fn manifest_merge_keeps_lower_when_absent() {
        // Merging an all-None higher layer is the identity on the lower layer.
        let lower = HarnessManifest {
            version: Some(1),
            style: Some(StyleSelection {
                active: Some("trusty-mpm".into()),
            }),
            ..HarnessManifest::default()
        };
        let merged = lower.clone().merge(HarnessManifest::default());
        assert_eq!(lower, merged);
    }

    #[test]
    fn manifest_merge_mcp_field_level() {
        // A partial `[mcp]` override (only trusty_search) must NOT discard the
        // lower layer's other toggle (trusty_memory) — field-level merge.
        let lower = HarnessManifest {
            mcp: Some(McpServers {
                trusty_memory: Some(true),
                trusty_search: Some(true),
            }),
            ..HarnessManifest::default()
        };
        let higher = HarnessManifest {
            mcp: Some(McpServers {
                trusty_memory: None,
                trusty_search: Some(false),
            }),
            ..HarnessManifest::default()
        };

        let merged = lower.merge(higher);
        let mcp = merged.mcp.expect("mcp section present");
        assert_eq!(
            mcp.trusty_memory,
            Some(true),
            "the unmentioned toggle must survive the partial override"
        );
        assert_eq!(
            mcp.trusty_search,
            Some(false),
            "the overridden toggle must take the higher layer's value"
        );
    }

    #[test]
    fn instruction_layers_field_merge() {
        // A partial `[instructions]` override must merge field-by-field too.
        let lower = HarnessManifest {
            instructions: Some(InstructionLayers {
                system: Some(true),
                contextual: Some(true),
                domain: Some(true),
            }),
            ..HarnessManifest::default()
        };
        let higher = HarnessManifest {
            instructions: Some(InstructionLayers {
                system: None,
                contextual: None,
                domain: Some(false),
            }),
            ..HarnessManifest::default()
        };

        let layers = lower.merge(higher).instructions.expect("layers present");
        assert_eq!(layers.system, Some(true), "system inherited from lower");
        assert_eq!(
            layers.contextual,
            Some(true),
            "contextual inherited from lower"
        );
        assert_eq!(layers.domain, Some(false), "domain overridden by higher");
    }

    #[test]
    fn selection_matches() {
        // Empty include → match all; exclude wins over include.
        assert!(super::selection_matches("anything", &[], &[]));
        // Exact include.
        let inc = vec!["rust-engineer".to_string()];
        assert!(super::selection_matches("rust-engineer", &inc, &[]));
        assert!(!super::selection_matches("qa", &inc, &[]));
        // Glob include.
        let glob_inc = vec!["*-engineer".to_string()];
        assert!(super::selection_matches("rust-engineer", &glob_inc, &[]));
        assert!(!super::selection_matches("rust-ops", &glob_inc, &[]));
        // Multi-`*` glob include (regression: was mis-matched by split_once).
        let multi_inc = vec!["*-engineer-*".to_string()];
        assert!(super::selection_matches(
            "rust-engineer-ops",
            &multi_inc,
            &[]
        ));
        assert!(!super::selection_matches("rust-engineer", &multi_inc, &[]));
        // Exclude wins.
        let exc = vec!["rust-engineer".to_string()];
        assert!(!super::selection_matches("rust-engineer", &[], &exc));
        let inc_and_exc = ["rust-engineer".to_string()];
        assert!(!super::selection_matches(
            "rust-engineer",
            &inc_and_exc,
            &exc
        ));
    }

    #[test]
    fn glob_matching() {
        // Bare `*` matches everything.
        assert!(super::glob_matches("*", "anything"));
        assert!(super::glob_matches("*", ""));

        // Exact (no wildcard) is equality.
        assert!(super::glob_matches("rust-engineer", "rust-engineer"));
        assert!(!super::glob_matches("rust-engineer", "qa"));
        assert!(!super::glob_matches("rust-engineer", "rust-engineer-2"));

        // Trailing `*` = prefix; leading `*` = suffix.
        assert!(super::glob_matches("*-engineer", "rust-engineer"));
        assert!(!super::glob_matches("*-engineer", "rust-engineer-ops"));
        assert!(super::glob_matches("engineer-*", "engineer-rust"));
        assert!(!super::glob_matches("engineer-*", "rust-engineer"));
        assert!(super::glob_matches("rust-*", "rust-engineer"));

        // Single interior `*` = prefix + suffix.
        assert!(super::glob_matches("a*b", "axxb"));
        assert!(super::glob_matches("a*b", "ab"), "interior * matches empty");
        assert!(!super::glob_matches("a*b", "axx"));

        // Multi-`*` patterns: the previously-broken cases.
        // `*-engineer-*` = "contains -engineer-".
        assert!(super::glob_matches("*-engineer-*", "rust-engineer-ops"));
        assert!(super::glob_matches("*-engineer-*", "x-engineer-y"));
        assert!(
            !super::glob_matches("*-engineer-*", "rust-engineer"),
            "no trailing segment after -engineer- → no match"
        );
        // `a*b*c` = a-prefix, then b, then c-suffix in order.
        assert!(super::glob_matches("a*b*c", "aXbYc"));
        assert!(super::glob_matches("a*b*c", "abc"));
        assert!(
            !super::glob_matches("a*b*c", "acb"),
            "segments must appear in order"
        );
        assert!(
            !super::glob_matches("a*b*c", "aXbY"),
            "missing the trailing c suffix"
        );
    }

    #[test]
    fn content_source_roundtrip() {
        // ContentSource serializes lowercase and round-trips.
        let serialized = toml::to_string(&AgentSet {
            source: ContentSource::Catalog,
            ..AgentSet::default()
        })
        .unwrap();
        assert!(serialized.contains("catalog"));
    }

    #[test]
    fn agent_set_source_default_is_bundled() {
        // An [agents] section that omits `source` must default to Bundled, so
        // the default manifest reproduces today's bundled-asset behavior.
        let toml = "[agents]\ninclude = []\n";
        let parsed = HarnessManifest::from_toml(toml).unwrap();
        assert_eq!(parsed.agents.unwrap().source, ContentSource::Bundled);
    }
}
