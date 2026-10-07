//! The `store` feature: backends, the names-only index, scopes, masking, and
//! config resolution (DOC-74 §15.2).
//!
//! Why: this is the half of the crate that touches the OS — the keychain,
//! the value files, the index directory, the git remote — so callers that
//! only name keys can leave it out.
//! What: re-exports of the submodules below, including [`resolve`], the
//! in-process `secret://` resolver behind `tm secrets exec`. Every OS
//! touchpoint sits in the private `platform` module.
//! Test: the `*_tests.rs` files beside this module.

mod backend;
// #7519: the runner the CLI-backed backends share; Unix only, because it
// kills the child's process group.
#[cfg(all(unix, feature = "cli-backends"))]
pub mod cli;
pub mod config;
mod dotenv;
// #9326: the 0600 value-file backend needs Unix modes and owners. #4567: the
// audit sink reuses its mode, owner and symlink checks, so it is crate-visible.
#[cfg(unix)]
pub(crate) mod file;
mod index;
mod keychain;
mod mask;
#[cfg(any(test, feature = "test-support"))]
mod memory;
pub(crate) mod platform;
pub mod resolve;
mod scope;
mod secret_store;

pub use backend::{Capabilities, SecretBackend, default_backend, open_backend};
pub use dotenv::parse_dotenv;
#[cfg(unix)]
pub use file::{FileBackend, VALUES_SUBDIR};
pub use index::{DEFAULT_LOCK_TIMEOUT, INDEX_SUBDIR, NamesIndex};
pub use keychain::KeychainBackend;
pub use mask::{MASK_HEAD_CHARS, mask_secret};
#[cfg(any(test, feature = "test-support"))]
pub use memory::MemoryBackend;
pub use platform::{GIT_ENV_REDIRECTS, git_redirect_vars};
pub use resolve::{EnvEntry, ResolvedVar, VarSource, resolve_env, resolve_reference};
pub use scope::{
    RemoteRefusal, SUPPORTED_REMOTE_HOST, ScopeSet, VaultOverride, parse_remote_identity,
};
pub use secret_store::SecretStore;

#[cfg(test)]
#[path = "config_scope_tests.rs"]
mod config_scope_tests;
#[cfg(test)]
#[path = "index_tests.rs"]
mod index_tests;
#[cfg(test)]
#[path = "resolve_tests.rs"]
mod resolve_tests;
#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
