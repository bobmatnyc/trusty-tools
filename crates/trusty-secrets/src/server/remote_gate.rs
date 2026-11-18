//! `trusty-secrets remote-gate --host <name>`: the ssh forced-command front
//! for a remote host (#11464, epic #11463 phase 1).
//!
//! Why: a remote host's agent cannot use the login Keychain through an
//! ssh-spawned server (#10589), so the gate never starts one. It relays one
//! call to the server already running in the GUI session, as the same user,
//! and fails closed when none answers.
//! What: [`run_remote_gate`] reads one JSON frame (one line, at most
//! [`MAX_FRAME_BYTES`]) from `input` and writes one JSON line to `output`:
//! 1. the frame's `method` must be `resolve` or `doctor`, else exit
//!    [`EXIT_REFUSED`] and nothing is sent;
//! 2. the host's entry in the account machine config must list the frame's
//!    `<owner>/<repo>`, else [`EXIT_REFUSED`] before any relay;
//! 3. the call is relayed as `secrets.remote.*` with `host` from `--host`,
//!    never from the frame. No dial, a server refusal of availability, or
//!    [`GATE_DEADLINE`] passing is [`EXIT_UNAVAILABLE`]; a server refusal of
//!    policy is [`EXIT_REFUSED`].
//!
//! Every failure reply is `{"ok": false, "error": "<kind>"}` with a fixed
//! kind. A value reaches `output` only on success, and stderr never.
//! Test: `remote_tests.rs` beside `wire_tests.rs`, `tests/remote_gate_bin.rs`.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};

use super::errors::ErrorKind;
use super::remote::{host_allows, parse_project, valid_label};
use super::settings::SOCKET_SUBPATH;
use crate::api::SecretKey;
use crate::api::methods::method;
use crate::store::config::{MACHINE_CONFIG_SUBPATH, load_machine_at};
use crate::store::platform;

/// The subcommand name.
pub const GATE_SUBCOMMAND: &str = "remote-gate";

/// Exit status of a refused call: a bad frame, an unlisted host or
/// project, or a server policy refusal.
pub const EXIT_REFUSED: u8 = 13;

/// Exit status when no server answered in time (`EX_UNAVAILABLE`).
pub const EXIT_UNAVAILABLE: u8 = 69;

/// Exit status of a bad command line (`EX_USAGE`).
pub const EXIT_USAGE: u8 = 64;

/// The whole call's deadline, frame read and relay included.
pub const GATE_DEADLINE: Duration = Duration::from_secs(10);

/// The largest frame read from `input`.
pub const MAX_FRAME_BYTES: usize = 16 * 1024;

/// Where the gate reads its allowlist and dials, and for how long.
///
/// What: built by [`GateConfig::from_args`]; tests build it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateConfig {
    /// The `--host` name, as `authorized_keys` pins it.
    pub host: String,
    /// The GUI-session server's socket.
    pub socket: PathBuf,
    /// The account machine config holding `secrets.remote_hosts`.
    pub machine_config: PathBuf,
    /// The whole call's deadline.
    pub deadline: Duration,
}

impl GateConfig {
    /// Parse `remote-gate --host <name> [--socket P] [--machine-config P]`.
    ///
    /// What: both paths default under `account_home` (the password
    /// database's home, never `$HOME`). The server re-checks the allowlist
    /// in the account's own file whatever `--machine-config` says.
    /// Test: `remote_gate_args_table`.
    pub fn from_args(
        args: impl IntoIterator<Item = OsString>,
        account_home: Option<PathBuf>,
    ) -> Result<Self, &'static str> {
        let mut args = args.into_iter();
        if args.next().as_deref() != Some(GATE_SUBCOMMAND.as_ref()) {
            return Err("expected `remote-gate`");
        }
        let (mut host, mut socket, mut machine) = (None, None, None);
        while let Some(flag) = args.next() {
            let value = args.next().ok_or("a flag is missing its value")?;
            match flag.to_str() {
                Some("--host") => host = Some(value.into_string().map_err(|_| "bad --host")?),
                Some("--socket") => socket = Some(PathBuf::from(value)),
                Some("--machine-config") => machine = Some(PathBuf::from(value)),
                _ => return Err("unknown flag"),
            }
        }
        let host = host
            .filter(|h| valid_label(h))
            .ok_or("--host is missing or invalid")?;
        let home = || account_home.clone().ok_or("the account home is unknown");
        Ok(Self {
            host,
            socket: match socket {
                Some(path) => path,
                None => home()?.join(SOCKET_SUBPATH),
            },
            machine_config: match machine {
                Some(path) => path,
                None => home()?.join(MACHINE_CONFIG_SUBPATH),
            },
            deadline: GATE_DEADLINE,
        })
    }
}

/// One frame from the remote host.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    method: String,
    project: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    agent: Option<String>,
}

/// The gate's answer: the reply line and the exit status.
#[derive(Debug, Clone, PartialEq)]
pub struct GateReply {
    /// The one JSON line written to `output`.
    pub line: Value,
    /// The process exit status.
    pub status: u8,
}

impl GateReply {
    fn fail(status: u8, kind: &str) -> Self {
        Self {
            line: json!({"ok": false, "error": kind}),
            status,
        }
    }

    fn refused(kind: &str) -> Self {
        Self::fail(EXIT_REFUSED, kind)
    }

    fn unavailable(kind: &str) -> Self {
        Self::fail(EXIT_UNAVAILABLE, kind)
    }
}

/// Run the gate over `input` and `output`; returns the exit status.
///
/// What: see the module docs. The frame is read on its own thread, so an
/// `input` that never ends still answers at the deadline.
/// Test: `remote_gate_refuses_an_unlisted_verb_and_sends_nothing`,
/// `remote_gate_with_a_silent_server_answers_within_the_deadline`.
pub async fn run_remote_gate(
    config: &GateConfig,
    input: impl Read + Send + 'static,
    output: &mut impl Write,
) -> u8 {
    let reply = gate_reply(config, input).await;
    let mut line = reply.line.to_string();
    line.push('\n');
    // A reply that cannot be written changes nothing the server did.
    let _ = output
        .write_all(line.as_bytes())
        .and_then(|()| output.flush());
    reply.status
}

/// [`run_remote_gate`]'s decision, before it is written.
pub async fn gate_reply(config: &GateConfig, input: impl Read + Send + 'static) -> GateReply {
    let until = tokio::time::Instant::now() + config.deadline;
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_frame(input));
    });
    let frame = match tokio::time::timeout_at(until, rx).await {
        Ok(Ok(Some(frame))) => frame,
        Ok(Ok(None) | Err(_)) => return GateReply::refused("invalid_frame"),
        Err(_) => return GateReply::unavailable("deadline_exceeded"),
    };
    let (rpc_method, params) = match decide(config, &frame) {
        Ok(call) => call,
        Err(reply) => return reply,
    };
    let request =
        json!({"jsonrpc": JSONRPC_VERSION, "id": 1, "method": rpc_method, "params": params});
    // The outer `timeout_at` is the bound; the inner one only backs it up,
    // so a silent server reads as `deadline_exceeded`.
    let remaining =
        until.saturating_duration_since(tokio::time::Instant::now()) + Duration::from_secs(1);
    // #11464: dial only; never spawn a server (#10589).
    let sent = tokio::time::timeout_at(
        until,
        send_framed_request(&config.socket, &request, remaining),
    );
    match sent.await {
        Err(_) => GateReply::unavailable("deadline_exceeded"),
        Ok(Err(_)) => GateReply::unavailable("server_unavailable"),
        Ok(Ok(response)) => answer(response),
    }
}

/// Read one line of at most [`MAX_FRAME_BYTES`]; `None` when over it,
/// unreadable, or not UTF-8.
fn read_frame(input: impl Read) -> Option<String> {
    let mut bytes = Vec::new();
    let limit = u64::try_from(MAX_FRAME_BYTES + 1).ok()?;
    let mut limited = input.take(limit);
    let mut byte = [0u8; 1];
    loop {
        match limited.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => bytes.push(byte[0]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    if bytes.len() > MAX_FRAME_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Check the frame and the allowlist; the relay's method and params.
///
/// What: steps 1 and 2 of the module docs, in that order, before anything
/// is dialled. An unreadable config refuses.
/// Test: `remote_gate_refuses_an_unlisted_verb_and_sends_nothing`.
fn decide(config: &GateConfig, frame: &str) -> Result<(&'static str, Value), GateReply> {
    let frame: Frame =
        serde_json::from_str(frame).map_err(|_| GateReply::refused("invalid_frame"))?;
    // #11464 AC1: an allowlist of verbs, never a deny list.
    let rpc_method = match frame.method.as_str() {
        "resolve" => method::REMOTE_RESOLVE,
        "doctor" => method::REMOTE_DOCTOR,
        _ => return Err(GateReply::refused("method_refused")),
    };
    let invalid = || GateReply::refused("invalid_frame");
    let (owner, repo) = parse_project(&frame.project).ok_or_else(invalid)?;
    if frame
        .agent
        .as_deref()
        .is_some_and(|label| !valid_label(label))
    {
        return Err(invalid());
    }
    let key = match (rpc_method, frame.key.as_deref()) {
        (method::REMOTE_RESOLVE, Some(key)) => Some(SecretKey::new(key).map_err(|_| invalid())?),
        (method::REMOTE_RESOLVE, None) => return Err(invalid()),
        (_, Some(_)) => return Err(invalid()),
        (_, None) => None,
    };
    // #11464 AC2: an unknown host or unlisted project goes no further.
    let refused = || GateReply::refused(ErrorKind::RemoteRefused.as_str());
    let machine = load_machine_at(&config.machine_config)
        .ok()
        .flatten()
        .ok_or_else(refused)?;
    if !host_allows(&machine, &config.host, &owner, &repo) {
        return Err(refused());
    }
    let mut params = json!({"host": config.host, "project": format!("{owner}/{repo}")});
    if let Some(key) = key {
        params["key"] = json!(key);
    }
    if let Some(agent) = frame.agent {
        params["agent"] = json!(agent);
    }
    Ok((rpc_method, params))
}

/// The server's response as the gate's reply.
///
/// What: a result is `{"ok": true, "result": …}` and exit 0. An error whose
/// kind says the server could not serve now (a backend, the audit log, a
/// deadline, or no kind at all, e.g. an older server without the method)
/// is [`EXIT_UNAVAILABLE`]; every other kind is [`EXIT_REFUSED`].
/// Test: `remote_gate_answer_table`.
pub(crate) fn answer(response: RpcResponse) -> GateReply {
    match (response.result, response.error) {
        (_, Some(error)) => {
            let kind = error
                .data
                .as_ref()
                .and_then(|data| data.get("kind"))
                .and_then(Value::as_str)
                .and_then(ErrorKind::from_wire);
            match kind {
                None => GateReply::unavailable("server_unavailable"),
                Some(kind) if unavailable(kind) => GateReply::unavailable(kind.as_str()),
                Some(kind) => GateReply::refused(kind.as_str()),
            }
        }
        (Some(result), None) => GateReply {
            line: json!({"ok": true, "result": result}),
            status: 0,
        },
        (None, None) => GateReply::unavailable("server_unavailable"),
    }
}

/// Whether `kind` means "not now" rather than "not allowed".
fn unavailable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::BackendFailed
            | ErrorKind::BackendTimeout
            | ErrorKind::BackendLocked
            | ErrorKind::DeadlineExceeded
            | ErrorKind::AuditUnavailable
            | ErrorKind::StorageUnavailable
            | ErrorKind::IndexBusy
            | ErrorKind::HomeUnavailable
            | ErrorKind::Internal
    )
}

/// The binary's entry: parse `args`, run the gate on stdin and stdout.
///
/// What: a bad command line is [`EXIT_USAGE`] with one fixed line on
/// stderr; nothing else ever reaches stderr.
/// Test: `remote_gate_with_no_server_exits_69_and_spawns_nothing`.
pub async fn run_from_args(args: impl IntoIterator<Item = OsString>) -> u8 {
    let account_home = platform::account_home_dir().ok();
    let config = match GateConfig::from_args(args, account_home) {
        Ok(config) => config,
        Err(reason) => {
            eprintln!("trusty-secrets remote-gate: {reason}");
            return EXIT_USAGE;
        }
    };
    let mut stdout = std::io::stdout();
    run_remote_gate(&config, std::io::stdin(), &mut stdout).await
}
