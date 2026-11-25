//! Assistant model choice: the provider pin, the provider lock and the
//! assistant provider policy.
//!
//! Why (#11632): the chat clients, the tool loop and the provider adapters were
//! cut with the agent loop; an assistant's manifest still names its model and
//! provider, and loading it must still validate that choice the same way.
//! What: `provider_pin` (a manifest's `provider_id` pin and the slug it
//! implies), `provider_lock` (the operator's provider lock), and
//! `provider_policy` (the configured assistant default provider).
//! Test: each submodule's own tests.

pub mod provider_lock;
pub mod provider_pin;
pub mod provider_policy;
