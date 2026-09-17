//! System-prompt assembly layer (parity-spec — `docs/trusty-code/parity-spec.md`).
//!
//! Why: The model-comparison harness must drive the *same* system instruction
//! surface through every model so a comparison measures the model, not the
//! scaffolding (parity-spec §1). This module owns that surface: the
//! byte-identical BASE preamble, its version token, and the fixed-order
//! assembler that merges the BASE preamble with the per-agent prompt, project
//! `CLAUDE.md` context, and the optional per-tier fallback guidance.
//! #4602: every section that INSTRUCTS the model to call a named tool — file
//! discovery, batch writes, code discovery — left that byte-identical floor and
//! is gated on the run's tool registry, so an agent is never instructed to call
//! a tool it was not given. `BASE_PREAMBLE` still MENTIONS `write_file`,
//! `read_file` and `bash` inside the tool-use protocol, as `e.g.` illustrations
//! of what batching means; those are not instructions and are not gated.
//! What: Re-exports [`BASE_PREAMBLE`], [`BASE_PREAMBLE_VERSION`],
//! [`PromptAssembler`], [`assemble_system_prompt`], and (#2059)
//! [`assemble_system_prompt_for_mode`] — the `HarnessMode`-branching entry
//! point `task::executor`/`runner::in_process` call. Its `skills_catalog`
//! parameter (#2069) is P1B's first token-efficiency layer to actually land:
//! `DailyDriver` appends the cheap, always-cached skill metadata catalog
//! (`crate::skills::format_skill_catalog`); `Parity` ignores it.
//! Test: `prompt::tests::*` (via `assembler.rs`'s inline test include).
//!
//! [`BASE_PREAMBLE`]: crate::prompt::BASE_PREAMBLE
//! [`BASE_PREAMBLE_VERSION`]: crate::prompt::BASE_PREAMBLE_VERSION
//! [`PromptAssembler`]: crate::prompt::PromptAssembler
//! [`assemble_system_prompt`]: crate::prompt::assemble_system_prompt
//! [`assemble_system_prompt_for_mode`]: crate::prompt::assemble_system_prompt_for_mode

mod assembler;
mod preamble;
mod version;

pub use assembler::{
    BATCH_WRITE_TOOLS, DISCOVERY_GUIDANCE, DISCOVERY_GUIDANCE_TOOLS, FILE_DISCOVERY_TOOLS,
    GATED_SECTIONS, PromptAssembler, assemble_system_prompt, assemble_system_prompt_for_mode,
};
pub use preamble::{BASE_PREAMBLE, BATCH_WRITE_GUIDANCE, FILE_DISCOVERY_GUIDANCE};
pub use version::BASE_PREAMBLE_VERSION;
