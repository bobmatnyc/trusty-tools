//! `tm secrets resolve <key>`: the tier-3 grant client on the command line
//! (#9072, DOC-74 §15.8).
//!
//! Why: owner ruling 2026-10-21 — no Python or npm package. A program that
//! `tm secrets exec` started, in any language, runs this verb to read one
//! granted value.
//! What: [`run`] reads the token on the fd `TRUSTY_SECRETS_TOKEN_FD` names
//! and sends one `secrets.resolve` through `trusty_secrets::granted`, off
//! the runtime. It writes the value with no trailing newline, so `$(...)`
//! and a pipe see the exact bytes. Every failure is one fixed line from
//! [`describe`]: no key, value, token, path or server text, and nothing on
//! the output. It never grants, revokes or starts a server.
//! Test: `resolve_prints_only_the_granted_value`,
//! `resolve_failures_exit_nonzero_with_a_fixed_error_and_no_value`,
//! `resolve_error_output_never_carries_the_value_or_the_token`
//! (`tests/tm_secrets_resolve.rs`), `resolve_errors_drop_paths_and_io_text`.

use std::io::Write;

use anyhow::anyhow;
use trusty_secrets::SecretKey;
use trusty_secrets::granted::{self, GrantedError, Refusal};

const PREFIX: &str = "tm secrets resolve";

/// Resolve `key` under this process's grant and write its value to `out`.
///
/// Test: `resolve_prints_only_the_granted_value`,
/// `resolve_failures_exit_nonzero_with_a_fixed_error_and_no_value`.
///
/// # Errors
///
/// An invalid key name, every [`GrantedError`] (as [`describe`] text) and a
/// failed write; nothing is written before the value is in hand.
pub(crate) async fn run(key: &str, out: &mut dyn Write) -> anyhow::Result<()> {
    // #9072: the key may be a value typed by mistake, so it is never echoed.
    let key = SecretKey::new(key).map_err(|_| anyhow!("{PREFIX}: the key name is not valid"))?;
    let value = tokio::task::spawn_blocking(move || granted::resolve(&key))
        .await
        .map_err(|_| anyhow!("{PREFIX}: the resolve did not finish"))?
        .map_err(|e| anyhow!("{PREFIX}: {}", describe(&e)))?;
    out.write_all(value.expose().as_bytes())
        .and_then(|()| out.flush())
        .map_err(|_| anyhow!("{PREFIX}: the value could not be written"))
}

/// The fixed text for `error`.
///
/// Why: a caller scripts against the message, and an io error, a path or
/// the server's own text is neither stable nor needed to act on.
fn describe(error: &GrantedError) -> String {
    match error {
        GrantedError::NoGrant => "no exec grant: run this under `tm secrets exec`".into(),
        GrantedError::TokenUnreadable(_) | GrantedError::InvalidToken => {
            "the grant token is unreadable or malformed".into()
        }
        GrantedError::HomeUnavailable => "the home directory is unavailable".into(),
        GrantedError::UntrustedSocket { reason, .. } => {
            format!("refusing the trusty-secrets socket: {reason}")
        }
        GrantedError::Unavailable { .. } => "trusty-secrets is not running".into(),
        GrantedError::Timeout | GrantedError::Transport(_) => {
            "trusty-secrets did not answer".into()
        }
        GrantedError::Refused(refusal) => {
            format!("trusty-secrets refused the key ({})", refusal_kind(refusal))
        }
        _ => "the trusty-secrets answer was not usable".into(),
    }
}

/// The refusal's wire kind when it is a plain `snake_case` word, else its code.
fn refusal_kind(refusal: &Refusal) -> String {
    match refusal.kind.as_deref() {
        Some(kind)
            if (1..=64).contains(&kind.len())
                && kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
        {
            kind.to_owned()
        }
        _ => refusal.code.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: #9072 — the socket path and an io error's text never reach the
    /// message, so it stays the same on every machine.
    /// Test: itself.
    #[test]
    fn resolve_errors_drop_paths_and_io_text() {
        let io = || std::io::Error::other("io-detail-9072");
        let cases = [
            GrantedError::Unavailable {
                path: "/tmp/path-9072/secrets.sock".into(),
                source: io(),
            },
            GrantedError::Transport(io()),
            GrantedError::TokenUnreadable(io()),
        ];
        for error in cases {
            let text = describe(&error);
            assert!(!text.contains("9072"), "{text}");
        }
    }
}
