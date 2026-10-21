<!-- #8533: the recall/remember protocol itself is a PINNED block in the
     manifest, so a MEMORY override replaces only this elaboration.
     #7835: the section used to promise a per-prompt hook injection the PM
     could rely on. The guaranteed seed is `catchup_context`, built once at
     launch by `core::session_launch::prepare_session` from
     `core::catchup::run_catchup_blocking` (`include_palace`); the
     `session_context_catchup` MCP tool re-reads the three sources with `full: true` (#9883).
     The optional `[hooks] prompt_context` entry
     (`core::session_launch::settings`, `TRUSTY_MEMORY_HOOKS`) may or may not be
     registered in a given project, which is why nothing here depends on it.
     Authoring comments are folded out before delivery — see
     `core::instruction_fold`. -->
Palace context arrives ONCE, as the catch-up seed injected at session start; no
per-prompt hook refreshes it, so it predates all this session has learned.
`session_context_catchup` with `full: true` re-reads its sources on demand.
