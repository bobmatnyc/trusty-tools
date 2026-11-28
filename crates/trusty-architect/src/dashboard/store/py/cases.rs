//! The replay driver's mirror for `dashboard_store` (#12029 P2c).
//!
//! Why: the recordings run `tests/fixtures/replay/dashboard_store/driver.py`
//! against the frozen module; the port is replayed through the same case
//! lists, and its values ([`PyValue`]) stay inside the crate.
//! What: [`replay_cases`] runs each `{"op": ...}` of `CASES.json` and prints
//! `case <i> <op>` and the answer, indented, or the exception in the
//! driver's form. Paths are relative to the working directory, the replay
//! sandbox; schemas are read from its `schemas/`.
//! Test: `dashboard_store_recordings_replay_clean_against_the_port`.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::write::OpOut;
use super::{Options, PyDocOut, PyStore, PyStoreError, default_format};
use crate::clock::Clock;
use crate::dashboard::pyjsonl;
use crate::pyjson_file::read_text;
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pystr::py_repr;
use crate::pyval::{dumps, get, int_text, py_str, repr};
use crate::spec_drift::Outcome;

const USAGE: &str = "usage: driver.py CASES.json\n";

struct Driver {
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    stores: BTreeMap<String, PyStore>,
    clock: Arc<dyn Clock + Send + Sync>,
    stderr: String,
}

fn s<'a>(c: &'a PyValue, k: &str) -> &'a str {
    match get(c, k) {
        Some(PyValue::Str(v)) => v,
        _ => "",
    }
}

/// `case.get(k)` with JSON `null` as absent.
fn opt<'a>(c: &'a PyValue, k: &str) -> Option<&'a PyValue> {
    get(c, k).filter(|v| !matches!(v, PyValue::Null))
}

fn opt_str(c: &PyValue, k: &str) -> Option<String> {
    opt(c, k).map(py_str)
}

fn json(v: &PyValue) -> String {
    dumps(&super::value::sorted(v), None, false)
}

fn result_line(r: &OpOut) -> String {
    let text = |v: &str| PyValue::Str(v.to_owned());
    let mut pairs = vec![
        ("collection".to_owned(), text(&r.collection)),
        ("id".to_owned(), text(&r.id)),
    ];
    match &r.version {
        Some(v) => pairs.push(("version".into(), PyValue::Int(v.clone()))),
        None => pairs.push(("deleted".into(), PyValue::Bool(true))),
    }
    json(&PyValue::Dict(pairs))
}

fn doc_line(d: &PyDocOut) -> String {
    format!(
        "{} {} {}",
        py_repr(&d.id),
        int_text(&d.version),
        json(&d.data)
    )
}

fn error_line(e: &PyStoreError) -> String {
    match (e, e.http()) {
        (_, Some((status, code))) => format!("{} {status} {code}: {e}", e.class()),
        (PyStoreError::Os { class, errno, .. }, None) => format!("{class}: errno {errno}"),
        _ => format!("{}: {e}", e.class()),
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|i| u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

fn os_err(path: &str) -> impl Fn(std::io::Error) -> PyStoreError + '_ {
    move |e| PyStoreError::os(path, &e)
}

impl Driver {
    fn store(&self, c: &PyValue) -> Result<&PyStore, PyStoreError> {
        let name = opt_str(c, "store").unwrap_or_else(|| "s".into());
        self.stores
            .get(&name)
            .ok_or_else(|| PyStoreError::Store(format!("KeyError: {}", py_repr(&name))))
    }

    fn setup(&mut self, c: &PyValue) -> Result<Option<Vec<String>>, PyStoreError> {
        let path = s(c, "path");
        let ok = Ok(Some(vec!["ok".to_owned()]));
        match s(c, "op") {
            "env" => {
                if let Some(PyValue::Dict(set)) = get(c, "set") {
                    for (k, v) in set {
                        self.env.insert(k.clone(), py_str(v));
                    }
                }
                if let Some(PyValue::List(unset)) = get(c, "unset") {
                    for k in unset {
                        self.env.remove(&py_str(k));
                    }
                }
                ok
            }
            "write" => {
                let parent = super::dirname(path);
                super::write::makedirs(if parent.is_empty() { "." } else { parent })?;
                let blob = match get(c, "hex") {
                    Some(PyValue::Str(h)) => hex(h),
                    _ => s(c, "text").as_bytes().to_vec(),
                };
                std::fs::write(path, blob).map_err(os_err(path))?;
                ok
            }
            "rm" => {
                std::fs::remove_file(path).map_err(os_err(path))?;
                ok
            }
            "mkdir" => {
                super::write::makedirs(path)?;
                ok
            }
            "chmod" => {
                use std::os::unix::fs::PermissionsExt;
                let mode = u32::from_str_radix(s(c, "mode"), 8).unwrap_or(0o755);
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                    .map_err(os_err(path))?;
                ok
            }
            "cat" => match std::fs::read(path) {
                Ok(b) => Ok(Some(vec![py_repr(&String::from_utf8_lossy(&b))])),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Ok(Some(vec!["absent".into()]))
                }
                Err(e) => Err(PyStoreError::os(path, &e)),
            },
            "changes" => {
                let all = pyjsonl::read_changes(s(c, "root")).map_err(super::from_jsonl)?;
                Ok(Some(if all.is_empty() {
                    vec!["empty".into()]
                } else {
                    all.iter().map(json).collect()
                }))
            }
            "default_format" => {
                let env = |k: &str| self.env.get(k).cloned();
                Ok(Some(vec![default_format(&env)?.name().to_owned()]))
            }
            "normalize" => Ok(Some(vec![json(&super::normalize_timestamps(
                get(c, "value").unwrap_or(&PyValue::Null),
                None,
            ))])),
            "new" => {
                let env = |k: &str| self.env.get(k).cloned();
                let opts = Options {
                    fmt: opt_str(c, "fmt"),
                    actor: opt_str(c, "actor"),
                    schema_mode: opt_str(c, "schema_mode"),
                    prune_log: opt_str(c, "prune_log"),
                };
                let store = PyStore::new(
                    s(c, "root"),
                    opts,
                    &env,
                    &self.cwd,
                    self.cwd.join("schemas"),
                    self.clock.clone(),
                )?;
                let lines = vec![
                    format!(
                        "fmt={} actor={} schema_mode={}",
                        store.fmt.name(),
                        store.actor,
                        store.schema_mode
                    ),
                    format!("prune_log={}", store.prune_log_path()),
                ];
                self.stores
                    .insert(opt_str(c, "name").unwrap_or_else(|| "s".into()), store);
                Ok(Some(lines))
            }
            _ => Ok(None),
        }
    }

    /// `hold`: another open file description holds the store lock for `seconds`.
    fn hold(&mut self, c: &PyValue) -> Result<Vec<String>, PyStoreError> {
        let path = format!("{}/.lock", self.store(c)?.root);
        let lf = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(os_err(&path))?;
        // SAFETY: the fd is open for the life of `lf`; flock touches nothing else.
        if unsafe { libc::flock(lf.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(PyStoreError::os(&path, &std::io::Error::last_os_error()));
        }
        let secs = match get(c, "seconds") {
            Some(PyValue::Float(f)) => *f,
            Some(PyValue::Int(PyInt::Small(n))) => *n as f64,
            _ => 0.0,
        };
        let start = Instant::now();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(secs));
            // SAFETY: as above; the file closes when the thread drops it.
            unsafe { libc::flock(lf.as_raw_fd(), libc::LOCK_UN) };
        });
        let then = get(c, "then").cloned().unwrap_or(PyValue::Null);
        let lines = self.answer(&then);
        let waited = start.elapsed().as_secs_f64() >= secs * 0.9;
        let _ = release.join();
        let mut out = vec![format!("waited {}", if waited { "True" } else { "False" })];
        out.extend(lines?);
        Ok(out)
    }

    fn answer(&mut self, c: &PyValue) -> Result<Vec<String>, PyStoreError> {
        if let Some(lines) = self.setup(c)? {
            return Ok(lines);
        }
        let null = PyValue::Null;
        let val = |k: &str| get(c, k).unwrap_or(&null);
        let actor = opt_str(c, "actor");
        let actor = actor.as_deref();
        let op = s(c, "op");
        if op == "hold" {
            return self.hold(c);
        }
        let st = self.store(c)?;
        let out = match op {
            "get" => vec![doc_line(&st.get(val("coll"), val("id"))?)],
            "list" => {
                let limit = match opt(c, "limit") {
                    Some(PyValue::Int(PyInt::Small(n))) => Some(*n),
                    Some(PyValue::Float(f)) => Some(f.trunc() as i64),
                    _ => None,
                };
                let desc = get(c, "desc").is_some_and(crate::pyval::truthy);
                let order_by = opt_str(c, "order_by");
                let docs = st.list_docs(
                    val("coll"),
                    opt(c, "status"),
                    order_by.as_deref(),
                    desc,
                    limit,
                )?;
                if docs.is_empty() {
                    vec!["empty".into()]
                } else {
                    docs.iter().map(doc_line).collect()
                }
            }
            "collections" => {
                let names = st.collections()?;
                vec![repr(&PyValue::List(
                    names.into_iter().map(PyValue::Str).collect(),
                ))]
            }
            "set" => vec![result_line(&st.set(
                val("coll"),
                val("id"),
                get(c, "data"),
                opt(c, "if_version"),
                actor,
            )?)],
            "update" => vec![result_line(&st.update(
                val("coll"),
                val("id"),
                get(c, "data"),
                opt(c, "if_version"),
                actor,
            )?)],
            "delete" => vec![result_line(&st.delete(
                val("coll"),
                val("id"),
                opt(c, "if_version"),
                actor,
            )?)],
            "batch" => st
                .batch(val("ops"), actor)?
                .iter()
                .map(result_line)
                .collect(),
            "add" => {
                let r = st.add(val("coll"), get(c, "data"), actor)?;
                let shape =
                    r.id.len() == 20 && r.id.chars().all(|ch| "0123456789abcdef".contains(ch));
                let got = st.get(val("coll"), &PyValue::Str(r.id.clone()))?;
                let path = format!("{}/{}/{}.json", st.root, r.collection, r.id);
                std::fs::remove_file(&path).map_err(os_err(&path))?;
                let v = r.version.as_ref().map_or_else(String::new, int_text);
                vec![format!(
                    "{} v{v} id20hex={} {}",
                    r.collection,
                    if shape { "True" } else { "False" },
                    json(&got.data)
                )]
            }
            "seed_meta" => vec![
                if st.seed_meta(s(c, "owner_name"))? {
                    "True"
                } else {
                    "False"
                }
                .into(),
            ],
            other => return Err(PyStoreError::Store(format!("KeyError: {}", py_repr(other)))),
        };
        Ok(out)
    }
}

/// Why: the one entry `examples/replay_port.rs dashboard_store` dispatches to.
/// What: see the module doc; `clock` stands for the frozen `datetime.now`.
/// Test: `dashboard_store_recordings_replay_clean_against_the_port`.
pub fn replay_cases(args: &[String], clock: Arc<dyn Clock + Send + Sync>) -> Outcome {
    let fail = |stdout: String, stderr: String, code: i32| Outcome {
        stdout,
        stderr,
        code,
    };
    let [path] = args else {
        return fail(String::new(), USAGE.into(), 2);
    };
    let cases = match read_text(Path::new(path)).map(|t| loads(&t)) {
        Ok(Ok(PyValue::List(cases))) => cases,
        _ => return fail(String::new(), format!("cannot read {path}\n"), 1),
    };
    let mut d = Driver {
        cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
        env: std::env::vars().collect(),
        stores: BTreeMap::new(),
        clock,
        stderr: String::new(),
    };
    let mut stdout = String::new();
    for (i, case) in cases.iter().enumerate() {
        stdout.push_str(&format!("case {i} {}\n", s(case, "op")));
        let lines = d.answer(case).unwrap_or_else(|e| vec![error_line(&e)]);
        for line in lines {
            stdout.push_str(&format!("  {line}\n"));
        }
        for st in d.stores.values() {
            d.stderr.push_str(&st.take_stderr());
        }
    }
    fail(stdout, d.stderr, 0)
}
