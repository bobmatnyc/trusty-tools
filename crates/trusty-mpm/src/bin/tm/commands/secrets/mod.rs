//! `tm secrets` — a client of the trusty-secrets on-demand socket (#7521).
//!
//! Why: owner ruling 24 — trusty-secrets serves its own socket and tm is a
//! client, so this module holds no store. The 2026-09-17 rulings fix the
//! `set` grammar: one upsert verb, the value from the clipboard, never from
//! argv. The 2026-10-01 ruling fixes the confirmation: first 8 characters and
//! the length, or the length alone for 8 characters or fewer.
//! What: [`run`] builds a [`Ctx`] — the socket client
//! (`OnDemandSecrets`, which starts the server on first call), the working
//! directory as the project, the system clipboard and stdin — and
//! [`dispatch`]es one verb from `verbs`. Output goes to one writer; errors
//! carry fixed text, key names and paths only. Nothing here logs. Every
//! write — `set`, `remove`, `import`, `copy` — stays in the project scope.
//! Test: `tests.rs` beside this file, against an in-process server over a
//! `MemoryBackend` in a temp dir, plus a second one as the `copy` target.
//!
//! Governing document: DOC-74 §9, §15.2, §15.6
//! (`docs/specs/DOC-74-secrets-integration.md`).

#[cfg(unix)]
use std::io::Write;

use crate::cli::SecretsAction;

#[cfg(unix)]
mod doctor;
#[cfg(unix)]
mod session;
#[cfg(unix)]
mod value;
#[cfg(unix)]
mod verbs;

#[cfg(unix)]
pub(crate) use session::Ctx;
#[cfg(unix)]
pub(crate) use value::{StdinSource, SystemClipboard, ValueSource};

/// Run one `tm secrets` verb against the default socket.
///
/// Why: the production entry; tests call [`dispatch`] with their own [`Ctx`].
/// What: the socket is `~/.trusty-tools/trusty-secrets/secrets.sock`; the
/// project is the working directory. Exit status 0 on success, 1 on any
/// error (anyhow from `main`).
/// Test: `tests.rs` covers [`dispatch`]; this wrapper only builds the context.
#[cfg(unix)]
pub(crate) async fn run(action: SecretsAction) -> anyhow::Result<()> {
    let client = trusty_secrets::server::OnDemandSecrets::new()
        .map_err(|e| anyhow::anyhow!("tm secrets: {e}"))?;
    let project = std::env::current_dir()?;
    let ctx = Ctx {
        client: &client,
        project: &project,
        clipboard: &SystemClipboard,
        stdin: &StdinSource,
    };
    dispatch(&ctx, action, &mut std::io::stdout()).await
}

/// `tm secrets` needs trusty-common's Unix socket stack.
#[cfg(not(unix))]
pub(crate) async fn run(_action: SecretsAction) -> anyhow::Result<()> {
    anyhow::bail!("tm secrets: the secrets socket needs a Unix platform")
}

/// Run `action` in `ctx`, writing its report to `out`.
///
/// Test: every test in `tests.rs`.
#[cfg(unix)]
pub(crate) async fn dispatch(
    ctx: &Ctx<'_>,
    action: SecretsAction,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    match action {
        SecretsAction::Set {
            key,
            group,
            value,
            extra,
        } => {
            let args = verbs::SetArgs {
                key: key.text(),
                group: group.as_ref().map(|g| g.text()),
                value: value.as_ref().map(|v| v.text()),
                extra: !extra.is_empty(),
            };
            verbs::set(ctx, args, out).await
        }
        SecretsAction::List => verbs::list(ctx, out).await,
        // #7521: slice 2 — remove, import, copy.
        SecretsAction::Remove { key, group } => {
            verbs::remove(ctx, key.text(), group.as_ref().map(|g| g.text()), out).await
        }
        SecretsAction::Import { path, group } => {
            verbs::import(ctx, &path, group.as_ref().map(|g| g.text()), out).await
        }
        SecretsAction::Copy { from, to, keys } => {
            let keys: Vec<&str> = keys.iter().map(|k| k.text()).collect();
            verbs::copy(ctx, &from, &to, &keys, out).await
        }
        SecretsAction::Doctor => doctor::doctor(ctx, out).await,
    }
}

#[cfg(all(test, unix))]
mod tests;
