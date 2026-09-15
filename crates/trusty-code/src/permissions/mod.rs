//! Per-verb glob-pattern tool permissions for tcode agents (#7948).
//!
//! Why: before this, an agent's only tool control was `tcode_tools:` — a flat
//! exact-match allowlist that can say "this agent may run `bash`" and nothing
//! finer. Real coding work needs "`git status` without asking, never `rm`, ask
//! about everything else", which is what opencode's `permission` config
//! expresses and what the TUI permission prompt (#3422) has to prompt FROM.
//! What: `config` (the `permissions:` frontmatter schema, parser, and
//! fail-closed validation), `matcher` (glob matching, subject extraction, and
//! the precedence order), `gate` (the runtime decision and the ask/await
//! round trip), `session` (per-session pending requests and remembered
//! grants, plus the daemon-wide broker), `redact` (credential scrubbing for
//! the published subject), and `protocol` (the `session.permission.respond`
//! JSON-RPC method).
//!
//! The legacy `tcode_tools` allowlist is enforced FIRST: a tool absent from it
//! is denied before the permission map is consulted, so a map can only narrow
//! an agent's reach. #7948: the exception is `gate::HARNESS_REGISTERED_TOOLS` —
//! tools the harness wires up for the agent rather than the agent asking for
//! them — which the allowlist branch skips; a written `deny` still refuses one.
//! Stock bundled agents declare no `permissions:` block, so their behaviour
//! does not change.
//! Test: `permissions::tests` — one test module per submodule.

pub mod config;
pub mod gate;
pub mod matcher;
pub mod protocol;
pub mod redact;
pub mod session;

pub use config::{PermissionConfigError, PermissionMap, RuleDecision, parse_permissions};
pub use gate::{
    DEFAULT_ASK_TIMEOUT_SECS, Decision, DenySource, HARNESS_REGISTERED_TOOLS, Outcome,
    PERMISSION_MODE_ENV, PermissionContext, PermissionEvents, PermissionGate, PermissionMode,
};
pub use matcher::{RuleMatch, subjects_for, subjects_for_in_root};
pub use protocol::{PERMISSION_RESPOND_METHOD, PermissionDecision};
pub use redact::redact_subject;
pub use session::{PermissionBroker, SessionPermissions};

#[cfg(test)]
mod tests;
