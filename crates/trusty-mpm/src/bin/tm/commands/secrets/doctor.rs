//! `tm secrets doctor`: one `secrets.doctor` call, rendered (#7521, #7519 P4).
//!
//! Why: DOC-74 §9 — doctor flags a configured backend that cannot work and
//! says why, so the operator knows whether to rebuild, edit the account's
//! machine config, install a CLI or fix a tracked file (#7519 A4).
//! What: [`doctor`] asks the server (`ask`), writes the report (`render`)
//! and decides the exit status (`verdict`). Every line names paths, ids,
//! reasons and fixed hints; the server never sends a value or a token.
//! Test: `tests.rs` beside this file — the `doctor_*` tests.

use std::io::Write;

use anyhow::{anyhow, bail};
use serde_json::Value;
use trusty_secrets::BackendId;
use trusty_secrets::server::{BackendStatus, ClientError, DOCTOR, DoctorResponse, StoragePosture};

use super::Ctx;
use super::session::{describe, is_project_refusal};

/// `doctor`: socket reachability, paths, and which backends the server
/// opens, with the reason for each one it does not.
///
/// What: asks with the project; when the server refuses the project (no
/// checkout, no remote, or a remote off github.com) reports that, asks
/// again without it, writes the machine default's report, and exits 1
/// (#7519 d4 Q4). A socket that cannot be started or reached exits 1 as
/// `unreachable`; a server refusal exits 1 as `reachable` with the server's
/// text; a selected backend that is unavailable exits 1 after the table;
/// under `CI=true`, so does a selected 1Password with no service-account
/// token at server start (#7519 d4 Q2).
/// Test: `doctor_reports_socket_and_backends_without_values`,
/// `doctor_reports_backends_when_the_remote_is_off_github`,
/// `doctor_reports_a_refusing_server_as_reachable`,
/// `doctor_fails_when_the_selected_backend_is_unavailable`,
/// `doctor_renders_reasons_posture_and_headless_readiness`,
/// `doctor_under_ci_fails_when_onepassword_has_no_headless_credential`,
/// `every_verb_fails_without_a_value_when_the_socket_is_unreachable`.
pub(super) async fn doctor(ctx: &Ctx<'_>, out: &mut dyn Write) -> anyhow::Result<()> {
    let (report, project) = ask(ctx, out).await?;
    render(&report, out)?;
    verdict(&report, project, ctx.ci)
}

/// Whether doctor judged the project the caller is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Judged {
    /// The report covers the project, or no project was asked about.
    Project,
    /// The server refused the project; the report is the machine default's.
    MachineDefault,
}

/// Whether a `CI` value marks a CI run: `true` or `1`, in any case.
pub(super) fn is_ci(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1")
}

/// The doctor report, asked with the project and, when the server refuses
/// the project, again without it.
///
/// What: the refusal is written as a `project:` line before the retry, and
/// the report comes back as [`Judged::MachineDefault`].
async fn ask(ctx: &Ctx<'_>, out: &mut dyn Write) -> anyhow::Result<(DoctorResponse, Judged)> {
    let socket = ctx.client.socket();
    let mut judged = Judged::Project;
    let mut answer = ctx.call_raw(DOCTOR, Value::Null).await?;
    if let Err(e) = &answer
        && is_project_refusal(e)
    {
        writeln!(out, "project: {}", describe(e, socket))?;
        judged = Judged::MachineDefault;
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
    let report = serde_json::from_value(report)
        .map_err(|_| anyhow!("tm secrets doctor: the answer did not decode"))?;
    Ok((report, judged))
}

/// Write the report: paths, the selection and its posture, one line per
/// backend, headless readiness, then DOC-74 §7's unsupported tools: one line
/// per installed tool and one listing the rest.
pub(super) fn render(report: &DoctorResponse, out: &mut dyn Write) -> anyhow::Result<()> {
    writeln!(out, "socket {}: reachable", report.socket.display())?;
    writeln!(out, "index: {}", report.index_root.display())?;
    writeln!(out, "machine config: {}", report.machine_config.display())?;
    // #7519 P4: the one file that enables a CLI backend (ruling 74).
    if let Some(account) = &report.account_config {
        writeln!(out, "account machine config: {}", account.display())?;
    }
    if let Some(root) = &report.project_root {
        writeln!(out, "project: {}", root.display())?;
    }
    writeln!(out, "selected backend: {}", report.selected_backend)?;
    if let Some(posture) = report.posture {
        writeln!(out, "posture: {}", posture_text(posture))?;
    }
    let token = report.headless.map(|h| h.onepassword_token);
    for backend in &report.backends {
        writeln!(out, "backend {}: {}", backend.id, row_text(backend, token))?;
    }
    if let Some(token) = token {
        let present = if token { "yes" } else { "no" };
        writeln!(
            out,
            "headless: 1Password service-account token at server start: {present}"
        )?;
    }
    // #7519 P4: DOC-74 §7 — report-only; trusty-secrets has no backend for these.
    let mut absent = Vec::new();
    for tool in &report.tools {
        match &tool.path {
            Some(path) if tool.installed => writeln!(
                out,
                "tool {} ({}): installed at {}; {}",
                tool.id,
                tool.program,
                path.display(),
                if tool.supported {
                    "supported"
                } else {
                    "no trusty-secrets backend"
                }
            )?,
            _ => absent.push(tool.program.as_str()),
        }
    }
    if !absent.is_empty() {
        writeln!(out, "tools not installed: {}", absent.join(", "))?;
    }
    Ok(())
}

fn posture_text(posture: StoragePosture) -> &'static str {
    match posture {
        StoragePosture::Keychain => "keychain",
        // #9326: ruling f5 — the degraded posture is always said so.
        StoragePosture::FileDegraded => "file_degraded (values are plaintext 0600 files)",
        _ => "other",
    }
}

/// One backend's state: its capabilities, or the reason and fix.
///
/// What: an available 1Password row with no token at server start says it
/// needs an unlocked app or a session, so it never reads as plain "ok"
/// (#7519 A4); doctor never spawns `op` to learn more.
fn row_text(backend: &BackendStatus, onepassword_token: Option<bool>) -> String {
    if !backend.available {
        return match (backend.reason, &backend.detail) {
            (Some(reason), Some(detail)) => format!("unavailable ({}): {detail}", reason.as_str()),
            (Some(reason), None) => format!("unavailable ({})", reason.as_str()),
            _ => "unavailable".to_owned(),
        };
    }
    let mut text = format!("available [{}]", backend.capabilities.join(", "));
    if backend.id == BackendId::onepassword() && onepassword_token == Some(false) {
        text.push_str(
            "; no service-account token, so it needs an unlocked 1Password app or an \
             `op signin` session",
        );
    }
    text
}

/// The exit status, judged after the report is written.
///
/// What, in order, each an error:
/// 1. the selected backend is unavailable;
/// 2. #7519 d4 Q4 (owner ruling 2026-10-07): the server refused the
///    project, so the report is the machine default's, not the project's;
/// 3. #7519 d4 Q2 (owner ruling 2026-10-07): a CI run (`ci`) whose selected
///    backend is 1Password with no service-account token at server start,
///    or a server too old to say. Keeper's device approval and the Keychain
///    are not judged: doctor cannot detect either without a spawn.
///
/// Test: `doctor_fails_when_the_selected_backend_is_unavailable`,
/// `doctor_reports_backends_when_the_remote_is_off_github`,
/// `doctor_under_ci_fails_when_onepassword_has_no_headless_credential`.
pub(super) fn verdict(report: &DoctorResponse, judged: Judged, ci: bool) -> anyhow::Result<()> {
    let selected = report
        .backends
        .iter()
        .any(|b| b.available && b.id == report.selected_backend);
    if !selected {
        bail!(
            "tm secrets doctor: the selected backend `{}` is unavailable",
            report.selected_backend
        );
    }
    if judged == Judged::MachineDefault {
        bail!(
            "tm secrets doctor: the project was refused, so the report above is the machine \
             default's; fix the `project:` line above"
        );
    }
    let token = report.headless.is_some_and(|h| h.onepassword_token);
    if ci && report.selected_backend == BackendId::onepassword() && !token {
        bail!(
            "tm secrets doctor: CI=true and the selected backend `onepassword` has no headless \
             credential: OP_SERVICE_ACCOUNT_TOKEN was not set where the secrets server started"
        );
    }
    Ok(())
}
