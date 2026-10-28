//! Keep credential values out of the MCP `env` blocks `tm mcp add` writes (#10041).
//!
//! Why: `tm mcp add slack-mcp -e SLACK_BOT_TOKEN=<token> slack-mcp` used to
//! write the token verbatim into the managed `.claude.json`, a mode-0644 file
//! every local user could read. The trusty servers in [`STORE_READING_COMMANDS`]
//! resolve their own credential at launch through
//! `trusty_common::credentials::resolve_key` (process env, then `.env.local`,
//! then the credential store), so for them the env block never needed the
//! value — the store does. Any other server reads only its process env, so
//! moving its credential to the store would launch it without one.
//! What: [`store_credential_env`] moves every registry-mapped `KEY=VALUE` pair
//! of a store-reading server into the credential store, confirmed by
//! read-back, and removes it from the env map. A credential for any other
//! server, a credential-shaped key with no registry mapping, and a store slot
//! already holding a different value (without `replace`) are refused.
//! Test: `mcp_credential_env_tests.rs` beside this file.

use std::path::Path;

use anyhow::{Result, bail};
use serde_json::{Map, Value};
use trusty_common::credential_registry::provider_for_env_var;
use trusty_common::credentials::{KeyStore, KeyStoreError, StoreErrorKind};
use trusty_common::launchd_secrets::is_credential_env_key;

/// Executable basenames of trusty binaries that read their credential through
/// `trusty_common::credentials::resolve_key` / `resolve_key_with`.
///
/// #10041: only these may have a credential moved out of their env block.
/// `slack-mcp`/`telegram-mcp`: trusty-channels' API clients; `tagent`: its
/// LLM credential resolver; `tcode`: its LLM client.
const STORE_READING_COMMANDS: &[&str] = &[
    "slack-mcp",
    "telegram-mcp",
    "tagent",
    "trusty-agents",
    "tcode",
    "trusty-code",
];

/// Does the MCP server `command` read its credential from tm's store?
///
/// Why: a credential may leave the env block only when the server will find
/// it in the store at launch; a third-party server (`npx …`, `uvx …`) never
/// looks there (#10041).
/// What: true when the command's basename (a bare name or a path) is in
/// [`STORE_READING_COMMANDS`].
/// Test: `store_reading_commands_match_by_basename`.
pub fn reads_credential_store(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|base| STORE_READING_COMMANDS.contains(&base))
}

/// One env pair bound for the credential store.
struct Planned {
    /// The env var name, as the operator typed it.
    key: String,
    /// The store slot the registry maps it to.
    provider: &'static str,
    /// The value. Never formatted.
    value: String,
    /// False when the store already holds exactly this value.
    write: bool,
}

/// Where a `tm mcp add` env block is headed, and how the store may be changed.
#[derive(Debug, Clone, Copy)]
pub struct CredentialTarget<'a> {
    /// The MCP server name, for messages.
    pub server: &'a str,
    /// The server's command, as it will be written to the config.
    pub command: &'a str,
    /// `--replace`: overwrite a store slot that holds a different value.
    pub replace: bool,
}

/// Move every credential in `env` into `store`, removing it from `env`.
///
/// Why: see the module doc. Every refusal happens BEFORE any write, so a
/// refused add changes neither the store nor the env map; a store failure
/// returns `Err` so the caller writes no config at all. Nothing here falls back
/// to leaving the value in `env`.
/// What: for a registry-mapped key (`provider_for_env_var`): refuses unless
/// `target.command` [`reads_credential_store`]; reads the slot first — an
/// equal value is kept without a write, a different one is refused unless
/// `target.replace` (#8563's rule, as `--fix` applies it); otherwise writes
/// with `set` and requires a byte-equal read-back. Returns the keys moved, in
/// order. Every error names the key and the store's error kind, never a value.
///
/// # Errors
///
/// A credential-shaped key with no registry mapping; a mapped key for a
/// server that does not read the store; an empty value for a mapped key; a
/// slot holding a different value without `replace`; a store read, write or
/// read-back that fails or disagrees.
///
/// # Code Contract
/// Postconditions:
/// - On `Ok(moved)`, no key in `moved` is in `env`, and `store` serves each
///   one's value back byte-for-byte.
/// - On `Err`, `env` is unchanged, and the store is unchanged unless a write
///   or read-back failed.
///
/// Test: `a_mapped_credential_moves_to_the_store_and_out_of_env`,
/// `a_store_write_failure_keeps_the_value_out_of_env_and_errors`,
/// `a_read_back_mismatch_is_an_error`,
/// `an_unmapped_credential_key_is_refused_before_any_write`,
/// `ordinary_env_pairs_are_left_alone`,
/// `a_third_party_server_credential_is_refused_and_not_stored`,
/// `a_different_stored_value_is_refused_without_replace`,
/// `replace_overwrites_a_different_stored_value`,
/// `an_equal_stored_value_is_a_no_op_success`.
pub fn store_credential_env(
    env: &mut Map<String, Value>,
    store: &dyn KeyStore,
    target: CredentialTarget<'_>,
) -> Result<Vec<String>> {
    let CredentialTarget {
        server,
        command,
        replace,
    } = target;
    let mut planned = Vec::new();
    for (key, value) in env.iter() {
        let value = value.as_str().unwrap_or_default();
        match provider_for_env_var(key) {
            Some(provider) => {
                if value.is_empty() {
                    bail!("--env {key} is empty; nothing to store");
                }
                // #10041: a third-party server never reads tm's store.
                if !reads_credential_store(command) {
                    bail!(
                        "--env {key}: MCP server '{server}' runs `{command}`, which does not read \
                         tm's credential store, so moving {key} there would launch it without \
                         the credential, and writing it into the MCP config is what #10041 \
                         forbids; refusing. Supply {key} to '{server}' outside the MCP config \
                         (for example in the environment the session starts from), or use a \
                         trusty server that resolves it from the store"
                    );
                }
                let write = match store.try_get(provider) {
                    Ok(Some(existing)) if !existing.is_empty() && existing == value => false,
                    // #10041: never overwrite a different stored credential
                    // silently — #8563's rule, as `--fix` applies it.
                    Ok(Some(existing)) if !existing.is_empty() && !replace => bail!(
                        "--env {key}: the credential store already holds a different {provider} \
                         credential; nothing was written. Pass --replace to overwrite it"
                    ),
                    Ok(_) => true,
                    Err(e) => bail!(
                        "could not read the credential store before storing {key} ({}); nothing \
                         was written",
                        kind_of(&e)
                    ),
                };
                planned.push(Planned {
                    key: key.clone(),
                    provider,
                    value: value.to_string(),
                    write,
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

    for p in planned.iter().filter(|p| p.write) {
        if let Err(e) = store.set(p.provider, &p.value) {
            bail!(
                "could not store {} in the credential store ({}); nothing was written to the \
                 MCP config",
                p.key,
                kind_of(&e)
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
                kind_of(&e)
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

/// The store error's kind, never its text.
fn kind_of(e: &KeyStoreError) -> &'static str {
    StoreErrorKind::from(e).as_str()
}

#[cfg(test)]
#[path = "mcp_credential_env_tests.rs"]
mod tests;
