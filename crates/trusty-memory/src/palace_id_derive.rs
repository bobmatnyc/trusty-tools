//! Default palace-ID derivation from project identity (issue #1217).
//!
//! Why: the pure derivation core moved into `trusty-common` (issue #1605) so
//! trusty-mpm's managed-session MCP injection can derive the *identical* palace
//! slug it pins via `TRUSTY_MEMORY_PALACE` without taking a trusty-mpm →
//! trusty-memory dependency edge. This module is now a thin re-export shim:
//! every trusty-memory call site (`messaging::operations`, `messaging::mod`)
//! keeps working unchanged via `crate::palace_id_derive::*`, while the single
//! source of truth — and the unit tests for every precedence level / git-URL
//! variant — lives in [`trusty_common::palace_id`].
//!
//! What: re-exports [`derive_palace_id`], [`owner_repo_from_git_remote`],
//! [`parent_dir_slug`], [`PALACE_OVERRIDE_ENV`], and [`palace_override_from_env`].
//!
//! Test: behaviour is pinned by `cargo test -p trusty-common -- palace_id::tests`;
//! trusty-memory inherits it transitively (its `messaging` tests exercise the
//! env-override read path that wraps these shims).

pub use trusty_common::palace_id::{
    PALACE_OVERRIDE_ENV, derive_palace_id, owner_repo_from_git_remote, palace_override_from_env,
    parent_dir_slug,
};
