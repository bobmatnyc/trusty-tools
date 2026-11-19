//! `trusty-secrets remote-gate --host <name>`: the ssh forced-command front
//! for a remote host (#11464, epic #11463 phase 1, owner ruling D3a).
//!
//! Why: a remote host's agent cannot use the login Keychain through an
//! ssh-spawned server (#10589), so the gate never starts one. It relays one
//! call to the server already running in the GUI session, as the same user,
//! and fails closed when none answers.
//! What: [`run_remote_gate`] reads one JSON frame (one line, at most
//! [`MAX_FRAME_BYTES`], within [`FRAME_READ_TIMEOUT`]) from `input` and
//! writes one JSON line to `output`. `SSH_ORIGINAL_COMMAND` is never read.
//! 1. The frame is parsed into a typed struct (an unknown field, a
//!    duplicate key, a second value or a second line refuses) and its
//!    `method` must be exactly `resolve` or `doctor`. Every refusal is
//!    [`EXIT_REFUSED`] and nothing is sent.
//! 2. The host's entry in the account machine config must list keys, map
//!    the frame's `<owner>/<repo>` to a checkout whose `origin` names that
//!    repository, and for `resolve` list the key. Else [`EXIT_REFUSED`],
//!    nothing sent.
//! 3. `resolve`: a one-shot `secrets.grant` of that key to the gate's own
//!    pid in that checkout, then `secrets.gate` `resolve` with its token and
//!    `host` from `--host` (never from the frame), then a `secrets.revoke`
//!    when it failed. `doctor`: `secrets.gate` `doctor`. The grant is an
//!    internal step: no frame names it and the token never leaves.
//!
//! Exit [`EXIT_UNAVAILABLE`] only when no dial connects; every timeout and
//! every server refusal is [`EXIT_REFUSED`]. Every failure reply is
//! `{"ok": false, "error": "<kind>"}` with a fixed kind. A value reaches
//! `output` only on success, and stderr never.
//! Test: `remote_tests.rs` beside `wire_tests.rs`, `tests/remote_gate_bin.rs`.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::Instant;
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};

use super::errors::ErrorKind;
use super::remote::{parse_project, valid_label};
use super::settings::SOCKET_SUBPATH;
use crate::api::methods::method;
use crate::api::{OwnerName, RepoName, SecretKey};
use crate::store::config::{MACHINE_CONFIG_SUBPATH, RemoteHostConfig, load_machine_at};
use crate::store::{parse_remote_identity, platform};

/// The subcommand name.
pub const GATE_SUBCOMMAND: &str = "remote-gate";

/// Exit status of a refused call: a bad frame, an unlisted host, project or
/// key, a timeout, or any server refusal.
pub const EXIT_REFUSED: u8 = 13;

/// Exit status when no server socket could be dialled (`EX_UNAVAILABLE`).
pub const EXIT_UNAVAILABLE: u8 = 69;

/// Exit status of a bad command line (`EX_USAGE`).
pub const EXIT_USAGE: u8 = 64;

/// The whole call's deadline, frame read and relay included.
pub const GATE_DEADLINE: Duration = Duration::from_secs(10);

/// The longest the frame read may take.
pub const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest frame read from `input`, its newline excluded.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// The lifetime the gate asks for its one-shot grant.
const GRANT_TTL_SECS: u64 = 30;

/// The fixed kind of a server socket that could not be dialled.
pub const SERVER_ABSENT: &str = "studio_server_absent";

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

/// What a checked frame asks the server for.
enum Plan {
    /// Grant, then resolve `key` in the checkout `dir`.
    Resolve {
        dir: PathBuf,
        key: SecretKey,
        project: String,
        agent: Option<String>,
    },
    /// The server's preflight.
    Doctor,
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
    fn refused(kind: &str) -> Self {
        Self {
            line: json!({"ok": false, "error": kind}),
            status: EXIT_REFUSED,
        }
    }
}

/// Run the gate over `input` and `output`; returns the exit status.
///
/// What: see the module docs. The frame is read on its own thread, so an
/// `input` that never ends still answers at the read timeout.
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
    let until = Instant::now() + config.deadline;
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_frame(input));
    });
    let read_until = until.min(Instant::now() + FRAME_READ_TIMEOUT);
    let frame = match tokio::time::timeout_at(read_until, rx).await {
        Ok(Ok(Some(frame))) => frame,
        Ok(Ok(None) | Err(_)) => return GateReply::refused("invalid_frame"),
        Err(_) => return GateReply::refused(ErrorKind::DeadlineExceeded.as_str()),
    };
    match decide(config, &frame) {
        Ok(Plan::Doctor) => {
            let params = json!({"verb": "doctor"});
            match relay(config, until, method::GATE, params).await {
                Ok(response) => answer(response),
                Err(reply) => reply,
            }
        }
        Ok(Plan::Resolve {
            dir,
            key,
            project,
            agent,
        }) => resolve(config, until, &dir, &key, &project, agent).await,
        Err(reply) => reply,
    }
}

/// Grant, resolve through `secrets.gate`, revoke on failure.
///
/// What: the grant is the gate's own registration, which the server judges
/// a person's under sshd, so its record is an allow even for a key denied
/// to agents; the deny happens at `secrets.gate` `resolve`, where the
/// server forces `agent_parent` (#11464 critic LOW). A grant refusal goes
/// no further.
/// Test: `remote_gate_relays_to_the_running_server`.
async fn resolve(
    config: &GateConfig,
    until: Instant,
    dir: &Path,
    key: &SecretKey,
    project: &str,
    agent: Option<String>,
) -> GateReply {
    let grant = json!({
        "project": dir,
        "child_pid": std::process::id(),
        "keys": [key],
        "ttl_secs": GRANT_TTL_SECS,
        "one_shot": true,
    });
    let token = match relay(config, until, method::GRANT, grant).await {
        Ok(RpcResponse {
            result: Some(result),
            error: None,
            ..
        }) => match result.get("token").and_then(Value::as_str) {
            Some(token) => token.to_owned(),
            None => return GateReply::refused(ErrorKind::Internal.as_str()),
        },
        Ok(response) => return answer(response),
        Err(reply) => return reply,
    };
    let mut params = json!({
        "verb": "resolve",
        "token": token,
        "key": key,
        "host": config.host,
        "project": project,
    });
    if let Some(agent) = agent {
        params["agent"] = json!(agent);
    }
    let reply = match relay(config, until, method::GATE, params).await {
        Ok(response) => answer(response),
        Err(reply) => reply,
    };
    // A success spent the one-shot grant; a failure would leave it until
    // its TTL. Best-effort, inside the deadline.
    if reply.status != 0 {
        let _ = relay(config, until, method::REVOKE, json!({"token": token})).await;
    }
    reply
}

/// One request to the server, bounded by `until`; dial only, never spawn.
///
/// What: a dial failure is [`EXIT_UNAVAILABLE`] ([`SERVER_ABSENT`]); the
/// deadline passing and every other transport failure are [`EXIT_REFUSED`].
/// Test: `remote_gate_with_no_server_exits_unavailable`,
/// `remote_gate_with_a_silent_server_answers_within_the_deadline`.
async fn relay(
    config: &GateConfig,
    until: Instant,
    rpc_method: &str,
    params: Value,
) -> Result<RpcResponse, GateReply> {
    let request =
        json!({"jsonrpc": JSONRPC_VERSION, "id": 1, "method": rpc_method, "params": params});
    // The outer `timeout_at` is the bound; the inner one only backs it up.
    let remaining = until.saturating_duration_since(Instant::now()) + Duration::from_secs(1);
    // #11464: dial only; never spawn a server (#10589).
    let sent = tokio::time::timeout_at(
        until,
        send_framed_request::<_, RpcResponse>(&config.socket, &request, remaining),
    );
    match sent.await {
        Err(_) => Err(GateReply::refused(ErrorKind::DeadlineExceeded.as_str())),
        Ok(Err(e)) if e.is_dial_failure() => Err(GateReply {
            line: json!({"ok": false, "error": SERVER_ABSENT}),
            status: EXIT_UNAVAILABLE,
        }),
        Ok(Err(_)) => Err(GateReply::refused(ErrorKind::BackendFailed.as_str())),
        Ok(Ok(response)) => Ok(response),
    }
}

/// Read `input` to its end, at most [`MAX_FRAME_BYTES`] plus a newline;
/// `None` when over it, unreadable or not UTF-8. A second frame is a
/// second JSON value, which the typed parse refuses.
fn read_frame(input: impl Read) -> Option<String> {
    let mut bytes = Vec::new();
    let limit = u64::try_from(MAX_FRAME_BYTES + 2).ok()?;
    input.take(limit).read_to_end(&mut bytes).ok()?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.len() > MAX_FRAME_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Check the frame and the allowlist; what to ask the server.
///
/// What: steps 1 and 2 of the module docs, in that order, before anything
/// is dialled. An unreadable config, an unknown or disabled host, an empty
/// key list, an unlisted key, an unmapped project or a mapped dir that is
/// not that repository's checkout all refuse with `remote_refused`.
/// Test: `remote_gate_refuses_an_unlisted_verb_and_sends_nothing`,
/// `remote_gate_config_failures_refuse_and_send_nothing`,
/// `remote_gate_without_a_matching_checkout_refuses_and_sends_nothing`.
fn decide(config: &GateConfig, frame: &str) -> Result<Plan, GateReply> {
    let invalid = || GateReply::refused("invalid_frame");
    let frame: Frame = serde_json::from_str(frame).map_err(|_| invalid())?;
    // #11464 AC1: an allowlist of verbs, never a deny list; exact case.
    let resolving = match frame.method.as_str() {
        "resolve" => true,
        "doctor" => false,
        _ => return Err(GateReply::refused("method_refused")),
    };
    let (owner, repo) = parse_project(&frame.project).ok_or_else(invalid)?;
    if frame.agent.as_deref().is_some_and(|l| !valid_label(l)) {
        return Err(invalid());
    }
    let key = match (resolving, frame.key.as_deref()) {
        (true, Some(key)) => Some(SecretKey::new(key).map_err(|_| invalid())?),
        (false, None) => None,
        _ => return Err(invalid()),
    };
    // #11464 AC2: no config, an unknown host, or an unlisted project or key
    // goes no further.
    let refused = || GateReply::refused(ErrorKind::RemoteRefused.as_str());
    let machine = load_machine_at(&config.machine_config)
        .ok()
        .flatten()
        .ok_or_else(refused)?;
    let entry = machine.remote_host(&config.host).ok_or_else(refused)?;
    if entry.keys.is_empty() || key.as_ref().is_some_and(|k| !entry.allows_key(k)) {
        return Err(refused());
    }
    let dir = checkout(entry, &owner, &repo).ok_or_else(refused)?;
    Ok(match key {
        Some(key) => Plan::Resolve {
            dir,
            key,
            project: format!("{owner}/{repo}"),
            agent: frame.agent,
        },
        None => Plan::Doctor,
    })
}

/// The checkout `entry` maps `<owner>/<repo>` to, when it is one.
///
/// What: the mapped path must be absolute, a directory, and have an
/// `origin` remote naming the same repository; anything else is `None`.
/// Test: `remote_gate_without_a_matching_checkout_refuses_and_sends_nothing`.
fn checkout(entry: &RemoteHostConfig, owner: &OwnerName, repo: &RepoName) -> Option<PathBuf> {
    let dir = entry.project_dir(owner, repo)?;
    if !dir.is_absolute() || !dir.is_dir() {
        return None;
    }
    let url = platform::origin_remote_url(dir)?;
    let (o, r) = parse_remote_identity(&url).ok()?;
    (&o == owner && &r == repo).then(|| dir.to_path_buf())
}

/// The server's response as the gate's reply.
///
/// What: a result is `{"ok": true, "result": …}` and exit 0. Every error,
/// and a response with neither, is [`EXIT_REFUSED`], with the error's kind
/// or `backend_failed` when it carries none (an older server).
/// Test: `remote_gate_answer_table`.
pub(crate) fn answer(response: RpcResponse) -> GateReply {
    match (response.result, response.error) {
        (_, Some(error)) => {
            let kind = error
                .data
                .as_ref()
                .and_then(|data| data.get("kind"))
                .and_then(Value::as_str)
                .and_then(ErrorKind::from_wire)
                .unwrap_or(ErrorKind::BackendFailed);
            GateReply::refused(kind.as_str())
        }
        (Some(result), None) => GateReply {
            line: json!({"ok": true, "result": result}),
            status: 0,
        },
        (None, None) => GateReply::refused(ErrorKind::BackendFailed.as_str()),
    }
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
