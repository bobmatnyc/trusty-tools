//! The `tm secrets` verbs, each one or a few `secrets.*` calls (#7521).
//!
//! Why: DOC-74 §9's grammar over the §15.2 methods. tm holds a value only
//! between reading it (clipboard or stdin) and sending it in `secrets.set`;
//! nothing reads one back.
//! What: `set` calls `secrets.scopes` then `secrets.set`; `list` calls
//! `secrets.scopes` then `secrets.list` per scope; `doctor` calls
//! `secrets.doctor`. Every line written names keys, scopes, backends or
//! paths. `set` alone prints part of a value: the server's `mask_secret`
//! confirmation.
//! Test: `tests.rs` beside this file.

use std::io::Write;

use anyhow::{anyhow, bail};
use serde_json::{Value, json};
use trusty_secrets::SecretValue;
use trusty_secrets::api::methods::{ListResponse, ScopesResponse, SetOutcome, SetResponse, method};
use trusty_secrets::server::{ClientError, DOCTOR, DoctorResponse};

use super::Ctx;
use super::session::{describe, entry_key, rpc_kind};

/// Refusal for a third positional argument to `set`.
const ARGV_REFUSED: &str =
    "tm secrets set: the value never goes on the command line; copy it to the clipboard";

/// Refusal for `--value` with anything but `-`.
const VALUE_FLAG_REFUSED: &str =
    "tm secrets set: `--value` accepts only `-` (read stdin); a value is never an argument";

/// The parsed `set` command line.
pub(super) struct SetArgs<'a> {
    pub(super) key: &'a str,
    pub(super) group: Option<&'a str>,
    /// `--value`'s text; only `-` is accepted.
    pub(super) value: Option<&'a str>,
    /// Whether a third positional argument was given.
    pub(super) extra: bool,
}

fn outcome_word(outcome: SetOutcome) -> &'static str {
    match outcome {
        SetOutcome::New => "new",
        SetOutcome::Updated => "updated",
    }
}

/// `set KEY [group]`: upsert one key of the project scope from the clipboard
/// or stdin.
///
/// Why: DOC-74 §15.3 gives the owner scope to the console; the CLI writes
/// the project scope only.
/// What: refuses argv values before reading anything; reads the source,
/// trims surrounding whitespace, refuses an empty result before any socket
/// call, then prints `(new|updated) secret set KEY: <mask_secret>`.
/// Test: `set_reads_the_clipboard_and_confirms_head_and_length_only`,
/// `set_with_a_short_value_shows_only_its_length`,
/// `set_with_an_empty_clipboard_is_an_error_and_stores_nothing`,
/// `set_refuses_a_value_on_the_command_line_without_echoing_it`.
pub(super) async fn set(
    ctx: &Ctx<'_>,
    args: SetArgs<'_>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    if args.extra {
        bail!(ARGV_REFUSED);
    }
    let key = entry_key(args.key, args.group)?;
    let (raw, empty) = match args.value {
        None => (ctx.clipboard.read()?, "the clipboard is empty"),
        Some("-") => (ctx.stdin.read()?, "stdin is empty"),
        Some(_) => bail!(VALUE_FLAG_REFUSED),
    };
    let value = SecretValue::new(raw.trim());
    drop(raw);
    if value.is_empty() {
        bail!("tm secrets set: {empty}; nothing was stored");
    }
    // #7521: project scope only; the owner scope is the console's (§15.3).
    let vault = ctx.project_vault().await?;
    let params = json!({ "vault": vault, "key": key, "value": value });
    let response: SetResponse = ctx.call(method::SET, params).await?;
    let word = outcome_word(response.outcome);
    writeln!(out, "({word}) secret set {key}: {}", response.masked)?;
    Ok(())
}

/// `list`: the key names of the project scope, then the owner scope.
///
/// Test: `list_prints_key_names_only`.
pub(super) async fn list(ctx: &Ctx<'_>, out: &mut dyn Write) -> anyhow::Result<()> {
    let scopes: ScopesResponse = ctx.call(method::SCOPES, Value::Null).await?;
    for scope in scopes.scopes {
        let listed: ListResponse = ctx
            .call(method::LIST, json!({ "vault": scope.vault }))
            .await?;
        writeln!(out, "{} ({:?}):", listed.vault, scope.kind)?;
        if listed.keys.is_empty() {
            writeln!(out, "  (no keys)")?;
        }
        for meta in listed.keys {
            writeln!(out, "  {}", meta.name)?;
        }
    }
    Ok(())
}

/// `doctor`: socket reachability, paths, and which backends this build opens.
///
/// What: asks with the project; when the project has no scopes (no checkout,
/// no remote) reports that and asks again without it. A socket that cannot
/// be started or reached exits 1 as `unreachable`; a server refusal exits 1
/// as `reachable` with the server's text; a selected backend that is
/// unavailable exits 1 after the backend table.
/// Test: `doctor_reports_socket_and_backends_without_values`,
/// `doctor_reports_a_refusing_server_as_reachable`,
/// `doctor_fails_when_the_selected_backend_is_unavailable`,
/// `every_verb_fails_without_a_value_when_the_socket_is_unreachable`.
pub(super) async fn doctor(ctx: &Ctx<'_>, out: &mut dyn Write) -> anyhow::Result<()> {
    let socket = ctx.client.socket();
    let mut answer = ctx.call_raw(DOCTOR, Value::Null).await?;
    if let Err(e) = &answer
        && rpc_kind(e).is_some_and(|kind| kind.starts_with("project_"))
    {
        writeln!(out, "project: {}", describe(e, socket))?;
        answer = ctx.client.call(DOCTOR, Value::Null).await;
    }
    let report = match answer {
        Ok(report) => report,
        Err(e) => {
            // #7521: only a server that answered can send an `Rpc` refusal.
            let state = match e {
                ClientError::Rpc(_) => "reachable",
                _ => "unreachable",
            };
            writeln!(out, "socket {}: {state}", socket.display())?;
            bail!(describe(&e, socket));
        }
    };
    let report: DoctorResponse = serde_json::from_value(report)
        .map_err(|_| anyhow!("tm secrets doctor: the answer did not decode"))?;
    writeln!(out, "socket {}: reachable", report.socket.display())?;
    writeln!(out, "index: {}", report.index_root.display())?;
    writeln!(out, "machine config: {}", report.machine_config.display())?;
    if let Some(root) = &report.project_root {
        writeln!(out, "project: {}", root.display())?;
    }
    writeln!(out, "selected backend: {}", report.selected_backend)?;
    let mut selected_ok = false;
    for backend in &report.backends {
        let state = if backend.available {
            format!("available [{}]", backend.capabilities.join(", "))
        } else {
            "unavailable".to_owned()
        };
        writeln!(out, "backend {}: {state}", backend.id)?;
        selected_ok |= backend.available && backend.id == report.selected_backend;
    }
    if !selected_ok {
        bail!(
            "tm secrets doctor: the selected backend `{}` is unavailable",
            report.selected_backend
        );
    }
    Ok(())
}
