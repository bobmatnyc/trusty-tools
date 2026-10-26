//! Keep credential values out of the MCP `env` blocks `tm mcp add` writes (#10041).
//!
//! Why: `tm mcp add slack-mcp -e SLACK_BOT_TOKEN=<token> slack-mcp` used to
//! write the token verbatim into the managed `.claude.json`, a mode-0644 file
//! every local user could read. The servers tm registers already resolve their
//! own credential at launch through `trusty_common::credentials::resolve_key`
//! (process env, then `.env.local`, then the credential store), so the env
//! block never needed the value — the store does.
//! What: [`store_credential_env`] moves every registry-mapped `KEY=VALUE` pair
//! into the credential store, confirmed by read-back, and removes it from the
//! env map. A credential-shaped key with no registry mapping is refused, since
//! it has no store slot and the only other place for it is the file.
//! Test: `mcp_credential_env_tests.rs` beside this file.

use anyhow::{Result, bail};
use serde_json::{Map, Value};
use trusty_common::credential_registry::provider_for_env_var;
use trusty_common::credentials::KeyStore;
use trusty_common::launchd_secrets::is_credential_env_key;

/// One env pair bound for the credential store.
struct Planned {
    /// The env var name, as the operator typed it.
    key: String,
    /// The store slot the registry maps it to.
    provider: &'static str,
    /// The value. Never formatted.
    value: String,
}

/// Move every credential in `env` into `store`, removing it from `env`.
///
/// Why: see the module doc. Every refusal happens BEFORE any write, so a
/// refused add changes neither the store nor the env map; a store failure
/// returns `Err` so the caller writes no config at all. Nothing here falls back
/// to leaving the value in `env`.
/// What: a registry-mapped key (`provider_for_env_var`) is written with
/// `set` — overwriting, because the operator just supplied this value — and
/// read back; only a byte-equal read-back removes it from `env`. Returns the
/// keys moved, in order. Every error names the key and the store's error kind,
/// never a value.
///
/// # Errors
///
/// A credential-shaped key with no registry mapping; an empty value for a
/// mapped key; a store write or read-back that fails or disagrees.
///
/// # Code Contract
/// Postconditions:
/// - On `Ok(moved)`, no key in `moved` is in `env`, and `store` serves each
///   one's value back byte-for-byte.
/// - On `Err`, `env` is unchanged.
///
/// Test: `a_mapped_credential_moves_to_the_store_and_out_of_env`,
/// `a_store_write_failure_keeps_the_value_out_of_env_and_errors`,
/// `a_read_back_mismatch_is_an_error`,
/// `an_unmapped_credential_key_is_refused_before_any_write`,
/// `ordinary_env_pairs_are_left_alone`.
pub fn store_credential_env(
    env: &mut Map<String, Value>,
    store: &dyn KeyStore,
) -> Result<Vec<String>> {
    let mut planned = Vec::new();
    for (key, value) in env.iter() {
        let value = value.as_str().unwrap_or_default();
        match provider_for_env_var(key) {
            Some(provider) => {
                if value.is_empty() {
                    bail!("--env {key} is empty; nothing to store");
                }
                planned.push(Planned {
                    key: key.clone(),
                    provider,
                    value: value.to_string(),
                });
            }
            None if is_credential_env_key(key) => bail!(
                "--env {key} looks like a credential, and no credential provider is registered \
                 for it, so tm has nowhere to keep it but the config file; refusing to write it \
                 there (#10041). Put it in `.env.local` instead, or add it to the credential registry"
            ),
            None => {}
        }
    }

    for p in &planned {
        if let Err(e) = store.set(p.provider, &p.value) {
            bail!(
                "could not store {} in the credential store ({}); nothing was written to the \
                 MCP config",
                p.key,
                trusty_common::credentials::StoreErrorKind::from(&e).as_str()
            );
        }
        match store.try_get(p.provider) {
            Ok(Some(back)) if back == p.value => {}
            Ok(_) => bail!(
                "the credential store did not read back {} (mismatch); nothing was written to \
                 the MCP config",
                p.key
            ),
            Err(e) => bail!(
                "the credential store could not read back {} ({}); nothing was written to the \
                 MCP config",
                p.key,
                trusty_common::credentials::StoreErrorKind::from(&e).as_str()
            ),
        }
    }

    Ok(planned
        .into_iter()
        .map(|p| {
            env.remove(&p.key);
            p.key
        })
        .collect())
}

#[cfg(test)]
#[path = "mcp_credential_env_tests.rs"]
mod tests;
