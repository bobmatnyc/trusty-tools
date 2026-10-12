//! #9171 A3a parity: the poller's sensors and state helpers against goldens
//! written by the frozen prototype (`fleet-poll.py` at 988152c).
//!
//! What: reads `tests/fixtures/a3/corpus_a3.json`, runs every case through the
//! Rust port, renders each result with `to_python_json` exactly as
//! `regen_a3.py` renders the Python result, and compares the golden file byte
//! for byte. Regenerate with the command in `tests/fixtures/a3/README.md`;
//! never edit the goldens by hand. Every file a case writes lives in a tempdir.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use trusty_architect::classify::{Alert, alert};
use trusty_architect::poller::{
    BandConfig, Config, DiskUsage, HostProbe, TranscriptCtx, append_alerts, band_alerts,
    builder_band, host_alerts, parse_list_panes, read_events, self_ctx_alerts,
};
use trusty_architect::{PaneId, to_python_json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/a3")
}

fn corpus() -> Result<Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(&fs::read_to_string(
        fixture_dir().join("corpus_a3.json"),
    )?)?)
}

/// Compares `rows` with the golden file line by line, naming the first diff.
fn assert_golden(name: &str, rows: &[String]) -> TestResult {
    let golden = fs::read_to_string(fixture_dir().join(name))?;
    let got: String = rows.iter().map(|r| format!("{r}\n")).collect();
    for (i, (g, r)) in golden.lines().zip(got.lines()).enumerate() {
        assert_eq!(r, g, "{name} line {}: Rust (left) differs from Python (right)", i + 1);
    }
    assert_eq!(got, golden, "{name}: row count or trailing bytes differ");
    Ok(())
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or_default()
}

/// A `Config` whose every path is under `dir` and whose env is `vars`.
fn config(dir: &Path, vars: &[(&str, String)]) -> Result<Config, trusty_architect::ConfigError> {
    let base: Vec<(String, String)> = [
        ("SUPERVISOR_INBOX_DIR", "inbox"),
        ("SESSIONS_DIR", "sessions"),
        ("TRANSCRIPT_BASE", "projects"),
        ("SELF_CTX_DIR", "self"),
    ]
    .iter()
    .map(|(k, sub)| ((*k).to_owned(), dir.join(sub).to_string_lossy().into_owned()))
    .chain(vars.iter().map(|(k, v)| ((*k).to_owned(), v.clone())))
    .collect();
    let get = move |k: &str| base.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    Config::from_env(dir, dir, &get)
}

fn set_mtime(path: &Path, secs: f64) -> io::Result<()> {
    fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(UNIX_EPOCH + Duration::from_secs_f64(secs))
}

/// The prototype's pane dict, in its key order.
#[derive(Serialize, serde::Deserialize)]
struct PyPane {
    session: String,
    pane: String,
    cmd: String,
    active: String,
    path: String,
    dead: String,
}

#[test]
fn list_panes_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let golden = fs::read_to_string(fixture_dir().join("list_panes.jsonl"))?;
    for (case, line) in arr(&c["list_panes"]).iter().zip(golden.lines()) {
        let (name, python): (String, Vec<PyPane>) = serde_json::from_str(line)?;
        let listing = parse_list_panes(s(&case[1]));
        let got = to_python_json(&(&name, &listing.panes), true)?;
        if listing.skipped.is_empty() {
            assert_eq!(got, line, "list_panes case {name}");
            continue;
        }
        // Python keeps every row; the port moves non-%N pane ids to `skipped`.
        let (kept, dropped): (Vec<PyPane>, Vec<PyPane>) =
            python.into_iter().partition(|p| PaneId::parse(&p.pane).is_ok());
        assert_eq!(listing.skipped.len(), dropped.len(), "{name}");
        assert_eq!(got, to_python_json(&(&name, &kept), true)?, "list_panes case {name}");
    }
    assert_eq!(arr(&c["list_panes"]).len(), golden.lines().count());
    Ok(())
}

#[test]
fn band_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let cfg = BandConfig::default();
    let rows = arr(&c["band"])
        .iter()
        .map(|r| {
            let band = builder_band(
                &cfg,
                f(&r[0]),
                r[1].as_u64().unwrap_or(1) as usize,
                f(&r[2]),
                r[3].as_f64(),
                r[4].as_f64(),
                r[5].as_str(),
            );
            to_python_json(&(r, band.as_str()), true)
        })
        .collect::<Result<Vec<_>, _>>()?;
    assert_golden("band.jsonl", &rows)
}

#[derive(Serialize)]
struct BandRow<'a> {
    case: &'a str,
    step: usize,
    alerts: Vec<Alert>,
    state: BTreeMap<&'static str, String>,
}

#[test]
fn band_alerts_match_python_goldens() -> TestResult {
    let c = corpus()?;
    let cfg = BandConfig::default();
    let mut rows = Vec::new();
    for sc in arr(&c["band_alerts"]) {
        let mut stored = sc["state"]["band"].as_str().map(str::to_owned);
        for (step, a) in arr(&sc["steps"]).iter().enumerate() {
            let alerts = band_alerts(
                &cfg,
                &mut stored,
                f(&a[0]),
                f(&a[1]),
                a[2].as_u64().unwrap_or(1) as usize,
                f(&a[3]),
                a[4].as_f64(),
                a[5].as_f64(),
            );
            let state = stored.iter().map(|b| ("band", b.clone())).collect();
            let row = BandRow { case: s(&sc["case"]), step, alerts, state };
            rows.push(to_python_json(&row, false)?);
        }
    }
    assert_golden("band_alerts.jsonl", &rows)
}

/// Probe values injected into the prototype's `host_alerts` by `regen_a3.py`.
struct FakeProbe<'a>(&'a Value);

impl HostProbe for FakeProbe<'_> {
    fn loadavg(&self) -> io::Result<[f64; 3]> {
        let l = arr(&self.0["loads"]);
        Ok([f(&l[0]), f(&l[1]), f(&l[2])])
    }
    fn cores(&self) -> usize {
        self.0["cores"].as_u64().unwrap_or(1) as usize
    }
    fn data_volume(&self) -> PathBuf {
        PathBuf::from("/data")
    }
    fn disk_usage(&self, _: &Path) -> io::Result<DiskUsage> {
        let d = arr(&self.0["disk"]);
        let n = |i: usize| d[i].as_u64().unwrap_or_default();
        Ok(DiskUsage { total: n(0), used: n(1), free: n(2) })
    }
    fn swap_used_mb(&self) -> io::Result<Option<f64>> {
        match &self.0["swap"] {
            Value::String(e) if e == "error" => Err(io::Error::other("sysctl failed")),
            v => Ok(v.as_f64()),
        }
    }
    fn mem_free_pct(&self) -> Option<f64> {
        self.0["mem"].as_f64()
    }
}

#[derive(Serialize)]
struct HostRow<'a> {
    case: &'a str,
    alerts: Vec<Alert>,
    state: Option<BTreeMap<&'static str, String>>,
}

#[test]
fn host_alerts_match_python_goldens() -> TestResult {
    let c = corpus()?;
    let now = f(&c["now"]);
    let dir = tempfile::tempdir()?;
    let mut rows = Vec::new();
    for case in arr(&c["host"]) {
        let vars: Vec<(&str, String)> = case["load_factor"]
            .as_str()
            .map(|v| ("LOAD_FACTOR", v.to_owned()))
            .into_iter()
            .collect();
        let cfg = config(dir.path(), &vars)?;
        let probe = FakeProbe(&case["probe"]);
        let has_state = case.get("state").is_some();
        let mut stored = case["state"]["band"].as_str().map(str::to_owned);
        let alerts = host_alerts(&probe, &cfg, now, has_state.then_some(&mut stored))?;
        let state = has_state.then(|| stored.iter().map(|b| ("band", b.clone())).collect());
        let row = HostRow { case: s(&case["case"]), alerts, state };
        rows.push(to_python_json(&row, false)?);
    }
    assert_golden("host.jsonl", &rows)
}

#[derive(Serialize)]
struct EventRow<'a> {
    case: &'a str,
    step: usize,
    alerts: Vec<Alert>,
    events_offset: Option<u64>,
}

#[test]
fn read_events_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let now = f(&c["now"]);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("events.jsonl");
    let mut rows = Vec::new();
    for case in arr(&c["events"]) {
        let _ = fs::remove_file(&path);
        let mut offset = case["offset"].as_u64();
        for (step, st) in arr(&case["steps"]).iter().enumerate() {
            match s(&st["mode"]) {
                "delete" => {
                    let _ = fs::remove_file(&path);
                }
                "a" => {
                    use std::io::Write;
                    let mut f = fs::OpenOptions::new().append(true).create(true).open(&path)?;
                    f.write_all(s(&st["text"]).as_bytes())?;
                }
                _ => fs::write(&path, s(&st["text"]))?,
            }
            let alerts = read_events(&path, &mut offset, now)?;
            let row = EventRow { case: s(&case["case"]), step, alerts, events_offset: offset };
            rows.push(to_python_json(&row, false)?);
        }
    }
    assert_golden("events.jsonl", &rows)
}

fn materialize(dir: &Path, files: &[Value]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    for spec in files {
        let path = dir.join(s(&spec["name"]));
        let body: String = arr(&spec["lines"]).iter().map(|l| format!("{}\n", s(l))).collect();
        fs::write(&path, body)?;
        set_mtime(&path, f(&spec["mtime"]))?;
    }
    Ok(())
}

#[derive(Serialize)]
struct SelfCtxRow<'a> {
    case: &'a str,
    alerts: Vec<Alert>,
    logged: bool,
}

#[test]
fn self_ctx_alerts_match_python_goldens() -> TestResult {
    let c = corpus()?;
    let now = f(&c["now"]);
    let mut rows = Vec::new();
    for case in arr(&c["self_ctx_alerts"]) {
        let dir = tempfile::tempdir()?;
        let ctx_dir = if case["missing"].as_bool() == Some(true) {
            dir.path().join("missing")
        } else {
            materialize(dir.path(), arr(&case["files"]))?;
            dir.path().to_owned()
        };
        let mut vars = vec![("SELF_CTX_DIR", ctx_dir.to_string_lossy().into_owned())];
        for (var, key) in [("SELF_CTX_WINDOW", "window"), ("SELF_CTX_THRESHOLD", "threshold")] {
            if let Some(v) = case[key].as_str() {
                vars.push((var, v.to_owned()));
            }
        }
        // The prototype logs a bad value and raises no alert; the port refuses
        // the value when the config is built.
        let (alerts, logged) = match config(dir.path(), &vars) {
            Err(_) => (Vec::new(), true),
            Ok(cfg) => match self_ctx_alerts(&cfg, now) {
                Ok(a) => (a, false),
                Err(_) => (Vec::new(), true),
            },
        };
        rows.push(to_python_json(&SelfCtxRow { case: s(&case["case"]), alerts, logged }, false)?);
    }
    assert_golden("self_ctx_alerts.jsonl", &rows)
}

#[derive(Serialize)]
struct CtxRow<'a> {
    case: &'a str,
    results: Vec<(String, Option<u64>)>,
}

fn write_sessions(sdir: &Path, sessions: &[Value], dead_pid: &Value) -> io::Result<()> {
    for sess in sessions {
        let pid = match &sess["pid"] {
            Value::String(p) if p == "self" => Value::from(std::process::id()),
            Value::String(p) if p == "self-str" => Value::from(std::process::id().to_string()),
            Value::String(p) if p == "dead" => dead_pid.clone(),
            other => other.clone(),
        };
        let text = match sess["raw"].as_str() {
            Some(raw) => raw.to_owned(),
            None => serde_json::json!({
                "pid": pid, "sessionId": sess["sid"], "tmux": sess["tmux"], "updatedAt": sess["updatedAt"]
            })
            .to_string(),
        };
        fs::write(sdir.join(s(&sess["name"])), text)?;
    }
    Ok(())
}

#[test]
fn transcript_ctx_matches_python_goldens() -> TestResult {
    let c = corpus()?;
    let mut rows = Vec::new();
    for case in arr(&c["transcript"]) {
        let dir = tempfile::tempdir()?;
        let (base, sdir) = (dir.path().join("projects"), dir.path().join("sessions"));
        if case["missing_sessions"].as_bool() != Some(true) {
            fs::create_dir_all(&sdir)?;
        }
        write_sessions(&sdir, arr(&case["sessions"]), &c["dead_pid"])?;
        for t in arr(&case["transcripts"]) {
            let d = base.join(s(&t["dir"]));
            fs::create_dir_all(&d)?;
            let p = d.join(format!("{}.jsonl", s(&t["sid"])));
            let line = serde_json::json!({"type": "assistant", "message": {"usage": {"input_tokens": t["tokens"]}}});
            fs::write(&p, format!("{line}\n"))?;
            set_mtime(&p, f(&t["mtime"]))?;
        }
        let vars: Vec<(&str, String)> = case["window"]
            .as_str()
            .map(|w| ("PANE_CTX_WINDOW", w.to_owned()))
            .into_iter()
            .collect();
        let cfg = config(dir.path(), &vars)?;
        let mut ctx = TranscriptCtx::new();
        let results = arr(&case["queries"])
            .iter()
            .map(|q| Ok((s(q).to_owned(), ctx.pct(&cfg, s(q))?)))
            .collect::<Result<Vec<_>, trusty_architect::SelfCtxError>>()?;
        rows.push(to_python_json(&CtxRow { case: s(&case["case"]), results }, true)?);
    }
    assert_golden("transcript_ctx.jsonl", &rows)
}

#[test]
fn alerts_jsonl_lines_match_python_bytes() -> TestResult {
    let c = corpus()?;
    let alerts: Vec<Alert> = arr(&c["alert_rows"])
        .iter()
        .map(|r| alert(s(&r[0]), s(&r[1]), s(&r[2]), s(&r[3]), s(&r[4]), f(&r[5])))
        .collect();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("alerts.jsonl");
    append_alerts(&path, &alerts[..2])?;
    append_alerts(&path, &alerts[2..])?;
    append_alerts(&path, &[])?;
    assert_eq!(fs::read(&path)?, fs::read(fixture_dir().join("alerts_lines.jsonl"))?);
    Ok(())
}
