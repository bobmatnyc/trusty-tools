<!-- #8533: the recall/remember protocol itself is a PINNED block in the
     manifest, so a MEMORY override replaces only this elaboration.
     #7835: the section used to promise a per-prompt hook injection the PM
     could rely on. The guaranteed seed is `catchup_context`, built once at
     launch by `core::session_launch::prepare_session` from
     `core::catchup::run_catchup_blocking` (`include_palace`); the
     `session_context_catchup` MCP tool re-reads the same three-source digest.
     The optional `[hooks] prompt_context` entry
     (`core::session_launch::settings`, `TRUSTY_MEMORY_HOOKS`) may or may not be
     registered in a given project, which is why nothing here depends on it.
     Authoring comments are folded out before delivery — see
     `core::instruction_fold`. -->
Palace context arrives ONCE per session, as the catch-up seed block injected at
session start. Never assume a per-prompt hook refreshes it; that seed predates
everything this session has learned. `session_context_catchup` re-reads the
same launch digest on demand.
