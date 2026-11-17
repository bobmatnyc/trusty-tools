//! `tm secrets` — a client of the trusty-secrets on-demand socket (#7521).
//!
//! Why: owner ruling 24 — trusty-secrets serves its own socket and tm is a
//! client, so this module holds no store. The 2026-09-17 rulings fix the
//! `set` grammar: one upsert verb, the value from the clipboard, never from
//! argv. #10394: the 2026-10-28 ruling fixes the confirmation — first 4 and
//! last 5 characters and the length at 16+, last 5 and the length at 10-15,
//! the length alone under 10 (`mask_secret`).
//! What: [`run`] builds a [`Ctx`] — the socket client
//! (`OnDemandSecrets`, which starts the server on first call), the working
//! directory as the project, the system clipboard and stdin — and
//! [`dispatch`]es one verb from `verbs`. Output goes to one writer; errors
//! carry fixed text, key names and paths only. Nothing here logs. Every
//! write — `set`, `remove`, `import`, `copy` — stays in the project scope;
//! `assign` (#11015) writes the scope of the project it names.
//! Test: `tests.rs` beside this file, against an in-process server over a
//! `MemoryBackend` in a temp dir, plus a second one as the `copy` target.
//!
//! Governing document: DOC-74 §9, §15.2, §15.6
//! (`docs/specs/DOC-74-secrets-integration.md`).

#[cfg(unix)]
use std::io::Write;

use crate::cli::SecretsAction;

// #11015: `tm secrets assign`.
#[cfg(unix)]
mod assign;
#[cfg(unix)]
mod doctor;
// #9621: `tm secrets exec` and its hidden stub.
#[cfg(unix)]
mod exec;
#[cfg(unix)]
mod exec_proc;
#[cfg(unix)]
mod exec_scrub;
#[cfg(unix)]
mod exec_stub;
// #9071 S10: the stub's token file and the sweep of stale token directories.
#[cfg(unix)]
mod exec_token;
#[cfg(unix)]
mod exec_wire;
#[cfg(unix)]
mod paste;
// #9072: `tm secrets resolve`, the tier-3 grant client.
#[cfg(unix)]
mod resolve;
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
/// `exec` and its stub end the process with the command's exit status.
/// Test: `tests.rs` covers [`dispatch`]; this wrapper only builds the context.
#[cfg(unix)]
pub(crate) async fn run(action: SecretsAction) -> anyhow::Result<()> {
    // #9621: the stub needs no clipboard, project or default socket.
    if let SecretsAction::ExecStub {
        socket,
        token_fd,
        report_fd,
        env,
        stdin,
        command,
    } = action
    {
        let code = match exec_spec(&env, stdin.as_ref(), command) {
            Ok(spec) => {
                let args = exec_stub::StubArgs {
                    socket,
                    token_fd,
                    report_fd,
                    spec,
                };
                exec_stub::run(args).await
            }
            Err(_) => 1,
        };
        std::process::exit(code);
    }
    // #9072: dials only the granted socket; the on-demand client could
    // start a server.
    if let SecretsAction::Resolve { key } = &action {
        return resolve::run(key.text(), &mut std::io::stdout()).await;
    }
    let client = trusty_secrets::server::OnDemandSecrets::new()
        .map_err(|e| anyhow::anyhow!("tm secrets: {e}"))?;
    let project = std::env::current_dir()?;
    if let SecretsAction::Exec {
        env,
        stdin,
        command,
    } = action
    {
        let spec = exec_spec(&env, stdin.as_ref(), command)?;
        let code = exec::run(&client, &project, spec).await?;
        std::io::stdout().flush()?;
        std::process::exit(code);
    }
    let clipboard = SystemClipboard::system();
    let ctx = Ctx {
        client: &client,
        project: &project,
        clipboard: &clipboard,
        stdin: &StdinSource,
        ci: doctor::is_ci(std::env::var("CI").ok().as_deref()),
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
    dispatch_with(ctx, assign::PRODUCTION, action, out).await
}

/// [`dispatch`], with `assign`'s project resolver and caller check from
/// `seam` (#11015).
///
/// Test: `assign_tests.rs`.
#[cfg(unix)]
pub(crate) async fn dispatch_with(
    ctx: &Ctx<'_>,
    seam: assign::Seam<'_>,
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
        SecretsAction::Copy {
            from,
            to,
            from_service,
            from_account,
            from_vault,
            from_key,
            keys,
        } => {
            let keys: Vec<&str> = keys.iter().map(|k| k.text()).collect();
            // #10998: clap pairs the flags; a lone half cannot reach here.
            let source = match (&from_service, &from_account, &from_vault, &from_key) {
                (Some(service), Some(account), _, _) => Some(verbs::NamedSource::Item {
                    service: service.as_str(),
                    account: account.as_str(),
                }),
                (_, _, Some(vault), Some(key)) => Some(verbs::NamedSource::Vault {
                    vault: vault.as_str(),
                    key: key.as_str(),
                }),
                _ => None,
            };
            match source {
                Some(source) => verbs::copy_named(ctx, &from, &to, source, &keys, out).await,
                None => verbs::copy(ctx, &from, &to, &keys, out).await,
            }
        }
        SecretsAction::Assign {
            src,
            project,
            dest,
            from_backend,
            to_backend,
        } => {
            let args = assign::AssignArgs {
                src: src.text(),
                project: &project,
                dest: dest.as_deref(),
                from: &from_backend,
                to: &to_backend,
            };
            assign::assign(ctx, seam, args, out).await
        }
        SecretsAction::Doctor => doctor::doctor(ctx, out).await,
        SecretsAction::Resolve { key } => resolve::run(key.text(), out).await,
        // #9621: both end the process, so only [`run`] dispatches them.
        SecretsAction::Exec { .. } | SecretsAction::ExecStub { .. } => {
            anyhow::bail!("tm secrets exec: runs only from the command line")
        }
    }
}

/// The validated [`exec_wire::ExecSpec`] of `exec`'s or the stub's arguments.
#[cfg(unix)]
fn exec_spec(
    env: &[crate::cli::ArgText],
    stdin: Option<&crate::cli::ArgText>,
    command: Vec<String>,
) -> anyhow::Result<exec_wire::ExecSpec> {
    let env: Vec<&str> = env.iter().map(|e| e.text()).collect();
    exec_wire::ExecSpec::parse(&env, stdin.map(|s| s.text()), command)
}

#[cfg(all(test, unix))]
mod exec_tests;
#[cfg(all(test, unix))]
mod exec_token_tests;
#[cfg(all(test, unix))]
mod paste_tests;
#[cfg(all(test, unix))]
mod tests;
