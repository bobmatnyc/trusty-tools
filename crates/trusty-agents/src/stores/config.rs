//! Declarative OKG store-binding shapes (#3816/#3864, DOC-54 SPEC-AGENTS-04
//! §5.1).
//!
//! Why: `agent.toml` needs to say WHICH knowledge the agent owns, in a form
//! the harness can resolve against the live trusty-search / trusty-memory
//! daemons at boot. Two spellings are accepted because two already exist in
//! the project: the product spec (`docs/specs/trusty-agents-product-spec.md`
//! §5.1) documents the shorthand `[stores] allow = ["izzie-personal-kb"]`,
//! while the binding model actually needs an index id and an optional memory
//! palace per store. Rejecting either spelling would have meant a checked-in
//! spec that no parser honours, so [`StoresConfig`] accepts BOTH and
//! normalises to one internal shape.
//! What: [`AgentStoreBinding`] is one store — `name` plus optional `tree`
//! (an `okg://…` URI), `index` (trusty-search index id) and `palace`
//! (trusty-memory palace id). Omitted `tree`/`index` are DERIVED
//! ([`AgentStoreBinding::resolved_tree`] / [`resolved_index`]) rather than
//! rejected, which is what makes the `[stores] allow = [...]` shorthand work.
//! [`StoresConfig::validate`] returns non-fatal problems as strings — a bad
//! binding is surfaced as a disconnected store with a reason, never a parse
//! error that would stop the agent booting.
//! Test: the `mod tests` at the bottom of this file.
//!
//! [`resolved_index`]: AgentStoreBinding::resolved_index

use serde::{Deserialize, Serialize};

/// The URI scheme every OKG knowledge tree is addressed by.
///
/// Why: `okg://<agent>` is the tree identity the GUI renders and the
/// md-KG builder writes under; pinning the scheme in one constant keeps the
/// derivation ([`AgentStoreBinding::resolved_tree`]) and the validation
/// ([`StoresConfig::validate`]) from drifting apart.
pub const OKG_SCHEME: &str = "okg://";

/// One OKG store bound to an agent (`agent.toml` `[[stores]]`).
///
/// Why: Bob's 2026-07-24 decision is ONE store per agent — the tree the
/// agent owns, plus the search index built over it. `palace` is optional
/// and separate because a memory palace is structured recall (facts,
/// decisions), not the document corpus: `cto-assistant` binds the `cto`
/// palace alongside the `cto-assistant` index, while a store that is purely
/// a document tree binds none.
/// What: `name` is the store's stable id (required, the only required
/// field). `tree` defaults to `okg://<agent-name>` and `index` defaults to
/// the store `name` — see [`Self::resolved_tree`] / [`Self::resolved_index`]
/// — so the spec's name-only shorthand expands to a complete binding.
/// Test: `store_binding_parses_full_form`,
/// `store_binding_derives_tree_and_index_when_omitted`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentStoreBinding {
    pub name: String,
    /// The OKG knowledge-tree URI (`okg://<agent-name>`). Absent = derived.
    #[serde(default)]
    pub tree: Option<String>,
    /// The trusty-search index id built over the tree. Absent = `name`.
    #[serde(default)]
    pub index: Option<String>,
    /// The trusty-memory palace holding this agent's structured recall.
    /// Absent = the agent binds no palace (a document-only store).
    #[serde(default)]
    pub palace: Option<String>,
}

impl AgentStoreBinding {
    /// The knowledge-tree URI for this binding, deriving `okg://<agent_name>`
    /// when the TOML omitted `tree`.
    ///
    /// Why: The name-only shorthand must still produce the tree URI the GUI
    /// renders and the md-KG builder writes under; deriving it here (rather
    /// than at each call site) means the API response, the GUI card, and any
    /// future KG writer all agree on one value.
    /// What: The declared `tree` verbatim when present, else
    /// `okg://<agent_name>`.
    /// Test: `store_binding_derives_tree_and_index_when_omitted`.
    pub fn resolved_tree(&self, agent_name: &str) -> String {
        self.tree
            .clone()
            .unwrap_or_else(|| format!("{OKG_SCHEME}{agent_name}"))
    }

    /// The trusty-search index id for this binding, defaulting to the store
    /// `name`.
    ///
    /// Why: A store and the index over it are usually named the same thing
    /// (`bob-kb`, `cto-assistant`); requiring both to be typed out would make
    /// the shorthand spelling unusable.
    /// What: The declared `index` verbatim when present, else `name`.
    /// Test: `store_binding_derives_tree_and_index_when_omitted`.
    pub fn resolved_index(&self) -> &str {
        self.index.as_deref().unwrap_or(&self.name)
    }

    /// Non-fatal problems with this binding, as human-readable reasons.
    ///
    /// Why: A hand-edited `agent.toml` with a typo'd tree scheme must not
    /// take the agent down — the harness reports the store as disconnected
    /// and boots anyway (see this module's doc comment). Returning strings
    /// (not an error type) keeps the reason renderable straight into the GUI
    /// card without a second mapping layer.
    /// What: `Some(reason)` for the first problem found, `None` when the
    /// binding is usable. Checks: non-blank `name`, non-blank `index`, and
    /// an `okg://` scheme on any explicitly declared `tree`.
    /// Test: `validate_flags_blank_name`, `validate_flags_bad_tree_scheme`,
    /// `validate_accepts_good_binding`.
    pub fn validate(&self) -> Option<String> {
        if self.name.trim().is_empty() {
            return Some("store binding has a blank `name`".to_string());
        }
        if self.resolved_index().trim().is_empty() {
            return Some(format!(
                "store `{}` resolves to a blank search index id",
                self.name
            ));
        }
        if let Some(tree) = &self.tree
            && !tree.starts_with(OKG_SCHEME)
        {
            return Some(format!(
                "store `{}` declares tree `{tree}`, which is not an `{OKG_SCHEME}` URI",
                self.name
            ));
        }
        None
    }
}

/// The `stores` field of `agent.toml`, in either accepted spelling.
///
/// Why: See this module's doc comment — the product spec documents
/// `[stores] allow = [...]` while the live binding model needs per-store
/// index/palace fields, so both are accepted and normalised here rather than
/// forcing a spec rewrite or a silently-ignored key.
/// What: Deserializes from EITHER an array-of-tables (`[[stores]]`, the rich
/// form) or a table with an `allow` list of names (`[stores]`, the
/// shorthand), collapsing both into `bindings`. Absent = no bound store,
/// which is a valid state (the agent simply knows nothing of its own).
/// Test: `stores_config_parses_array_of_tables`,
/// `stores_config_parses_allow_shorthand`,
/// `stores_config_defaults_empty`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StoresConfig {
    pub bindings: Vec<AgentStoreBinding>,
}

impl StoresConfig {
    /// The single bound store, per the one-store-per-agent decision.
    ///
    /// Why: Callers that need "the agent's own index" (notably
    /// `vector_search`'s default target, #3864) want one value, not a list.
    /// Extra bindings are a config mistake reported by [`Self::validate`],
    /// not something to silently fan out over.
    /// What: The first binding, or `None` when nothing is bound.
    /// Test: `primary_returns_first_binding`.
    pub fn primary(&self) -> Option<&AgentStoreBinding> {
        self.bindings.first()
    }

    /// The trusty-search index id `vector_search` should search by default.
    ///
    /// Why: This is the #3864 fix's data source — an unqualified
    /// `vector_search(query)` from an agent with a bound store must hit that
    /// agent's OWN knowledge rather than whatever code index happens to be
    /// under the process CWD.
    /// What: The primary binding's [`AgentStoreBinding::resolved_index`], or
    /// `None` when the agent binds no store (leaving the tool on its
    /// pre-existing local-index behaviour).
    /// Test: `default_search_index_uses_primary_binding`,
    /// `default_search_index_none_without_bindings`.
    pub fn default_search_index(&self) -> Option<&str> {
        self.primary().map(AgentStoreBinding::resolved_index)
    }

    /// Non-fatal configuration problems across all bindings.
    ///
    /// Why: Reported, never fatal — see [`AgentStoreBinding::validate`].
    /// What: One reason per problem: each binding's own validation, plus a
    /// warning when more than one store is bound (the spec says exactly one)
    /// and one per duplicated store name.
    /// Test: `validate_flags_multiple_bindings`,
    /// `validate_flags_duplicate_names`.
    pub fn validate(&self) -> Vec<String> {
        let mut issues: Vec<String> = self.bindings.iter().filter_map(|b| b.validate()).collect();
        if self.bindings.len() > 1 {
            issues.push(format!(
                "{} stores bound; the spec allows exactly one OKG store per agent \
                 (only the first is used as the default search target)",
                self.bindings.len()
            ));
        }
        let mut seen: Vec<&str> = Vec::new();
        for b in &self.bindings {
            if seen.contains(&b.name.as_str()) {
                issues.push(format!("duplicate store name `{}`", b.name));
            } else {
                seen.push(&b.name);
            }
        }
        issues
    }
}

/// The two on-disk spellings `StoresConfig` normalises from.
///
/// Why: `#[serde(untagged)]` picks whichever matches the TOML value's actual
/// shape — an array-of-tables deserializes as `Rich`, a table with `allow`
/// as `Shorthand`. Keeping this private means the rest of the crate only
/// ever sees the normalised [`StoresConfig`].
#[derive(Deserialize)]
#[serde(untagged)]
enum StoresConfigRepr {
    Rich(Vec<AgentStoreBinding>),
    Shorthand {
        #[serde(default)]
        allow: Vec<String>,
    },
}

impl<'de> Deserialize<'de> for StoresConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bindings = match StoresConfigRepr::deserialize(deserializer)? {
            StoresConfigRepr::Rich(bindings) => bindings,
            StoresConfigRepr::Shorthand { allow } => allow
                .into_iter()
                .map(|name| AgentStoreBinding {
                    name,
                    ..Default::default()
                })
                .collect(),
        };
        Ok(StoresConfig { bindings })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Wrapper {
        #[serde(default)]
        stores: StoresConfig,
    }

    #[test]
    fn stores_config_parses_array_of_tables() {
        let raw = r#"
[[stores]]
name = "cto-assistant-kb"
tree = "okg://cto-assistant"
index = "cto-assistant"
palace = "cto"
"#;
        let w: Wrapper = toml::from_str(raw).unwrap();
        assert_eq!(w.stores.bindings.len(), 1);
        let b = &w.stores.bindings[0];
        assert_eq!(b.name, "cto-assistant-kb");
        assert_eq!(b.tree.as_deref(), Some("okg://cto-assistant"));
        assert_eq!(b.resolved_index(), "cto-assistant");
        assert_eq!(b.palace.as_deref(), Some("cto"));
        assert!(w.stores.validate().is_empty());
    }

    #[test]
    fn stores_config_parses_allow_shorthand() {
        // The spelling documented in docs/specs/trusty-agents-product-spec.md
        // §5.1 — name only, everything else derived.
        let raw = r#"
[stores]
allow = ["izzie-personal-kb"]
"#;
        let w: Wrapper = toml::from_str(raw).unwrap();
        assert_eq!(w.stores.bindings.len(), 1);
        let b = &w.stores.bindings[0];
        assert_eq!(b.name, "izzie-personal-kb");
        assert_eq!(b.resolved_index(), "izzie-personal-kb");
        assert_eq!(b.resolved_tree("izzie"), "okg://izzie");
        assert!(b.palace.is_none());
    }

    #[test]
    fn stores_config_defaults_empty() {
        let w: Wrapper = toml::from_str("").unwrap();
        assert!(w.stores.bindings.is_empty());
        assert!(w.stores.primary().is_none());
        assert!(w.stores.validate().is_empty());
    }

    #[test]
    fn store_binding_parses_full_form() {
        let raw = r#"
name = "bob-kb"
tree = "okg://izzie"
index = "bob-kb"
palace = "owner-profile"
"#;
        let b: AgentStoreBinding = toml::from_str(raw).unwrap();
        assert_eq!(b.resolved_tree("izzie"), "okg://izzie");
        assert_eq!(b.resolved_index(), "bob-kb");
        assert_eq!(b.palace.as_deref(), Some("owner-profile"));
        assert_eq!(b.validate(), None);
    }

    #[test]
    fn store_binding_derives_tree_and_index_when_omitted() {
        let b: AgentStoreBinding = toml::from_str(r#"name = "bob-kb""#).unwrap();
        assert_eq!(
            b.resolved_tree("izzie"),
            "okg://izzie",
            "tree must derive from the AGENT name, not the store name"
        );
        assert_eq!(b.resolved_index(), "bob-kb");
    }

    #[test]
    fn validate_flags_blank_name() {
        let b: AgentStoreBinding = toml::from_str(r#"name = "  ""#).unwrap();
        assert!(b.validate().unwrap().contains("blank `name`"));
    }

    #[test]
    fn validate_flags_bad_tree_scheme() {
        let b: AgentStoreBinding =
            toml::from_str("name = \"kb\"\ntree = \"https://example.com/kb\"").unwrap();
        let reason = b.validate().unwrap();
        assert!(reason.contains("okg://"), "reason was: {reason}");
    }

    #[test]
    fn validate_accepts_good_binding() {
        let b: AgentStoreBinding =
            toml::from_str("name = \"kb\"\ntree = \"okg://izzie\"\nindex = \"bob-kb\"").unwrap();
        assert_eq!(b.validate(), None);
    }

    #[test]
    fn validate_flags_multiple_bindings() {
        let raw = r#"
[[stores]]
name = "a"

[[stores]]
name = "b"
"#;
        let w: Wrapper = toml::from_str(raw).unwrap();
        let issues = w.stores.validate();
        assert!(
            issues.iter().any(|i| i.contains("exactly one OKG store")),
            "issues were: {issues:?}"
        );
    }

    #[test]
    fn validate_flags_duplicate_names() {
        let raw = r#"
[[stores]]
name = "dup"

[[stores]]
name = "dup"
"#;
        let w: Wrapper = toml::from_str(raw).unwrap();
        let issues = w.stores.validate();
        assert!(
            issues.iter().any(|i| i.contains("duplicate store name")),
            "issues were: {issues:?}"
        );
    }

    #[test]
    fn primary_returns_first_binding() {
        let raw = r#"
[[stores]]
name = "first"

[[stores]]
name = "second"
"#;
        let w: Wrapper = toml::from_str(raw).unwrap();
        assert_eq!(w.stores.primary().unwrap().name, "first");
    }

    #[test]
    fn default_search_index_uses_primary_binding() {
        let raw = r#"
[[stores]]
name = "cto-assistant-kb"
index = "cto-assistant"
"#;
        let w: Wrapper = toml::from_str(raw).unwrap();
        assert_eq!(w.stores.default_search_index(), Some("cto-assistant"));
    }

    #[test]
    fn default_search_index_none_without_bindings() {
        let w: Wrapper = toml::from_str("").unwrap();
        assert_eq!(w.stores.default_search_index(), None);
    }

    #[test]
    fn unknown_keys_do_not_break_parsing() {
        // Forward compat: a future field must degrade to "ignored", never a
        // parse error that stops the agent booting.
        let b: AgentStoreBinding =
            toml::from_str("name = \"kb\"\nfuture_field = \"whatever\"").unwrap();
        assert_eq!(b.name, "kb");
    }
}
