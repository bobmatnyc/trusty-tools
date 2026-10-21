//! `tm dbatg`: the PM ledger verbs, over the daemon socket (#9758).
//!
//! Why: DOC-82 LDG-3.3 — one method family behind the daemon, the MCP tool
//! and this CLI, printing the prototype's stdout and exit codes. When the
//! daemon is down the CLI writes in-process under the same store lock, and
//! only to the caller's own store (owner ruling, #9758 amendment).
//! What: [`run`] resolves the store (`--project`, default the cwd, mapped to
//! its launch directory; `--session`, default `$DBATG_SESSION` or the tmux
//! session of `$TMUX_PANE`), builds the `mpm.dbatg.*` request and sends it.
//! A transport failure (no daemon, or a dropped response) runs the same body
//! in-process with the same `request_id`, so a batch the daemon already
//! committed is answered from its log record, never applied twice. A read
//! (`show`) falls back for any store; a write only for the store of this
//! process's own session and launch directory.
//! Test: `commands::dbatg::tests`.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use trusty_architect::ledger::LedgerError;
use trusty_architect::ledger::rpc::{
    BatchRequest, BatchResponse, InitRequest, InitResponse, Op, ShowRequest, StoreRef, TextResponse,
};
use trusty_architect::ledger::store::resolve_project;
use trusty_architect::ledger::writer::{render_ids, render_prune};
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::{JSONRPC_VERSION, RpcResponse};
use trusty_mpm::core::dbatg;

use crate::cli::{DbatgArgs, DbatgVerb};

#[cfg(test)]
#[path = "dbatg_tests.rs"]
mod tests;

/// How long a daemon call may take; the writer's own lock wait is 10 s.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// What a verb printed, and its failure (exit code, message).
pub(crate) struct Outcome {
    pub stdout: String,
    pub error: Option<(i32, String)>,
}

/// The process facts a verb runs against.
pub(crate) struct Ctx<'a> {
    /// The daemon socket.
    pub socket: PathBuf,
    /// The working directory.
    pub cwd: PathBuf,
    /// This process's own session name, from the environment or tmux.
    pub own_session: Option<String>,
    /// Batch input when no `--file` is given.
    pub stdin: &'a mut dyn Read,
}

/// Runs `args` for this process; returns the exit code.
pub(crate) async fn run(args: DbatgArgs) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let own_session = std::env::var("DBATG_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(tmux_session);
    let mut stdin = io::stdin().lock();
    let socket = trusty_common::mpm_rpc::mpm_socket();
    let out = execute(
        args,
        Ctx {
            socket,
            cwd,
            own_session,
            stdin: &mut stdin,
        },
    )
    .await;
    let mut stdout = io::stdout().lock();
    if let Err(e) = stdout
        .write_all(out.stdout.as_bytes())
        .and_then(|()| stdout.flush())
    {
        eprintln!("dbatg: cannot write stdout: {e}");
        return 1;
    }
    match out.error {
        None => 0,
        Some((code, msg)) => {
            eprintln!("dbatg: {msg}");
            code
        }
    }
}

/// The tmux session of `$TMUX_PANE`, as the prototype's `_tmux_session`.
fn tmux_session() -> Option<String> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", &pane, "#{session_name}"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !name.is_empty()).then_some(name)
}

type Failure = (i32, String);

fn fail(e: &LedgerError) -> Failure {
    (e.code().exit_code(), e.to_string())
}

/// Runs one verb; see the module doc.
pub(crate) async fn execute(args: DbatgArgs, ctx: Ctx<'_>) -> Outcome {
    match verb(args, ctx).await {
        Ok(stdout) => Outcome {
            stdout,
            error: None,
        },
        Err(e) => Outcome {
            stdout: String::new(),
            error: Some(e),
        },
    }
}

async fn verb(args: DbatgArgs, ctx: Ctx<'_>) -> Result<String, Failure> {
    let session = args
        .session
        .or_else(|| ctx.own_session.clone())
        .ok_or_else(|| {
            (
                2,
                "no session: pass --session or set DBATG_SESSION".to_owned(),
            )
        })?;
    let project = args.project.unwrap_or_else(|| ctx.cwd.clone());
    let launch = resolve_project(&project).map_err(|e| fail(&e))?;
    let own = resolve_project(&ctx.cwd).is_ok_and(|c| c == launch)
        && ctx.own_session.as_deref().is_none_or(|o| o == session);
    let store = StoreRef {
        project: launch.clone(),
        session: session.clone(),
    };
    let ops = match args.verb {
        DbatgVerb::Init { code } => {
            let req = InitRequest {
                project: launch,
                session,
                code: code.clone(),
            };
            let _: InitResponse = call(&ctx.socket, "mpm.dbatg.init", &req, own, || {
                dbatg::init(&req)
            })
            .await?;
            return Ok(format!("initialised {code}\n"));
        }
        DbatgVerb::Show { brief } => {
            let req = ShowRequest { store, brief };
            let out: TextResponse = call(&ctx.socket, "mpm.dbatg.show", &req, true, || {
                dbatg::show(&req)
            })
            .await?;
            return Ok(out.text + "\n");
        }
        DbatgVerb::Batch { file } => read_ops(file.as_deref(), ctx.stdin)?,
        other => vec![one_op(other)?],
    };
    let prune = match ops.as_slice() {
        [Op::Prune(p)] => Some(p.dry_run),
        _ => None,
    };
    let state = store.paths().map_err(|e| fail(&e))?.state;
    let size = || std::fs::metadata(&state).map_or(0, |m| m.len());
    let before = size();
    let request_id = args.request_id.unwrap_or_else(request_id);
    let req = BatchRequest {
        store,
        request_id,
        if_v: None,
        ops,
    };
    let resp: BatchResponse = call(&ctx.socket, "mpm.dbatg.batch", &req, own, || {
        dbatg::batch(&req)
    })
    .await?;
    Ok(match prune {
        Some(dry_run) => render_prune(&resp, dry_run, before, size()),
        None => render_ids(&resp),
    } + "\n")
}

/// A fresh request id: process id and wall-clock nanoseconds.
fn request_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("tm-{}-{nanos}", std::process::id())
}

/// The batch op a single-op verb stands for, as the prototype's CLI builds it.
fn one_op(verb: DbatgVerb) -> Result<Op, Failure> {
    let op = match verb {
        DbatgVerb::Add {
            kind,
            title,
            goal,
            rank,
            finish,
            maps,
            opts,
            to,
            id,
        } => {
            let opts: Vec<Value> = opts
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| json!({"l": p.trim_end_matches('*'), "rec": p.ends_with('*')}))
                .collect();
            json!({"op": "add", "kind": kind, "title": title, "goal": goal, "rank": rank, "finish": finish,
                   "maps": maps, "opts": opts, "to": to, "id": id})
        }
        DbatgVerb::Update {
            id,
            title,
            status,
            answer,
            outcome,
            esc,
            to,
            note,
            rank,
        } => json!({
            "op": "update", "id": id, "title": title, "status": status, "answer": answer,
            "outcome": outcome, "esc": esc, "to": to, "note": note, "rank": rank}),
        DbatgVerb::Close { id, outcome } => json!({"op": "close", "id": id, "outcome": outcome}),
        DbatgVerb::Prune {
            dry_run,
            answered_hours,
            require_outcome,
        } => json!({
            "op": "prune", "dry_run": dry_run, "answered_hours": answered_hours, "require_outcome": require_outcome}),
        DbatgVerb::Init { .. } | DbatgVerb::Show { .. } | DbatgVerb::Batch { .. } => {
            return Err((2, "not a single-op verb".into()));
        }
    };
    // `null` is an absent field: drop it so the op's own defaults apply.
    let op = match op {
        Value::Object(map) => {
            Value::Object(map.into_iter().filter(|(_, v)| !v.is_null()).collect())
        }
        other => other,
    };
    serde_json::from_value(op).map_err(|e| (2, format!("bad op: {e}")))
}

/// The JSON list of ops in `file`, or on `stdin`.
fn read_ops(file: Option<&Path>, stdin: &mut dyn Read) -> Result<Vec<Op>, Failure> {
    let mut text = String::new();
    match file {
        Some(path) => {
            text = std::fs::read_to_string(path)
                .map_err(|e| (2, format!("{}: {e}", path.display())))?
        }
        None => drop(
            stdin
                .read_to_string(&mut text)
                .map_err(|e| (2, format!("stdin: {e}")))?,
        ),
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|e| (2, format!("batch input is not JSON ({e})")))?;
    if !value.is_array() {
        return Err((2, "batch input must be a JSON list".into()));
    }
    serde_json::from_value(value).map_err(|e| (2, format!("bad op: {e}")))
}

/// Sends `method` to the daemon; on a transport failure runs `local` when
/// `fallback` allows it (see the module doc).
async fn call<Req, Resp>(
    socket: &Path,
    method: &str,
    req: &Req,
    fallback: bool,
    local: impl FnOnce() -> Result<Resp, LedgerError>,
) -> Result<Resp, Failure>
where
    Req: Serialize,
    Resp: DeserializeOwned,
{
    let frame = json!({"jsonrpc": JSONRPC_VERSION, "id": 1, "method": method, "params": req});
    let transport = match send_framed_request::<_, RpcResponse>(socket, &frame, CALL_TIMEOUT).await
    {
        Ok(RpcResponse { error: Some(e), .. }) => {
            let exit = e
                .data
                .as_ref()
                .and_then(|d| d["exit"].as_i64())
                .and_then(|x| i32::try_from(x).ok());
            return Err((exit.unwrap_or(2), e.message));
        }
        Ok(RpcResponse {
            result: Some(v), ..
        }) => {
            return serde_json::from_value(v).map_err(|e| (3, format!("{method}: bad reply: {e}")));
        }
        Ok(_) => format!("{method}: empty reply"),
        Err(e) => e.to_string(),
    };
    if !fallback {
        return Err((
            3,
            format!(
                "daemon unreachable ({transport}); the in-process fallback writes only this session's own store"
            ),
        ));
    }
    local().map_err(|e| fail(&e))
}
