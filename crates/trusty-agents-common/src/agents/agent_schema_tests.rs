//! Tests for the #4448 frontmatter-schema discriminator.
//!
//! The fixtures below are trimmed from REAL files: the trusty-mpm ones from
//! `crates/trusty-mpm/src/assets/agents/` and this crate's composer output, the
//! claude-mpm ones from the 2026-07-31 quarantine backup taken off this
//! machine. The filenames deliberately COLLIDE — that collision is the whole
//! reason a filename-keyed sweep is wrong.

use super::*;
use crate::agents::builder::compose_agent;
use tempfile::TempDir;

/// A composed trusty-mpm agent as `merge_frontmatter` actually emits it —
/// `role:` present, `initialPrompt:` present, nothing foreign.
const TRUSTY_MPM_QA: &str = "---\nname: qa\nrole: qa\ndescription: 'Expert quality assurance engineer.'\nmodel: sonnet\nskills: [test-driven-development, systematic-debugging]\ninitialPrompt: \"Begin verification.\"\n---\n\n# QA\n\nBody.\n";

/// A claude-mpm agent on the SAME name. Verbatim key shape from
/// `.trusty-mpm/quarantine-backup-20260731/tm-writing-01/code-critic.md`:
/// `agent_type:`/`effort:`/`version:`, a BLOCK-style `skills:` list, and an
/// `initialPrompt:` — which trusty-mpm emits too.
const CLAUDE_MPM_QA: &str = "---\nname: qa\ndescription: \"Use this agent when you need comprehensive testing.\"\nmodel: sonnet\neffort: balanced\nagent_type: qa\nversion: \"1.0.0\"\nskills:\n- code-review-standards\n- systematic-debugging\ninitialPrompt: \"Begin verification.\"\n---\n# QA\n\n**Inherits from**: BASE_AGENT.md\n";

#[test]
fn schema_of_a_composed_trusty_mpm_agent() {
    assert_eq!(agent_schema(TRUSTY_MPM_QA), AgentSchema::TrustyMpm);
}

#[test]
fn schema_of_a_claude_mpm_agent() {
    assert_eq!(agent_schema(CLAUDE_MPM_QA), AgentSchema::ClaudeMpm);
}

/// The two fixtures declare the SAME `name:` and would live under the same
/// filename. Only the schema separates them — the point of the whole module.
#[test]
fn colliding_names_are_separated_only_by_schema() {
    assert_ne!(agent_schema(TRUSTY_MPM_QA), agent_schema(CLAUDE_MPM_QA));
}

/// The older claude-mpm generation: `schema_version`/`agent_id`/`author`.
#[test]
fn schema_of_a_legacy_claude_mpm_agent() {
    let doc = "---\nschema_version: \"1.2.0\"\nagent_id: engineer\nname: engineer\nauthor: \"Claude MPM Team\"\nresource_tier: standard\n---\n\nBody.\n";
    assert_eq!(agent_schema(doc), AgentSchema::ClaudeMpm);
}

/// EVERY marker must stand alone. claude-mpm has shipped several frontmatter
/// generations and a file may carry only one of them; dropping any single entry
/// from `CLAUDE_MPM_KEYS` must be visible here, or a whole generation silently
/// reclassifies as `Unrecognized` and the receipt stops naming its real owner.
///
/// The list here is written out LITERALLY rather than iterating
/// `CLAUDE_MPM_KEYS`, which would be self-referential — deleting an entry from
/// the constant would silently delete the assertion that covers it.
#[test]
fn each_claude_mpm_marker_is_recognised_alone() {
    for marker in [
        "agent_type",
        "author",
        "schema_version",
        "agent_id",
        "effort",
        "version",
    ] {
        let doc = format!("---\nname: qa\nrole: qa\n{marker}: something\n---\n\nBody.\n");
        assert_eq!(
            agent_schema(&doc),
            AgentSchema::ClaudeMpm,
            "`{marker}:` alone must identify claude-mpm's schema"
        );
    }
}

/// The two vocabularies must stay disjoint. A key in both lists would make the
/// whitelist accept a foreign file.
#[test]
fn the_two_vocabularies_do_not_overlap() {
    for key in CLAUDE_MPM_KEYS {
        assert!(
            !TRUSTY_MPM_KEYS.contains(key),
            "`{key}` is in BOTH vocabularies; the schema gate would accept a foreign file"
        );
    }
}

/// `initialPrompt:` is NOT a claude-mpm marker — `merge_frontmatter` injects a
/// role-derived one. Treating it as foreign would exempt most of the roster.
#[test]
fn initial_prompt_alone_is_not_foreign() {
    let doc = "---\nname: ops\nrole: ops\ninitialPrompt: \"Begin.\"\n---\n\nBody.\n";
    assert_eq!(agent_schema(doc), AgentSchema::TrustyMpm);
}

/// A hand-authored project agent that declares something outside the composer's
/// vocabulary is NOT trusty-mpm's to move, even on a bundled name.
#[test]
fn schema_of_a_hand_authored_file() {
    let doc = "---\nname: qa\nrole: qa\ncolor: purple\n---\n\nOur own QA agent.\n";
    assert_eq!(agent_schema(doc), AgentSchema::Unrecognized);
}

/// A trusty-mpm SOURCE file still declares `extends:`; the composer strips it,
/// so a file that still carries one was never a deploy artifact.
#[test]
fn schema_of_a_source_file_with_extends() {
    let doc = "---\nname: rust-engineer\nrole: engineer\nextends: base-engineer\n---\n\nBody.\n";
    assert_eq!(agent_schema(doc), AgentSchema::Unrecognized);
}

/// `role:` is required. A file declaring only `name:` is not a composed agent.
#[test]
fn schema_requires_role() {
    let doc = "---\nname: qa\ndescription: 'Something.'\n---\n\nBody.\n";
    assert_eq!(agent_schema(doc), AgentSchema::Unrecognized);
}

/// `name:` is required too — a bodiless fragment is not an agent.
#[test]
fn schema_requires_name() {
    assert_eq!(
        agent_schema("---\nrole: qa\n---\n\nBody.\n"),
        AgentSchema::Unrecognized
    );
}

#[test]
fn schema_without_frontmatter_is_unrecognized() {
    assert_eq!(agent_schema("# Just a note\n"), AgentSchema::Unrecognized);
    assert_eq!(agent_schema(""), AgentSchema::Unrecognized);
}

/// An unterminated block is not a frontmatter block. Accepting one would let a
/// truncated file's first two keys decide a MOVE.
#[test]
fn schema_of_an_unterminated_block_is_unrecognized() {
    let doc = "---\nname: qa\nrole: qa\n\nno closing fence\n";
    assert_eq!(agent_schema(doc), AgentSchema::Unrecognized);
    assert_eq!(frontmatter_keys(doc), None);
}

#[test]
fn keys_are_declaration_ordered() {
    let keys = frontmatter_keys(TRUSTY_MPM_QA).expect("well-formed block");
    assert_eq!(
        keys,
        vec![
            "name",
            "role",
            "description",
            "model",
            "skills",
            "initialprompt"
        ]
    );
}

/// A block-style `skills:` list contributes the `skills` key and nothing else —
/// `- item` lines carry no colon.
#[test]
fn keys_skip_block_list_items() {
    let keys = frontmatter_keys(CLAUDE_MPM_QA).expect("well-formed block");
    assert!(keys.contains(&"skills".to_owned()));
    assert!(!keys.iter().any(|k| k.starts_with('-')));
}

#[test]
fn keys_require_a_leading_fence() {
    assert_eq!(frontmatter_keys("name: qa\nrole: qa\n"), None);
    assert_eq!(frontmatter_keys("\n---\nname: qa\n---\n"), None);
}

#[test]
fn keys_require_a_closing_fence() {
    assert_eq!(frontmatter_keys("---\nname: qa\n"), None);
}

/// THE DRIFT PIN. Compose an agent that declares every field the composer can
/// carry and assert the emitted document still classifies as trusty-mpm's. If
/// `merge_frontmatter` gains an emitted key that `TRUSTY_MPM_KEYS` does not
/// list, every freshly deployed agent starts classifying `Unrecognized` and the
/// quarantine silently stops working — this test fails first instead.
#[test]
fn pinned_to_the_composer_emission() {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::write(
        tmp.path().join("BASE-QA.md"),
        "---\nname: base-qa\nrole: qa\nskills: [systematic-debugging]\n---\n\nBase body.\n",
    )
    .expect("write base");
    std::fs::write(
        tmp.path().join("kitchen-sink.md"),
        "---\nname: kitchen-sink\nrole: qa\ndescription: 'Every field the composer models.'\n\
         model: sonnet\nmax_tokens: 4096\nresource_tier: standard\nextends: base-qa\n\
         skills: [test-driven-development]\ntools: [Read, Write]\n\
         initialPrompt: \"Go.\"\n---\n\nChild body.\n",
    )
    .expect("write child");

    let composed = compose_agent("kitchen-sink", tmp.path()).expect("compose");
    let keys = frontmatter_keys(&composed).expect("composed output has a frontmatter block");

    let unknown: Vec<&String> = keys
        .iter()
        .filter(|k| !TRUSTY_MPM_KEYS.contains(&k.as_str()))
        .collect();
    assert!(
        unknown.is_empty(),
        "the composer emits {unknown:?}, which TRUSTY_MPM_KEYS does not list — \
         add them there or the quarantine stops recognising its own artifacts. \
         Composed keys: {keys:?}"
    );
    assert_eq!(
        agent_schema(&composed),
        AgentSchema::TrustyMpm,
        "freshly composed output must classify as trusty-mpm's own"
    );
    assert!(
        !composed.contains("\nextends:"),
        "composed output must not carry `extends:` — the schema gate relies on it"
    );
}
