//! Built-in skills for memory, semantic search and knowledge-graph ingestion.
//!
//! Why: These rows carry `kind = Knowledge`, which is the ONLY mechanism
//! routing a capability into the Knowledge pane instead of the Skills pane
//! (DOC-57 §5.3). Before this table the Knowledge pane had no manifest to read
//! and said so; #3945's `AgentConfigKnowledge` names exactly this file as the
//! authority it was waiting for.
//! What: A `const` table of one-tool [`SkillDef`] rows.
//! Test: `super::super::tests::knowledge_kind_routes_out_of_the_skills_pane`.

use super::super::{SkillDef, SkillKind::Knowledge, tool_skill};
use super::GOOGLE_OAUTH;

pub(super) static TABLE: &[SkillDef] = &[
    // --- semantic + lexical search --------------------------------------
    tool_skill(
        "knowledge-search",
        "Knowledge Search",
        "Search this agent's bound knowledge store semantically and return passages.",
        "vector_search",
        Knowledge,
        None,
    ),
    tool_skill(
        "code-search",
        "Code Search",
        "Search the working project's code index for relevant files and symbols.",
        "search_code",
        Knowledge,
        None,
    ),
    tool_skill(
        "docs-search",
        "Documentation Search",
        "Search the harness's own documentation corpus.",
        "search_docs",
        Knowledge,
        None,
    ),
    tool_skill(
        "skill-search",
        "Skill Search",
        "Search the available skills by relevance to a question.",
        "search_skills",
        Knowledge,
        None,
    ),
    tool_skill(
        "session-search",
        "Search Past Sessions",
        "Search prior sessions for earlier work and decisions.",
        "search_sessions",
        Knowledge,
        None,
    ),
    // --- long-term memory -------------------------------------------------
    tool_skill(
        "memory-recall",
        "Memory Recall",
        "Recall stored project facts and decisions relevant to a question.",
        "memory_recall",
        Knowledge,
        None,
    ),
    tool_skill(
        "memory-search",
        "Memory Search",
        "Search long-term memory across palaces and rooms.",
        "memory_search",
        Knowledge,
        None,
    ),
    tool_skill(
        "memory-search-native",
        "Memory Search (Native)",
        "Search the native memory store for stored notes.",
        "search_memory",
        Knowledge,
        None,
    ),
    tool_skill(
        "memory-write",
        "Remember a Fact",
        "Store a durable fact or decision in long-term memory.",
        "memory_store",
        Knowledge,
        None,
    ),
    tool_skill(
        "memory-key-write",
        "Store a Memory Key",
        "Write a keyed value into the native memory store.",
        "store_memory",
        Knowledge,
        None,
    ),
    tool_skill(
        "memory-key-read",
        "Read a Memory Key",
        "Read a keyed value back from the native memory store.",
        "retrieve_memory",
        Knowledge,
        None,
    ),
    tool_skill(
        "memory-key-list",
        "List Memory Keys",
        "List the keys held in the native memory store.",
        "list_memory_keys",
        Knowledge,
        None,
    ),
    // --- knowledge-graph ingestion ---------------------------------------
    tool_skill(
        "okg-sources",
        "Knowledge Sources",
        "List the sources already ingested into this agent's knowledge graph.",
        "okg_sources",
        Knowledge,
        None,
    ),
    tool_skill(
        "okg-ingest-files",
        "Ingest a Document Folder",
        "Grow this agent's knowledge graph from a folder of documents.",
        "okg_ingest_docstore",
        Knowledge,
        None,
    ),
    tool_skill(
        "okg-ingest-gmail",
        "Ingest Gmail",
        "Grow this agent's knowledge graph from a Gmail search window.",
        "okg_ingest_gmail",
        Knowledge,
        Some(&GOOGLE_OAUTH),
    ),
    tool_skill(
        "okg-ingest-drive",
        "Ingest Google Drive",
        "Grow this agent's knowledge graph from a Google Drive folder.",
        "okg_ingest_drive",
        Knowledge,
        Some(&GOOGLE_OAUTH),
    ),
];
