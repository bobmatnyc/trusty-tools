//! Assistant INSTANCES — per-instance home, OKG store and persona (#4325).
//!
//! Why: "Assistant" is a TYPE; `izzie` and `cto-assistant` are INSTANCES of
//! that type, not distinct agent types (owner, 2026-07-30). The repo already
//! encoded the type half — `assistant/agent.toml` declares
//! `[agent] role = "assistant"` and both named personas carry
//! `extends = "assistant"` — but nothing encoded the INSTANCE half. An
//! instance's three belongings were spread across shared pools with no
//! per-instance container: the persona in the shared agents directory, the OKG
//! tree in the flat `<knowledge_dir>/<slug(agent)>` pool addressed by naming
//! convention, and `[[stores]]` with no field naming a per-assistant root at
//! all. That is why adding an instance today looks like adding a type. This
//! module is the container that was missing, and #4325 is the foundation the
//! rest of milestone 22 pillar (b) builds on (#4281, #4282, #4283).
//!
//! What:
//! - `instance` — [`ASSISTANT_ROLE`], the type discriminator, and
//!   [`AssistantInstanceId`], a VALIDATED instance name (it becomes a directory
//!   name, so it is checked, never silently slugged).
//! - `home` — [`AssistantHome`]: the app-generated, DOTLESS, human-browsable
//!   per-instance home at `~/trusty-agents/<instance>/`, holding
//!   `instructions.md`, `config.toml`, `agents/`, `okg/`, `attachments/` and
//!   `stores/`. Both paths are the owner's, verbatim (2026-08-01):
//!   `trusty-agents/<agent>/okg/` and
//!   `trusty-agents/<agent>/stores/<store-identifier>/` — this module creates
//!   the `stores/` PARENT only; what lives beneath it is a separate spec.
//!   Plus [`AssistantHome::store_root`], which
//!   resolves a `[[stores]]` binding's new `root` field inside that home
//!   (#4325 requirement 1 — #3890's data-model gap).
//! - `health` — [`inspect`]: the DETECTION half of #4325's resilience
//!   requirement. External modification is expected, so the system reports
//!   missing/malformed entries with a remedy the concierge can narrate; it
//!   never auto-migrates and never overwrites.
//! - `error` — [`AssistantError`], for resolving and creating a home.
//!   Inspecting one yields findings, not errors.
//!
//! Deliberately NOT here: the writable store-config path (#3890), index→OKG
//! entity extraction (#4283), the attached-index cap (#4282), everything under
//! [`home::STORES_DIR`] (store-identifier derivation, extraction state,
//! per-store subdirectories, extraction logic — a separate spec owns those),
//! and any migration of the existing dotted `~/.trusty-agents` tree — #4325
//! confirms the dotless decision but does not require auto-relocation.
//!
//! Ownership, not enforcement: "app-generated" was narrowed by the owner
//! (2026-07-29) to mean the app OWNS THE ORIGIN of the directory. Access control
//! is explicitly out of scope — there is no write-enforcement, no floor, no
//! narrow-only semantics. Users may change anything in here; the system's job is
//! to notice and help, which is what [`inspect`] is for. TOML is likewise
//! confirmed, not assumed: `config.toml` stays TOML and no format migration is
//! in scope.
//!
//! Naming caveat (#4406): two unrelated stores are both called "OKG" — the
//! trusty-kb markdown entity tree and the trusty-memory knowledge graph. #4325
//! specifies an `okg/` DIRECTORY inside the home, which only the filesystem
//! tree can be, so [`home::OKG_DIR`] is that tree. The owner CONFIRMED that
//! shape on 2026-08-01 — `trusty-agents/<agent>/okg/`, a store indexed by
//! trusty-search — so this is no longer an inference. The binding's separate
//! `palace` field stays untouched, which keeps the trusty-memory leg addressable
//! without changing this layout.
//!
//! Test: `tests` — the submodules under `assistants/tests/`.

pub mod error;
pub mod health;
pub mod home;
pub mod instance;

#[cfg(test)]
mod tests;

pub use error::AssistantError;
pub use health::{HomeHealth, HomeIssue, HomeIssueKind, inspect};
pub use home::{
    AGENTS_DIR, ASSISTANTS_DIR_ENV, ASSISTANTS_DIR_NAME, ATTACHMENTS_DIR, AssistantHome,
    AssistantHomeConfig, CONFIG_FILE, Created, INSTRUCTIONS_FILE, OKG_DIR, STORES_DIR,
    assistants_root,
};
pub use instance::{ASSISTANT_ROLE, AssistantInstanceId, is_assistant_role};
