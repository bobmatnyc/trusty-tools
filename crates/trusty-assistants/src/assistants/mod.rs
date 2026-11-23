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
//! - `roster` — [`discover_instances`]: which configs are instances of the
//!   type. `role = "assistant"` alone is not the rule — `ctrl` declares it too
//!   and is the concierge, not a selectable instance.
//! - `provision` — [`provision_startup_homes`]: the STARTUP call (owner,
//!   2026-08-01). Runs on every launch, creates what is missing, and CANNOT
//!   fail — a home the app cannot create becomes a reported issue with a
//!   remedy, never a failed boot.
//! - `mcp` — [`mcp::McpOverrides`]: the ASSISTANT tier of MCP configuration
//!   (#7454, ADR-0060). `<home>/config.toml`'s `[mcp]` table adds a server,
//!   replaces one by name, or disables one; the global tier and the resolver
//!   live in [`crate::mcp::shared`].
//! - `memory` — [`memory::MemoryConfig`] and [`memory::resolve_palace_plan`]:
//!   one trusty-memory palace per assistant plus the opt-in fan-out to other
//!   assistants' palaces (#7428). The binding's `palace` still wins where one
//!   is declared, so nothing migrates.
//! - `project` — (#11649) the optional `project_dir` key and
//!   [`instance_for_project`], which resolves a working directory to exactly
//!   one instance or a typed error.
//! - `instructions` — (#11649) [`AssistantHome::read_instructions`] and the
//!   role templates [`seed_role_instructions`] writes once.
//! - `instance_provision` — (#11649) [`provision_instance`]: one named
//!   instance with its role, project directory, fan-out peers and an
//!   `agent.toml` whose first `[[stores]]` binding is its own OKG and index.
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
//! markdown entity tree (`crate::kb`) and the trusty-memory knowledge graph. #4325
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
pub mod instance_provision;
pub mod instructions;
// #11742: which root entries are instance homes, for `ta list` and `ta ask`.
pub mod listing;
pub mod mcp;
pub mod memory;
pub mod memory_grants;
pub mod memory_policy;
pub mod project;
pub mod provision;
pub mod roster;

#[cfg(test)]
mod tests;

pub use error::AssistantError;
pub use health::{HomeHealth, HomeIssue, HomeIssueKind, inspect, inspect_project_claims};
pub use home::{
    AGENTS_DIR, APP_DIR_NAME, ASSISTANTS_DIR_ENV, ASSISTANTS_DIR_NAME, ATTACHMENTS_DIR,
    AssistantHome, AssistantHomeConfig, CONFIG_FILE, Created, INSTRUCTIONS_FILE,
    LEGACY_ASSISTANTS_DIR_ENV, OKG_DIR, STORES_DIR, app_dir, assistants_root,
};
pub use instance::{ASSISTANT_ROLE, AssistantInstanceId, is_assistant_role};
pub use instance_provision::{
    ProvisionedInstance, own_index_id, provision_instance, provision_instance_in,
};
pub use instructions::{AssistantRole, MAX_INSTRUCTIONS_BYTES, seed_role_instructions};
pub use listing::list_instances;
pub use mcp::{McpOverrides, OverridesRead, read_overrides, write_overrides};
pub use memory::{
    MemoryConfig, PalacePlan, PalaceSource, own_palace, read_memory_config, resolve_palace_plan,
    resolve_palace_plan_in, write_memory_config,
};
pub use project::{
    PROJECT_DIR_KEY, ProjectClaim, canonical_project_dir, instance_for_project, project_claims,
    set_project_dir,
};
pub use provision::{
    ProvisionedHome, StartupProvisioning, provision_all, provision_home, provision_startup_homes,
    provision_startup_homes_in,
};
pub use roster::{ASSISTANT_BASE, discover_instances};
