//! `tm architect dashboard <verb>`: the dashboard store's verbs (#10063 PR 3).
//!
//! Why: DOC-82 LDG-3.3 — `dashboard-db.py`'s verbs with its arguments,
//! stdout and exit classes, on the bodies the `mpm.architect.dashboard.*`
//! methods run. Like the rest of `tm architect` it runs in-process: the
//! store's own lock excludes the daemon and the prototype, so no daemon is
//! needed for a correct write.
//! What: [`run`] resolves the store (`--root`, else `DASHBOARD_ROOT`, else
//! `<Architect dir>/local/owner-queue`, made absolute; `--format` /
//! `DASHBOARD_FORMAT`, default `dir`; `--actor` / `DASHBOARD_ACTOR`), builds
//! the verb's request (a `--json` argument is a file, `-` for stdin, or a
//! literal starting `{` or `[`; a batch file holds a list or `{"ops": [...]}`),
//! runs `trusty_architect::dashboard::verbs` with
//! [`trusty_mpm::core::architect_dashboard::env`], and prints the reply as
//! the prototype does. A store error prints `tm architect dashboard: <reason>`
//! to stderr and exits 1 (stale `if_version`, owner ruling D34), 2 (refused)
//! or 3 (unreadable or locked); `fsck` and `validate` exit 1 on a problem.
//! Test: `cli_parses_architect_dashboard_verbs`; the binary in
//! `tests/tm_architect_verbs.rs` (`dashboard_binary_conflict_exits_1_and_writes_nothing`).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_architect::dashboard::rpc::{
    BatchRequest, DeleteRequest, FooterRequest, GetRequest, IdsRequest, ListRequest, PrunedRequest,
    StoreRequest, StoreSel, ValidateRequest, WriteRequest,
};
use trusty_architect::dashboard::verbs::{self, Reply, exit_code, render};
use trusty_architect::dashboard::{DashError, Format};
use trusty_mpm::core::architect_dashboard::env;

use crate::cli::{DashboardArgs, DashboardVerb};

/// A failure: exit code and message.
type Failure = (i32, String);

fn fail(e: &DashError) -> Failure {
    (e.code().exit_code(), e.to_string())
}

/// Runs `args` for this process; returns the exit code.
pub(crate) fn run(args: DashboardArgs) -> i32 {
    let get = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut stdin = io::stdin().lock();
    let outcome = execute(args, &get, &cwd, &mut stdin);
    let (stdout, code) = match outcome {
        Ok(ok) => ok,
        Err((code, msg)) => {
            eprintln!("tm architect dashboard: {msg}");
            return code;
        }
    };
    let mut out = io::stdout().lock();
    match out.write_all(stdout.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => code,
        Err(e) => {
            eprintln!("tm architect dashboard: cannot write stdout: {e}");
            3
        }
    }
}

/// The verb's stdout and exit code; see the module doc.
fn execute(
    args: DashboardArgs,
    get: &dyn Fn(&str) -> Option<String>,
    cwd: &Path,
    stdin: &mut dyn Read,
) -> Result<(String, i32), Failure> {
    let architect = trusty_architect::verbs::project_root(get, cwd);
    let root = args
        .root
        .or_else(|| get("DASHBOARD_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| architect.join("local").join("owner-queue"));
    let format: Format = args
        .format
        .or_else(|| get("DASHBOARD_FORMAT"))
        .unwrap_or_else(|| "dir".into())
        .parse()
        .map_err(|e: DashError| fail(&e))?;
    let store = StoreSel {
        root: cwd.join(root),
        format,
        actor: args.actor.or_else(|| get("DASHBOARD_ACTOR")),
        prune_log: args.prune_log.map(|p| cwd.join(p)),
    };
    let env = env();
    let read = |arg: &str, stdin: &mut dyn Read| load_json(arg, stdin);
    let reply = match args.verb {
        DashboardVerb::Get { collection, id } => verbs::get(
            &env,
            &GetRequest {
                store,
                collection,
                id,
            },
        )
        .map(Reply::Doc),
        DashboardVerb::List { collection, status } => {
            let r = ListRequest {
                store,
                collection,
                status,
                order_by: None,
                desc: false,
                limit: None,
            };
            verbs::list(&env, &r).map(Reply::Docs)
        }
        DashboardVerb::Set {
            collection,
            id,
            json,
            if_version,
        } => {
            let data = read(&json, stdin)?;
            verbs::set(
                &env,
                &WriteRequest {
                    store,
                    collection,
                    id,
                    data,
                    if_version,
                },
            )
            .map(Reply::Op)
        }
        DashboardVerb::Update {
            collection,
            id,
            json,
            if_version,
        } => {
            let data = read(&json, stdin)?;
            verbs::update(
                &env,
                &WriteRequest {
                    store,
                    collection,
                    id,
                    data,
                    if_version,
                },
            )
            .map(Reply::Op)
        }
        DashboardVerb::Delete {
            collection,
            id,
            if_version,
        } => verbs::delete(
            &env,
            &DeleteRequest {
                store,
                collection,
                id,
                if_version,
            },
        )
        .map(Reply::Op),
        DashboardVerb::Batch { file } => {
            let ops = match read(&file, stdin)? {
                Value::Object(mut m) if m.contains_key("ops") => {
                    m.remove("ops").unwrap_or_default()
                }
                other => other,
            };
            let Value::Array(ops) = ops else {
                return Err((
                    2,
                    "batch input must be a JSON list of ops or {\"ops\": [...]}".into(),
                ));
            };
            verbs::batch(&env, &BatchRequest { store, ops }).map(Reply::Ops)
        }
        DashboardVerb::Pruned { id, collection } => verbs::pruned(
            &env,
            &PrunedRequest {
                store,
                id,
                collection,
            },
        )
        .map(Reply::Pruned),
        DashboardVerb::Validate { schemas } => {
            let schemas = schemas
                .or_else(|| get("DASHBOARD_SCHEMA_DIR").map(PathBuf::from))
                .unwrap_or_else(|| architect.join("schemas").join("dashboard"));
            let r = ValidateRequest {
                store,
                schemas: cwd.join(schemas),
            };
            verbs::validate(&env, &r).map(Reply::Validation)
        }
        DashboardVerb::Fsck => verbs::fsck(&env, &StoreRequest { store }).map(Reply::Fsck),
        DashboardVerb::Tally { as_json } => {
            verbs::tally_verb(&env, &StoreRequest { store }).map(|t| Reply::Tally(t, as_json))
        }
        DashboardVerb::Footer {
            as_json,
            local,
            tailnet,
        } => {
            let tailnet = tailnet
                .or_else(|| get("DASHBOARD_TAILNET_URL"))
                .ok_or_else(|| {
                    (
                        2,
                        "footer needs --tailnet or DASHBOARD_TAILNET_URL (no host is guessed)"
                            .to_owned(),
                    )
                })?;
            let r = FooterRequest {
                store,
                local: local.or_else(|| get("DASHBOARD_LOCAL_URL")),
                tailnet,
            };
            verbs::footer_verb(&env, &r).map(|f| Reply::Footer(f, as_json))
        }
        DashboardVerb::Ids { prefix } => {
            verbs::ids(&env, &IdsRequest { store, prefix }).map(Reply::Ids)
        }
    }
    .map_err(|e| fail(&e))?;
    Ok((render(&reply), exit_code(&reply)))
}

/// `dashboard-db.py`'s `load_json`: `-` reads stdin, a literal starts with
/// `{` or `[`, anything else is a file path.
fn load_json(arg: &str, stdin: &mut dyn Read) -> Result<Value, Failure> {
    let text = arg.trim();
    let text = if arg == "-" {
        let mut buf = String::new();
        stdin
            .read_to_string(&mut buf)
            .map_err(|e| (2, format!("cannot read stdin: {e}")))?;
        buf
    } else if text.starts_with('{') || text.starts_with('[') {
        text.to_owned()
    } else {
        std::fs::read_to_string(arg).map_err(|e| (2, format!("cannot read {arg}: {e}")))?
    };
    serde_json::from_str(&text).map_err(|e| (2, format!("invalid JSON: {e}")))
}
