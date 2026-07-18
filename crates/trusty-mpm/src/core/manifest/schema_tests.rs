//! Unit tests for the harness-manifest schema (HR-2 / DOC-17), split out of
//! `schema.rs` (a 500-SLOC-capped production file) so coverage can grow under
//! the 1500-SLOC test cap — mirrors the `native_mcp_tests.rs` split.
//!
//! Test: this file IS the test module.

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
            ignore_staleness: vec!["engineer".into()],
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
            custom: BTreeMap::from([(
                "duetto-memory".to_string(),
                CustomMcpServer::Http {
                    url: "https://mcp.example/memory".to_string(),
                    headers: BTreeMap::new(),
                },
            )]),
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
            ..McpServers::default()
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
            ..McpServers::default()
        }),
        ..HarnessManifest::default()
    };
    let higher = HarnessManifest {
        mcp: Some(McpServers {
            trusty_memory: None,
            trusty_search: Some(false),
            ..McpServers::default()
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

#[test]
fn custom_mcp_server_stdio_roundtrip() {
    // A project-scope `[mcp.custom.<name>]` stdio table must round-trip and
    // parse from the exact TOML shape an operator would hand-write.
    let toml = r#"
[mcp.custom.my-local-tool]
type = "stdio"
command = "my-tool"
args = ["serve"]

[mcp.custom.my-local-tool.env]
API_KEY = "secret"
"#;
    let parsed = HarnessManifest::from_toml(toml).expect("parse");
    let custom = &parsed.mcp.expect("mcp section present").custom;
    let entry = custom.get("my-local-tool").expect("entry present");
    match entry {
        CustomMcpServer::Stdio { command, args, env } => {
            assert_eq!(command, "my-tool");
            assert_eq!(args, &vec!["serve".to_string()]);
            assert_eq!(env.get("API_KEY").map(String::as_str), Some("secret"));
        }
        other => panic!("expected Stdio, got {other:?}"),
    }
}

#[test]
fn custom_mcp_server_remote_roundtrip() {
    // The http/sse shapes must round-trip too, matching the acceptance
    // example (a clean URL, no headers).
    let toml = r#"
[mcp.custom.duetto-memory]
type = "http"
url = "https://mcp-services.dev.duettosystems.com/memory/mcp"
"#;
    let parsed = HarnessManifest::from_toml(toml).expect("parse");
    let custom = parsed.mcp.expect("mcp section present").custom;
    match custom.get("duetto-memory").expect("entry present") {
        CustomMcpServer::Http { url, headers } => {
            assert_eq!(url, "https://mcp-services.dev.duettosystems.com/memory/mcp");
            assert!(headers.is_empty());
        }
        other => panic!("expected Http, got {other:?}"),
    }

    // Full struct round-trip (serialize → parse) for both remote variants.
    let manifest = HarnessManifest {
        mcp: Some(McpServers {
            custom: BTreeMap::from([
                (
                    "a".to_string(),
                    CustomMcpServer::Http {
                        url: "https://a.example/mcp".to_string(),
                        headers: BTreeMap::new(),
                    },
                ),
                (
                    "b".to_string(),
                    CustomMcpServer::Sse {
                        url: "https://b.example/sse".to_string(),
                        headers: BTreeMap::new(),
                    },
                ),
            ]),
            ..McpServers::default()
        }),
        ..HarnessManifest::default()
    };
    let toml = manifest.to_toml().expect("serialize");
    let parsed = HarnessManifest::from_toml(&toml).expect("parse");
    assert_eq!(manifest, parsed);
}

#[test]
fn mcp_servers_merge_unions_custom() {
    // Two layers each declaring a DIFFERENT custom server must both survive
    // the merge (union by name); a SHARED name must take the higher layer's
    // definition (project-overrides-user precedent extended to multi-layer
    // manifest merging too).
    let lower = McpServers {
        custom: BTreeMap::from([
            (
                "shared".to_string(),
                CustomMcpServer::Stdio {
                    command: "lower-cmd".to_string(),
                    args: vec![],
                    env: BTreeMap::new(),
                },
            ),
            (
                "lower-only".to_string(),
                CustomMcpServer::Stdio {
                    command: "lower-only-cmd".to_string(),
                    args: vec![],
                    env: BTreeMap::new(),
                },
            ),
        ]),
        ..McpServers::default()
    };
    let higher = McpServers {
        custom: BTreeMap::from([(
            "shared".to_string(),
            CustomMcpServer::Stdio {
                command: "higher-cmd".to_string(),
                args: vec![],
                env: BTreeMap::new(),
            },
        )]),
        ..McpServers::default()
    };

    let merged = lower.merge(higher);
    assert_eq!(merged.custom.len(), 2, "both names survive the union");
    match &merged.custom["shared"] {
        CustomMcpServer::Stdio { command, .. } => {
            assert_eq!(command, "higher-cmd", "higher layer wins the shared name");
        }
        other => panic!("expected Stdio, got {other:?}"),
    }
    assert!(merged.custom.contains_key("lower-only"));
}
