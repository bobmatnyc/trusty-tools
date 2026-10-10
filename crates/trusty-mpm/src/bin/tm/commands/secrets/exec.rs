//! `tm secrets exec` — tm's side (#9621, DOC-74 §10.5, §15.8 tier 1).
//!
//! Why: the owner's "integrate with `gh` without exposing the password"
//! seam. Values reach the command's environment or stdin, never an argv and
//! never tm's own environment. The grant names the child's pid (Architect
//! ruling 2026-10-10), so tm spawns the child first and grants it second.
//! What: [`run`] in six steps.
//! 1. Spawn `tm secrets __exec-stub` in its own process group, with a token
//!    pipe and a report pipe inherited by fd number ([`exec_wire::stub_args`]).
//! 2. `secrets.grant` the stub's pid every key, for [`GRANT_TTL_SECS`].
//! 3. Write the token to the token pipe and close it. The stub waits on it,
//!    so it cannot resolve or exec before the grant exists.
//! 4. Read the stub's report: every value, or the failure. The stub execs
//!    the command in place after sending it, so the pid stays the grant's.
//! 5. Write the `--stdin` value; when stdout is not a terminal, pump stdout
//!    and stderr through [`Scrubber`]; wait, forwarding SIGINT, SIGTERM and
//!    SIGHUP to the group, and SIGKILL it on a second signal or after
//!    [`GRACE`].
//! 6. `secrets.revoke` the grant, take the terminal back, then drain the
//!    pumps; a signal during the drain (a terminal Ctrl-C included) goes to
//!    the group and ends tm with 128 + the signal.
//!
//! Every failure before the command runs kills the stub's group, revokes a
//! grant that exists, and exits 1 with fixed text. Known limits are in the
//! `exec` help and DOC-74 §10.5.
//! Test: `exec_tests.rs` (pure parts) and `tests/tm_secrets_exec.rs` (the
//! binary against a recording fake socket).

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use trusty_mpm::secrets_client;
use trusty_secrets::SecretKey;
use trusty_secrets::api::methods::{ExecGrantRequest, ExecGrantResponse, RevokeRequest, method};
use trusty_secrets::server::OnDemandSecrets;

use super::exec_proc::{Foreground, Signals, clear_cloexec, kill_group};
use super::exec_scrub::{Scrubber, pump};
use super::exec_wire::{self, ExecSpec, MAX_REPORT_BYTES, Report};

/// The lifetime tm asks for; the server caps it at its own maximum (3600 s
/// today). Architect ruling Q3: no refresh.
pub(crate) const GRANT_TTL_SECS: u64 = 3600;

/// How long a signalled command has before its group is killed.
const GRACE: Duration = Duration::from_secs(5);

/// How long tm waits for the stub's report: resolves may wait on a vault
/// unlock prompt.
const REPORT_WAIT: Duration = Duration::from_secs(300);

const PREFIX: &str = "tm secrets exec";

/// Run `spec` with its keys injected; the command's exit status.
///
/// Why/What: see the module doc.
/// Test: `exec_runs_the_command_with_the_value_and_revokes`,
/// `exec_grant_names_the_stub_pid_not_tm`,
/// `exec_kills_the_stub_group_when_the_grant_is_refused`,
/// `exec_drain_after_exit_answers_sigterm`,
/// `exec_drain_after_exit_answers_terminal_ctrl_c`.
pub(crate) async fn run(
    client: &OnDemandSecrets,
    project: &Path,
    spec: ExecSpec,
) -> anyhow::Result<i32> {
    let mut signals = Signals::install().map_err(|_| anyhow!("{PREFIX}: cannot watch signals"))?;
    let scrub = !std::io::stdout().is_terminal();
    let (token_r, token_w) = std::io::pipe()?;
    let (report_r, report_w) = std::io::pipe()?;
    let (token_fd, report_fd) = (token_r.as_raw_fd(), report_w.as_raw_fd());
    let program = std::env::current_exe().map_err(|_| anyhow!("{PREFIX}: cannot find tm"))?;
    let mut cmd = Command::new(program);
    cmd.args(exec_wire::stub_args(
        client.socket(),
        token_fd,
        report_fd,
        &spec,
    ))
    .process_group(0)
    .stdin(if spec.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::inherit()
    });
    if scrub {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    // SAFETY: the closure only calls `fcntl`, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            clear_cloexec(token_fd)?;
            clear_cloexec(report_fd)
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|_| anyhow!("{PREFIX}: cannot start the helper"))?;
    // #9621: only the stub holds these ends now, so its exit is our EOF.
    drop((token_r, report_w));
    let pid = child
        .id()
        .ok_or_else(|| anyhow!("{PREFIX}: the helper has no pid"))?;
    let terminal = Foreground::hand_to(pid);

    // #9621: the grant names the stub — the child pid — never tm's own.
    let granted = tokio::select! {
        granted = grant(client, project, pid, &spec) => granted,
        sig = signals.next() => {
            abort(&mut child, pid).await;
            return Ok(128 + sig);
        }
    };
    let token = match granted {
        Ok(token) => token,
        Err(e) => {
            abort(&mut child, pid).await;
            return Err(e);
        }
    };
    let supervised = supervise(
        &mut child,
        pid,
        &mut signals,
        &token,
        token_w,
        report_r,
        &spec,
        scrub,
    )
    .await;
    if supervised.is_err() {
        abort(&mut child, pid).await;
    }
    // #9621: every path that holds a token revokes it.
    revoke(client, &token).await;
    let (code, pumps) = supervised?;
    // #9621: the command has exited; a terminal Ctrl-C during the drain must
    // reach tm, not a background job that ignores SIGINT.
    drop(terminal);
    // #9621: a background job can hold the pipes past the command's exit;
    // a signal still ends tm. The group id stays reserved while that job is
    // in it; an empty group answers ESRCH, which kill_group ignores.
    if let Some(sig) = drain(pumps, &mut signals).await {
        kill_group(pid, sig);
        return Ok(128 + sig);
    }
    Ok(code)
}

/// Await every output pump; the first signal tm gets ends the wait early.
///
/// Why: a background job of the command can hold stdout open after the
/// command exits, and tm's handlers would swallow SIGINT, SIGTERM and SIGHUP
/// for as long as it does.
/// What: `None` once every pump reached end of stream; `Some(sig)` when a
/// signal came first. The pumps left running end with tm.
/// Test: `exec_drain_after_exit_answers_sigterm`,
/// `exec_drain_after_exit_answers_terminal_ctrl_c`.
async fn drain(pumps: Vec<JoinHandle<()>>, signals: &mut Signals) -> Option<i32> {
    let all = async {
        for pump in pumps {
            let _ = pump.await;
        }
    };
    tokio::select! {
        () = all => None,
        sig = signals.next() => Some(sig),
    }
}

/// `secrets.grant` every key of `spec` to `pid`; the token.
async fn grant(
    client: &OnDemandSecrets,
    project: &Path,
    pid: u32,
    spec: &ExecSpec,
) -> anyhow::Result<String> {
    let request = ExecGrantRequest::new(pid, spec.keys(), GRANT_TTL_SECS);
    let params =
        serde_json::to_value(request).map_err(|_| anyhow!("{PREFIX}: cannot encode the grant"))?;
    let params = secrets_client::with_project(project, params)
        .ok_or_else(|| anyhow!("{PREFIX}: the working directory is not UTF-8"))?;
    let answer = client.call(method::GRANT, params).await.map_err(|e| {
        anyhow!(
            "{}; nothing ran",
            secrets_client::describe(PREFIX, &e, client.socket())
        )
    })?;
    let granted: ExecGrantResponse = serde_json::from_value(answer)
        .map_err(|_| anyhow!("{PREFIX}: the grant answer did not decode; nothing ran"))?;
    if !exec_wire::is_token(&granted.token) {
        // A malformed token may still name a grant: revoke it, best effort.
        revoke(client, &granted.token).await;
        bail!("{PREFIX}: the grant token was malformed; nothing ran");
    }
    Ok(granted.token)
}

/// `secrets.revoke` `token`, best effort; a failure leaves the TTL.
async fn revoke(client: &OnDemandSecrets, token: &str) {
    let Ok(params) = serde_json::to_value(RevokeRequest::new(token)) else {
        return;
    };
    if client.call(method::REVOKE, params).await.is_err() {
        eprintln!("{PREFIX}: the grant could not be revoked; it expires at its TTL");
    }
}

/// Kill the stub's whole group and reap it.
async fn abort(child: &mut Child, pid: u32) {
    kill_group(pid, libc::SIGKILL);
    let _ = child.wait().await;
}

/// Steps 3–5 of the module doc: the command's status and the output pumps.
#[allow(clippy::too_many_arguments)]
async fn supervise(
    child: &mut Child,
    pid: u32,
    signals: &mut Signals,
    token: &str,
    token_w: std::io::PipeWriter,
    report_r: std::io::PipeReader,
    spec: &ExecSpec,
    scrub: bool,
) -> anyhow::Result<(i32, Vec<JoinHandle<()>>)> {
    // A dead stub makes this EPIPE (Rust ignores SIGPIPE), never a signal.
    let mut token_w = token_w;
    token_w
        .write_all(token.as_bytes())
        .map_err(|_| anyhow!("{PREFIX}: the helper exited before the grant; nothing ran"))?;
    drop(token_w);

    let report = tokio::select! {
        report = read_report(report_r) => report?,
        sig = signals.next() => bail!("{PREFIX}: stopped by signal {sig} before the command ran"),
        () = tokio::time::sleep(REPORT_WAIT) => bail!("{PREFIX}: the keys did not resolve in time; nothing ran"),
    };
    let values = match report {
        Report::Resolved(values) if values.keys().eq(spec.keys().iter()) => values,
        Report::Resolved(_) => bail!("{PREFIX}: the helper resolved the wrong keys; nothing ran"),
        Report::Failed {
            key: Some(key),
            reason,
        } => bail!("{PREFIX}: {key} did not resolve ({reason}); nothing ran"),
        Report::Failed { key: None, reason } => bail!("{PREFIX}: {reason}; nothing ran"),
    };

    let mut pumps = Vec::new();
    if let (Some(key), Some(mut stdin)) = (&spec.stdin, child.stdin.take()) {
        let value = values.get(key).cloned().unwrap_or_default();
        // A command that never reads stdin makes this EPIPE; that is its choice.
        pumps.push(tokio::spawn(async move {
            let _ = stdin.write_all(value.as_bytes()).await;
        }));
    }
    if scrub {
        start_pumps(child, &values, &mut pumps);
    }
    let status = wait_forwarding(child, pid, signals).await?;
    Ok((exit_code(status), pumps))
}

/// Read the stub's report to EOF, off the runtime.
async fn read_report(report_r: std::io::PipeReader) -> anyhow::Result<Report> {
    let bytes = tokio::task::spawn_blocking(move || {
        let mut bytes = Vec::new();
        report_r
            .take(MAX_REPORT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    })
    .await
    .map_err(|_| anyhow!("{PREFIX}: the report reader failed; nothing ran"))?
    .map_err(|_| anyhow!("{PREFIX}: the helper's report could not be read; nothing ran"))?;
    exec_wire::decode_report(&bytes)
        .ok_or_else(|| anyhow!("{PREFIX}: the helper exited before the command ran; nothing ran"))
}

/// Pump stdout and stderr through a [`Scrubber`] of every value.
fn start_pumps(
    child: &mut Child,
    values: &BTreeMap<SecretKey, String>,
    pumps: &mut Vec<JoinHandle<()>>,
) {
    let values: Vec<&String> = values.values().collect();
    if let Some(out) = child.stdout.take() {
        let scrubber = Scrubber::new(&values);
        pumps.push(tokio::spawn(async move {
            let _ = pump(out, tokio::io::stdout(), scrubber).await;
        }));
    }
    if let Some(err) = child.stderr.take() {
        let scrubber = Scrubber::new(&values);
        pumps.push(tokio::spawn(async move {
            let _ = pump(err, tokio::io::stderr(), scrubber).await;
        }));
    }
}

/// Wait for the command; forward each signal tm gets to its group, and
/// SIGKILL the group on a second signal or [`GRACE`] after the first.
async fn wait_forwarding(
    child: &mut Child,
    pid: u32,
    signals: &mut Signals,
) -> anyhow::Result<ExitStatus> {
    let mut grace: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    loop {
        tokio::select! {
            status = child.wait() => {
                return status.map_err(|_| anyhow!("{PREFIX}: lost track of the command"));
            }
            sig = signals.next() => {
                if grace.is_some() {
                    kill_group(pid, libc::SIGKILL);
                } else {
                    kill_group(pid, sig);
                    grace = Some(Box::pin(tokio::time::sleep(GRACE)));
                }
            }
            () = async { if let Some(g) = grace.as_mut() { g.await } }, if grace.is_some() => {
                kill_group(pid, libc::SIGKILL);
                grace = None;
            }
        }
    }
}

/// The shell convention: the exit code, or 128 + the signal that ended it.
pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}
