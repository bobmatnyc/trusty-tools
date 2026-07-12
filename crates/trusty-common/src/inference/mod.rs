//! Unified inference provider adapter layer (epic #2400).
//!
//! Why: `trusty-code`, `trusty-review`, and three internal `trusty-agents`
//! layers each hand-rolled an LLM client, a credential lookup, and an
//! `.env.local` loader with subtly different precedence rules. Epic #2400
//! centralises all of that in `trusty-common` so every consumer shares one
//! implementation instead of six.
//!
//! What: Wave 1 ticket #2401 (this module) ships the credential resolution
//! layer: [`credentials::KeyStore`] trait, three backends
//! (`MemoryKeyStore`, `FileKeyStore`, `KeyringStore`), the `resolve_key`
//! precedence chain (process env > `.env.local` > secure store), and the
//! shared `redact_secret` helper. Later Wave 1/2 tickets add sibling modules
//! under `inference/` — the `InferenceAdapter` trait, the two-stage
//! configurator, and the capability registry — none of which exist yet.
//!
//! Test: see `credentials` submodule and its `*_tests` sibling files.

pub mod credentials;
