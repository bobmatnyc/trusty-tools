//! The replay driver's mirror for `dashboard_ids` (#12029 P2d).
//!
//! Why: the recordings run `tests/fixtures/replay/dashboard_ids/driver.py`
//! against the frozen module; the port replays the same case lists.
//! What: [`replay_cases`] runs each `{"op": ...}` of `CASES.json` and prints
//! `case <i> <op>` and the answer, indented, or the exception in the
//! driver's form. The sandbox root (the working directory) stands for the
//! checkout: the default store is `local/owner-queue`, the brief registry
//! `local/briefs/registry.jsonl`, the schemas `schemas/`.
//! Test: `dashboard_ids_recordings_replay_clean_against_the_port`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::{At, Ctx, IdsError, Plan};
use crate::clock::Clock;
use crate::dashboard::pending::from_iso_parts;
use crate::dashboard::pyderive::ids::{KEEP_FREE, Retire};
use crate::dashboard::pyjsonl::join;
use crate::dashboard::store::py::{PyStoreError, error_line, file_op, makedirs};
use crate::ids36;
use crate::pyjson_file::read_text;
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pystr::py_repr;
use crate::pyval::{dumps, get, py_str};
use crate::spec_drift::Outcome;
use crate::writers::brief_ids;

fn s<'a>(c: &'a PyValue, k: &str) -> &'a str {
    match get(c, k) {
        Some(PyValue::Str(v)) => v,
        _ => "",
    }
}

fn json(v: &PyValue) -> String {
    dumps(&crate::dashboard::store::py::sorted(v), None, false)
}

fn strs(v: Vec<String>) -> PyValue {
    PyValue::List(v.into_iter().map(PyValue::Str).collect())
}

fn int(n: i64) -> PyValue {
    PyValue::Int(PyInt::Small(n))
}

fn num(v: Option<&PyValue>) -> Option<f64> {
    match v {
        Some(PyValue::Int(PyInt::Small(n))) => Some(*n as f64),
        Some(PyValue::Float(f)) => Some(*f),
        _ => None,
    }
}

fn when(c: &PyValue) -> Option<At> {
    let (wall, off) = from_iso_parts(s(c, "now"))?;
    Some(At {
        unix: (wall - off) as f64 / 1e6,
    })
}

fn opt_root(c: &PyValue) -> Option<&str> {
    match get(c, "root") {
        Some(PyValue::Str(r)) => Some(r),
        _ => None,
    }
}

/// `fill`: one `<root>/<coll>/<prefix><enc(n)>.json` per `n` in the range.
fn fill(c: &PyValue) -> Result<Vec<String>, PyStoreError> {
    let range: Vec<i64> = match get(c, "range") {
        Some(PyValue::List(l)) => l
            .iter()
            .filter_map(|v| num(Some(v)).map(|f| f as i64))
            .collect(),
        _ => Vec::new(),
    };
    let (a, b) = (
        range.first().copied().unwrap_or(0),
        range.get(1).copied().unwrap_or(0),
    );
    let coll = get(c, "coll").map_or_else(|| "items".to_owned(), py_str);
    let dir = join(s(c, "root"), &coll);
    makedirs(&dir)?;
    let base =
        from_iso_parts(&s(c, "base").replace('Z', "+00:00")).map(|(w, o)| (w - o) / 1_000_000);
    let step = num(get(c, "step")).unwrap_or(1.0) as i64;
    for n in a..b {
        let mut body = match get(c, "data") {
            Some(PyValue::Dict(p)) => p.clone(),
            _ => Vec::new(),
        };
        if let (Some(key), Some(base)) = (get(c, "stamp").map(py_str), base) {
            let at = PyValue::Str(crate::classify::alerts::iso((base + step * n) as f64));
            body = crate::dashboard::store::py::with_key(&body, &key, at);
        }
        body = crate::dashboard::store::py::with_key(&body, "version", int(1));
        let path = join(
            &dir,
            &format!(
                "{}{}.json",
                s(c, "prefix"),
                ids36::enc(n).unwrap_or_default()
            ),
        );
        std::fs::write(&path, dumps(&PyValue::Dict(body), None, true))
            .map_err(|e| PyStoreError::os(&path, &e))?;
    }
    Ok(vec![format!("filled {}", b - a)])
}

struct Driver<'a> {
    ctx: Ctx<'a>,
    /// `main`'s own stdout, printed before the driver's lines.
    raw: String,
    stderr: String,
}

impl Driver<'_> {
    fn answer(&mut self, c: &PyValue) -> Result<Vec<String>, IdsError> {
        let root = opt_root(c);
        let prefix = s(c, "prefix");
        let ctx = &self.ctx;
        Ok(match s(c, "op") {
            "fill" => fill(c)?,
            "ls" => {
                let d = s(c, "path");
                match std::fs::read_dir(d) {
                    Ok(list) => {
                        let mut names: Vec<String> = list
                            .filter_map(Result::ok)
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .collect();
                        names.sort();
                        vec![names.join(" ")]
                    }
                    Err(_) => vec!["absent".into()],
                }
            }
            "used" => {
                let u: Vec<String> = super::used(ctx, prefix, root)?
                    .iter()
                    .map(i64::to_string)
                    .collect();
                vec![format!("[{}]", u.join(", "))]
            }
            "free_count" => vec![super::free_count(ctx, prefix, root)?.to_string()],
            "next_id" => vec![
                super::next_id(ctx, prefix, root)?.map_or_else(|| "None".into(), |i| py_repr(&i)),
            ],
            "counters_path" => vec![super::counters_path(ctx, root)],
            "read_counters" => {
                let pairs = super::read_counters(ctx, root)
                    .into_iter()
                    .map(|(k, v)| (k, int(v)))
                    .collect();
                vec![json(&PyValue::Dict(pairs))]
            }
            "record_issued" => {
                super::record_issued(ctx, prefix, s(c, "id"), root)?;
                vec!["ok".into()]
            }
            "brief" => {
                let next = super::brief_next(ctx).map_err(PyStoreError::Store)?;
                vec![
                    next,
                    super::brief_used(ctx)
                        .map_err(PyStoreError::Store)?
                        .to_string(),
                ]
            }
            "retire_candidates" => {
                let r = Retire {
                    keep_free: match get(c, "keep_free") {
                        Some(PyValue::Int(PyInt::Small(n))) => *n,
                        _ => KEEP_FREE,
                    },
                    age_days: num(get(c, "age_days")),
                    now: when(c).map(|a| a.unix),
                };
                vec![json(&strs(super::retire_candidates(ctx, prefix, root, r)?))]
            }
            "retire_plan" => {
                let mut plan = Plan::default();
                if let Some(p) = get(c, "prefixes") {
                    plan.prefixes = py_str(p);
                }
                if let Some(PyValue::Int(PyInt::Small(n))) = get(c, "keep_free") {
                    plan.keep_free = *n;
                }
                if let Some(a) = num(get(c, "age_days")) {
                    plan.age_days = a;
                    plan.age_text = get(c, "age_days").map_or_else(String::new, crate::pyval::repr);
                }
                let at = when(c).unwrap_or(At {
                    unix: ctx.clock.now_unix(),
                });
                let snap = ctx.snap(root)?;
                super::retire_plan(&snap, at, &plan)
                    .into_iter()
                    .map(|(p, rows)| format!("{p} {}", json(&PyValue::List(rows))))
                    .collect()
            }
            "retire_apply" => {
                let prefixes =
                    get(c, "prefixes").map_or_else(|| super::PREFIXES.to_owned(), py_str);
                let apply = get(c, "apply") == Some(&PyValue::Bool(true));
                let out =
                    super::retire_apply(ctx, root, apply, when(c), &prefixes, &mut self.stderr)?;
                out.into_iter()
                    .map(|(p, a, b, k, f)| {
                        json(&PyValue::List(vec![
                            PyValue::Str(p),
                            int(a),
                            int(b),
                            int(k),
                            int(f),
                        ]))
                    })
                    .collect()
            }
            "main" => {
                let argv: Vec<String> = match get(c, "argv") {
                    Some(PyValue::List(l)) => l.iter().map(py_str).collect(),
                    _ => Vec::new(),
                };
                let o = super::cli::run(&argv, ctx);
                self.raw = o.stdout;
                self.stderr.push_str(&o.stderr);
                vec![format!("exit {}", o.code)]
            }
            other => {
                return Err(IdsError::Store(PyStoreError::Store(format!(
                    "KeyError: {}",
                    py_repr(other)
                ))));
            }
        })
    }
}

/// `env`: set and unset variables for the cases after it.
fn env_op(c: &PyValue, env: &mut BTreeMap<String, String>) {
    if let Some(PyValue::Dict(set)) = get(c, "set") {
        for (k, v) in set {
            env.insert(k.clone(), py_str(v));
        }
    }
    if let Some(PyValue::List(unset)) = get(c, "unset") {
        for k in unset {
            env.remove(&py_str(k));
        }
    }
}

fn error(e: &IdsError) -> String {
    match e {
        IdsError::Value(m) => format!("ValueError: {m}"),
        IdsError::Store(e) => error_line(e),
    }
}

/// Why: the one entry `examples/replay_port.rs dashboard_ids` dispatches to.
/// What: see the module doc; `clock` is the recording's frozen clock.
/// Test: `dashboard_ids_recordings_replay_clean_against_the_port`.
pub fn replay_cases(args: &[String], clock: Arc<dyn Clock + Send + Sync>) -> Outcome {
    let done = |stdout: String, stderr: String, code: i32| Outcome {
        stdout,
        stderr,
        code,
    };
    let [path] = args else {
        return done(String::new(), "usage: driver.py CASES.json\n".into(), 2);
    };
    let cases = match read_text(std::path::Path::new(path)).map(|t| loads(&t)) {
        Ok(Ok(PyValue::List(cases))) => cases,
        _ => return done(String::new(), format!("cannot read {path}\n"), 1),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_text = cwd.to_string_lossy().into_owned();
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    let mut stdout = String::new();
    let mut stderr = String::new();
    for (i, case) in cases.iter().enumerate() {
        let op = s(case, "op");
        stdout.push_str(&format!("case {i} {op}\n"));
        let lines = if op == "env" {
            env_op(case, &mut env);
            vec!["ok".to_owned()]
        } else if let Some(r) = file_op(case) {
            r.unwrap_or_else(|e| vec![error_line(&e)])
        } else {
            let lookup = |k: &str| env.get(k).cloned();
            let mut d = Driver {
                ctx: ctx_for(&cwd, &cwd_text, &lookup, clock.clone()),
                raw: String::new(),
                stderr: String::new(),
            };
            let lines = d.answer(case).unwrap_or_else(|e| vec![error(&e)]);
            stdout.push_str(&d.raw);
            stderr.push_str(&d.stderr);
            lines
        };
        for line in lines {
            stdout.push_str(&format!("  {line}\n"));
        }
    }
    done(stdout, stderr, 0)
}

fn ctx_for<'a>(
    cwd: &std::path::Path,
    cwd_text: &str,
    env: &'a dyn Fn(&str) -> Option<String>,
    clock: Arc<dyn Clock + Send + Sync>,
) -> Ctx<'a> {
    Ctx {
        cwd: cwd.to_path_buf(),
        default_root: join(&join(cwd_text, "local"), "owner-queue"),
        env,
        schema_dir: cwd.join("schemas"),
        briefs: brief_ids::Paths {
            repo: cwd.to_path_buf(),
        },
        clock,
    }
}
