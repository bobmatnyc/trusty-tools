//! System-prompt assembly layer (parity-spec — `docs/trusty-code/parity-spec.md`).
//!
//! Why: The model-comparison harness must drive the *same* system instruction
//! surface through every model so a comparison measures the model, not the
//! scaffolding (parity-spec §1). This module owns that surface: the
//! byte-identical BASE preamble, its version token, and the fixed-order
//! assembler that merges the BASE preamble with the per-agent prompt, project
//! `CLAUDE.md` context, and the optional per-tier fallback guidance.
//! What: Re-exports [`BASE_PREAMBLE`], [`BASE_PREAMBLE_VERSION`],
//! [`PromptAssembler`], [`assemble_system_prompt`], and (#2059)
//! [`assemble_system_prompt_for_mode`] — the `HarnessMode`-branching entry
//! point `task::executor`/`runner::in_process` call. Its `skills_catalog`
//! parameter (#2069) is P1B's first token-efficiency layer to actually land:
//! `DailyDriver` appends the cheap, always-cached skill metadata catalog
//! (`crate::skills::format_skill_catalog`); `Parity` ignores it.
//! Test: `prompt::tests::*` (via `assembler.rs`'s inline test include).

mod assembler;
mod preamble;
mod version;

pub use assembler::{PromptAssembler, assemble_system_prompt, assemble_system_prompt_for_mode};
pub use preamble::BASE_PREAMBLE;
pub use version::BASE_PREAMBLE_VERSION;
