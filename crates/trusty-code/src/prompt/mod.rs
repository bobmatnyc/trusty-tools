//! System-prompt assembly layer (parity-spec — `docs/trusty-code/parity-spec.md`).
//!
//! Why: The model-comparison harness must drive the *same* system instruction
//! surface through every model so a comparison measures the model, not the
//! scaffolding (parity-spec §1). This module owns that surface: the
//! byte-identical BASE preamble, its version token, and the fixed-order
//! assembler that merges the BASE preamble with the per-agent prompt, project
//! `CLAUDE.md` context, and the optional per-tier fallback guidance.
//! What: Re-exports [`BASE_PREAMBLE`], [`BASE_PREAMBLE_VERSION`],
//! [`PromptAssembler`], and [`assemble_system_prompt`].
//! Test: `prompt::tests::*` (via `assembler.rs`'s inline test include).

mod assembler;
mod preamble;
mod version;

pub use assembler::{PromptAssembler, assemble_system_prompt};
pub use preamble::BASE_PREAMBLE;
pub use version::BASE_PREAMBLE_VERSION;
