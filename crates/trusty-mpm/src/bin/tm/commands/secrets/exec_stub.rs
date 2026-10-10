//! `tm secrets __exec-stub` — the child side of `tm secrets exec` (#9621).
//!
//! Why: the grant names a pid, and the value must reach the command without
//! crossing an argv or tm's environment. So the child tm grants is this
//! stub; it resolves with its own pid (the socket peer the server checks)
//! and then execs the command in place, keeping that pid.
//! What: [`run`]:
//! 1. Read the token from `--token-fd` to EOF, within [`TOKEN_WAIT`]. No
//!    token, or a malformed one, ends here: nothing resolves, nothing runs.
//! 2. `secrets.resolve` each key once. The first failure is reported and
//!    ends the stub: never a partial environment.
//! 3. Send every value to tm on `--report-fd` (tm's scrubber needs them,
//!    Architect ruling Q2) and close it.
//! 4. Put the token on a fresh pipe whose fd number goes in
//!    [`TOKEN_FD_ENV`] (tier 3, ruling Q1), then exec the command with each
//!    `--env` value set. tm writes the `--stdin` value.
//!
//! Every failure is a report frame with fixed text; tm prints it. An exec
//! failure, after the report, prints fixed text and exits 127.
//! Test: `tests/tm_secrets_exec.rs` — `exec_never_execs_before_the_token`,
//! `exec_fails_closed_when_a_key_does_not_resolve`,
//! `exec_fails_closed_on_a_malformed_token`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::time::Duration;

use trusty_mpm::secrets_client;
use trusty_secrets::SecretKey;
use trusty_secrets::api::methods::{ResolveRequest, ResolveResponse, method};
use trusty_secrets::server::OnDemandSecrets;

use super::exec_proc::clear_cloexec;
use super::exec_wire::{self, ExecSpec, TOKEN_FD_ENV};

/// How long the stub waits for tm to write the token.
const TOKEN_WAIT: Duration = Duration::from_secs(120);

/// The stub's inherited fds and what to run.
pub(crate) struct StubArgs {
    /// The socket tm granted on.
    pub(crate) socket: PathBuf,
    /// The token pipe's read end.
    pub(crate) token_fd: i32,
    /// The report pipe's write end.
    pub(crate) report_fd: i32,
    /// The keys and the command.
    pub(crate) spec: ExecSpec,
}

/// Take ownership of an inherited fd; `None` for stdio or a closed fd.
fn inherited(fd: i32) -> Option<File> {
    if fd <= 2 {
        return None;
    }
    // SAFETY: `fcntl(F_GETFD)` only reads the fd's flags.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return None;
    }
    // SAFETY: tm passed this fd for the stub alone, and nothing else in this
    // process owns it, so taking ownership here closes it exactly once.
    Some(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// Run the stub; returns only on failure, with the exit status to use.
///
/// Why/What: see the module doc.
pub(crate) async fn run(args: StubArgs) -> i32 {
    let Some(mut report) = inherited(args.report_fd) else {
        return 1;
    };
    let Some(token_pipe) = inherited(args.token_fd) else {
        return fail(&mut report, None, "the grant token pipe is missing");
    };
    let Some(token) = read_token(token_pipe).await else {
        return fail(&mut report, None, "no grant token arrived");
    };
    let client = OnDemandSecrets::at(&args.socket);
    let mut values: BTreeMap<SecretKey, String> = BTreeMap::new();
    for key in args.spec.keys() {
        match resolve(&client, &token, &key).await {
            Ok(value) => {
                values.insert(key, value);
            }
            Err(reason) => return fail(&mut report, Some(&key), &reason),
        }
    }
    if report.write_all(&exec_wire::encode_ok(&values)).is_err() {
        // tm is gone, or stopped listening: run nothing.
        return 1;
    }
    drop(report);
    let Some(token_fd) = token_for_command(&token) else {
        eprintln!("tm secrets exec: cannot pass the grant token on; the command did not start");
        return 1;
    };
    let spec = &args.spec;
    let mut command = std::process::Command::new(&spec.command[0]);
    command
        .args(&spec.command[1..])
        .env(TOKEN_FD_ENV, token_fd.to_string());
    for binding in &spec.env {
        if let Some(value) = values.get(&binding.key) {
            command.env(&binding.name, value);
        }
    }
    drop(values);
    // #9621: exec in place — the pid, and so the grant, stays the same.
    let error = command.exec();
    eprintln!(
        "tm secrets exec: the command did not start ({})",
        error.kind()
    );
    127
}

/// Report a failure to tm and return the stub's exit status.
fn fail(report: &mut File, key: Option<&SecretKey>, reason: &str) -> i32 {
    let _ = report.write_all(&exec_wire::encode_fail(key, reason));
    1
}

/// The token from `pipe`, read to EOF within [`TOKEN_WAIT`]; `None` when it
/// is absent, cut short or malformed.
async fn read_token(pipe: File) -> Option<String> {
    let read = tokio::task::spawn_blocking(move || {
        let mut text = String::new();
        pipe.take(256).read_to_string(&mut text).map(|_| text)
    });
    let text = tokio::time::timeout(TOKEN_WAIT, read)
        .await
        .ok()?
        .ok()?
        .ok()?;
    exec_wire::is_token(&text).then_some(text)
}

/// One `secrets.resolve`; the value, or fixed text.
async fn resolve(client: &OnDemandSecrets, token: &str, key: &SecretKey) -> Result<String, String> {
    let params = serde_json::to_value(ResolveRequest::new(token, key.clone()))
        .map_err(|_| "the request did not encode".to_owned())?;
    let answer = client
        .call(method::RESOLVE, params)
        .await
        .map_err(|e| secrets_client::describe("refused", &e, client.socket()))?;
    let resolved: ResolveResponse =
        serde_json::from_value(answer).map_err(|_| "the answer did not decode".to_owned())?;
    if resolved.key != *key {
        return Err("the answer named another key".to_owned());
    }
    Ok(resolved.value.expose().to_owned())
}

/// A fresh pipe holding `token`, its read end kept across exec; its fd.
///
/// What: the token is 64 bytes, under the pipe buffer, so the write never
/// blocks; the write end is closed, so the command reads the token to EOF.
fn token_for_command(token: &str) -> Option<i32> {
    let (reader, mut writer) = std::io::pipe().ok()?;
    writer.write_all(token.as_bytes()).ok()?;
    drop(writer);
    let fd = reader.into_raw_fd();
    clear_cloexec(fd).ok()?;
    Some(fd)
}
