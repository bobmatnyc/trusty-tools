//! The `tm secrets` verbs, each one or a few `secrets.*` calls (#7521).
//!
//! Why: DOC-74 §9's grammar over the §15.2 methods. tm holds a value only
//! between reading it (clipboard, stdin or a dotenv file) and sending it in
//! `secrets.set`; nothing reads one back.
//! What: `set` calls `secrets.scopes` then `secrets.set`; `list` calls
//! `secrets.scopes` then `secrets.list` per scope; `remove` calls
//! `secrets.scopes` then `secrets.delete`; `import` parses the file with
//! `parse_dotenv`, then one `secrets.set` per entry; `copy` calls
//! `secrets.copy`; `doctor` calls `secrets.doctor`. Every line written names
//! keys, scopes, backends or paths. `set` alone prints part of a value: the
//! server's `mask_secret` confirmation.
//! Test: `tests.rs` beside this file.

use std::io::Write;
use std::path::Path;

use anyhow::{Context as _, anyhow, bail};
use serde_json::{Value, json};
use trusty_secrets::api::methods::{
    CopyResponse, DeleteResponse, ListResponse, ScopesResponse, SetOutcome, SetResponse, method,
};
use trusty_secrets::server::{ClientError, DOCTOR, DoctorResponse};
use trusty_secrets::store::parse_dotenv;
use trusty_secrets::{BackendId, SecretKey, SecretValue};

use super::Ctx;
use super::session::{check_group, describe, entry_key, is_project_refusal};

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
        // #7521: `SetOutcome` is `#[non_exhaustive]`.
        _ => "stored",
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

/// Key names joined for an error line.
fn names(keys: &[SecretKey]) -> String {
    keys.iter()
        .map(SecretKey::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `remove KEY [group]`: delete one key of the project scope.
///
/// Why: DOC-74 §15.3 — the CLI writes the project scope only, and a removal
/// is a write; a removal that matched nothing must not look like success.
/// What: `secrets.scopes` then `secrets.delete`; prints
/// `removed KEY from VAULT`. A key the project scope does not hold is an
/// error naming the key and the vault (exit 1), with nothing printed.
/// Test: `remove_deletes_a_project_key_and_fails_by_name_on_a_missing_one`.
pub(super) async fn remove(
    ctx: &Ctx<'_>,
    key: &str,
    group: Option<&str>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let key = entry_key(key, group)?;
    // #7521: project scope only; the owner scope is the console's (§15.3).
    let vault = ctx.project_vault().await?;
    let response: DeleteResponse = ctx
        .call(method::DELETE, json!({ "vault": vault, "key": key }))
        .await?;
    if !response.removed {
        bail!("tm secrets remove: {key} is not in {vault}");
    }
    writeln!(out, "removed {key} from {vault}")?;
    Ok(())
}

/// `import <path> [group]`: one project-scope `secrets.set` per dotenv entry.
///
/// Why: DOC-74 §9 — bulk-load a `.env` file without printing a value; the
/// file is left in place.
/// What: checks the group, then reads and parses the whole file with
/// `parse_dotenv` before any socket call, so a syntax error stores nothing;
/// its error names the line number and a fixed reason, never the line. Each
/// `secret://` entry is skipped and named. Each other entry is sent with
/// `secrets.set`; a key that fails is named with the server's fixed text and
/// the rest continue. Ends with a count line; any failed key makes the exit
/// status 1 and the error names it. Prints no value and no mask.
/// Test: `import_loads_a_dotenv_file_and_prints_names_only`,
/// `import_fails_by_name_on_a_refused_key_and_imports_the_rest`,
/// `import_syntax_error_names_the_line_not_its_text`,
/// `import_with_an_invalid_group_stores_nothing`.
pub(super) async fn import(
    ctx: &Ctx<'_>,
    path: &Path,
    group: Option<&str>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    check_group(group)?;
    let shown = path.display();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("tm secrets import: cannot read {shown}"))?;
    // #7521: `DotenvSyntax` carries the line number and a fixed reason only.
    let entries = parse_dotenv(&text).map_err(|e| anyhow!("tm secrets import: {shown}: {e}"))?;
    drop(text);
    // #7521: project scope only (§15.3).
    let vault = ctx.project_vault().await?;
    let (mut imported, mut skipped, mut failed) = (0usize, 0usize, Vec::new());
    for entry in &entries {
        let key = match entry_key(entry.name(), group) {
            Ok(key) => key,
            Err(e) => {
                writeln!(out, "failed {}: {e}", entry.name())?;
                failed.push(entry.name().to_owned());
                continue;
            }
        };
        if entry.is_reference() {
            writeln!(out, "skipped {key} (a secret:// reference)")?;
            skipped += 1;
            continue;
        }
        let value = SecretValue::new(entry.raw());
        let params = json!({ "vault": vault, "key": key, "value": value });
        match ctx.call::<SetResponse>(method::SET, params).await {
            Ok(response) => {
                writeln!(out, "imported {key} ({})", outcome_word(response.outcome))?;
                imported += 1;
            }
            Err(e) => {
                writeln!(out, "failed {key}: {e}")?;
                failed.push(key.to_string());
            }
        }
    }
    let count = failed.len();
    writeln!(
        out,
        "{vault}: {imported} imported, {skipped} skipped, {count} failed"
    )?;
    if count > 0 {
        bail!(
            "tm secrets import: {count} key(s) failed: {}",
            failed.join(", ")
        );
    }
    Ok(())
}

/// `copy --from A --to B [KEY...]`: copy this project's keys between backends.
///
/// Why: DOC-74 §9, §13 Q6 — the owner's "copy vars between stores", inside
/// one project only.
/// What: validates the backend ids and key names locally, then one
/// `secrets.copy`. The request names no vault, so the server copies inside
/// the caller's own project scope; no keys means every key of that scope.
/// Prints `copied KEY` and `not copied KEY` lines and a count line. A key
/// not copied — absent from the source, or refused by the destination —
/// makes the exit status 1 and the error names it. Values never reach tm.
/// Test: `copy_moves_project_keys_between_backends_without_printing_them`,
/// `copy_fails_by_name_on_a_missing_key`.
pub(super) async fn copy(
    ctx: &Ctx<'_>,
    from: &str,
    to: &str,
    keys: &[&str],
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let backend = |id: &str| BackendId::new(id).map_err(|e| anyhow!("tm secrets copy: {e}"));
    let (from, to) = (backend(from)?, backend(to)?);
    let keys = keys
        .iter()
        .map(|key| SecretKey::new(key).map_err(|e| anyhow!("tm secrets copy: {e}")))
        .collect::<anyhow::Result<Vec<_>>>()?;
    // #7521: no vault in the request — the server keeps the copy in-project.
    let params = json!({ "from_backend": from, "to_backend": to, "keys": keys });
    let response: CopyResponse = ctx.call(method::COPY, params).await?;
    for key in &response.copied {
        writeln!(out, "copied {key}")?;
    }
    for key in &response.failed {
        writeln!(out, "not copied {key}")?;
    }
    let (copied, failed) = (response.copied.len(), response.failed.len());
    writeln!(out, "{from} -> {to}: {copied} copied, {failed} not copied")?;
    if failed > 0 {
        bail!(
            "tm secrets copy: {failed} key(s) not copied (absent from {from}, or refused by {to}): {}",
            names(&response.failed)
        );
    }
    Ok(())
}

/// `doctor`: socket reachability, paths, and which backends this build opens.
///
/// What: asks with the project; when the server refuses the project (no
/// checkout, no remote, or a remote off github.com) reports that and asks
/// again without it. A socket that cannot
/// be started or reached exits 1 as `unreachable`; a server refusal exits 1
/// as `reachable` with the server's text; a selected backend that is
/// unavailable exits 1 after the backend table.
/// Test: `doctor_reports_socket_and_backends_without_values`,
/// `doctor_reports_backends_when_the_remote_is_off_github`,
/// `doctor_reports_a_refusing_server_as_reachable`,
/// `doctor_fails_when_the_selected_backend_is_unavailable`,
/// `every_verb_fails_without_a_value_when_the_socket_is_unreachable`.
pub(super) async fn doctor(ctx: &Ctx<'_>, out: &mut dyn Write) -> anyhow::Result<()> {
    let socket = ctx.client.socket();
    let mut answer = ctx.call_raw(DOCTOR, Value::Null).await?;
    if let Err(e) = &answer
        && is_project_refusal(e)
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
