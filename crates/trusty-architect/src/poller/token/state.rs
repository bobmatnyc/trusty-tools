//! `token_watch.py`'s state file, thresholds and hourly rollup.
//!
//! Why: the live Python poller and this port share
//! `inbox/token-watch-state.json` (offsets, counters, fired fingerprints), so
//! the port reads and writes the same shape, keeping keys it does not know.
//! What: [`TokenState`] mirrors `new_state()`; [`load`] is `load_state` (a
//! missing or unparsable file is a fresh state, a wrong shape is an error),
//! [`save`] writes through a temp file and rename. [`Thresholds`] is
//! `DEFAULTS` overlaid with `local/usage/token-thresholds.json`.
//! [`flush_hours`] appends completed-hour rows once per key; [`prune`] is
//! `prune`.
//! Test: `hourly_rollup_idempotent`, `rollup_row_flags_fable_cost_unknown`,
//! `effort_and_thinking_in_rollup_and_legacy_key` (token/tests.rs).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::clock::hour_of;
use super::units::family;

/// `FIRED_TTL`: a fired fingerprint is forgotten after a day.
pub const FIRED_TTL: f64 = 86_400.0;

/// One transcript file's read position and rolling counters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileState {
    /// `main` or `sub`.
    pub kind: String,
    /// The project directory name.
    pub project: String,
    /// The session id.
    pub session: String,
    /// The subagent id, empty for a main transcript.
    pub agent_id: String,
    /// Timestamp of the last turn read.
    pub last_ts: Option<f64>,
    /// The last 20 turns' `[read, create, input]`.
    #[serde(default)]
    pub recent: Vec<[u64; 3]>,
    /// Polling-turn times in the last 30 min.
    #[serde(default)]
    pub polls: Vec<f64>,
    /// A subagent's `agentType`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// The tmux session name last resolved for `session`.
    // See ar-T9u parity window 5: E.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// When a scan last stat'ed the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<f64>,
    /// The inode at the last read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ino: Option<u64>,
    /// The size at the last read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// The byte offset read up to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off: Option<u64>,
    /// The first read started at byte 0 (a session start, not a tail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off0: Option<bool>,
    /// A main transcript's first turn was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<bool>,
    /// The session is the triage gate's own `claude -p` call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage_gate: Option<bool>,
    /// A subagent ended on an `end_turn` assistant turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
    /// Keys this port does not know, kept as they were.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One open hourly accumulator.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenHour {
    /// Turns.
    pub turns: u64,
    /// Thinking tokens.
    #[serde(default)]
    pub think: u64,
    /// Context per turn.
    pub ctx: Vec<u64>,
    /// Output tokens.
    pub out: u64,
    /// Cache reads.
    pub rd: u64,
    /// Cache creation.
    pub cc: u64,
    /// Uncached input.
    #[serde(rename = "in")]
    pub inp: u64,
    /// Expected rewrites.
    pub rw_exp: u64,
    /// Unexpected rewrites.
    pub rw_unexp: u64,
    /// Polling turns.
    pub polls: u64,
    /// Cost units.
    pub units: f64,
}

/// One fleet hour.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetHour {
    /// Cost units.
    pub units: f64,
    /// Opus units.
    pub opus: f64,
    /// Polling turns.
    pub polls: u64,
}

/// `inbox/token-watch-state.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenState {
    /// Per transcript path.
    #[serde(default)]
    pub files: BTreeMap<String, FileState>,
    /// Message ids already counted (newest 5000).
    #[serde(default)]
    pub seen: Vec<String>,
    /// Open hourly accumulators by `hour|project|session|agent|model|effort`.
    #[serde(default)]
    pub open: BTreeMap<String, OpenHour>,
    /// Fleet totals by hour.
    #[serde(default)]
    pub fleet: BTreeMap<String, FleetHour>,
    /// Fired dedupe keys and when.
    #[serde(default)]
    pub fired: BTreeMap<String, f64>,
    /// Main-session starts per project: `[ts, session]`.
    #[serde(default)]
    pub starts: BTreeMap<String, Vec<(f64, String)>>,
    /// Unexpected misses per `session|hour`.
    #[serde(default)]
    pub miss_count: BTreeMap<String, u64>,
    /// Lines that failed to parse in the last scan.
    #[serde(default)]
    pub parse_errors: u64,
    /// Lines read in the last scan.
    #[serde(default)]
    pub lines: u64,
    /// Keys this port does not know, kept as they were.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The state file has JSON of the wrong shape (the prototype raises and the
/// cycle's token step yields nothing).
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Read or write failed.
    #[error("{what}: {source}")]
    Io {
        /// What was done.
        what: String,
        /// The cause.
        source: io::Error,
    },
    /// The JSON does not have the state's shape.
    #[error("{what}: {detail}")]
    Shape {
        /// The file.
        what: String,
        /// The serde message.
        detail: String,
    },
}

/// `load_state`: a missing or unparsable file is a fresh state.
///
/// # Errors
/// [`StateError::Shape`] when the file is JSON of another shape.
pub fn load(path: &Path) -> Result<TokenState, StateError> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(TokenState::default());
    };
    let Ok(doc) = serde_json::from_str::<Value>(&text) else {
        return Ok(TokenState::default());
    };
    serde_json::from_value(doc).map_err(|e| StateError::Shape {
        what: path.display().to_string(),
        detail: e.to_string(),
    })
}

/// `save_state`: the parent is created, the write is a temp file and rename.
///
/// # Errors
/// [`StateError::Io`] when the file cannot be written.
pub fn save(path: &Path, st: &TokenState) -> Result<(), StateError> {
    let io_err = |source| StateError::Io {
        what: format!("save {}", path.display()),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_err)?;
    }
    let text = serde_json::to_string(st).map_err(|e| StateError::Shape {
        what: path.display().to_string(),
        detail: e.to_string(),
    })?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(io_err)
}

/// `DEFAULTS`, overlaid by the thresholds file.
#[derive(Debug, Clone, PartialEq)]
pub struct Thresholds {
    /// Main-session context buckets.
    pub ctx_main: Vec<i64>,
    /// Subagent context buckets.
    pub ctx_sub: Vec<i64>,
    /// Cache-hit ratio floor over 20 turns.
    pub hit_low: f64,
    /// Unexpected rewrite size.
    pub miss_unexpected: f64,
    /// Idle rewrite size (Opus).
    pub miss_idle: f64,
    /// Polling turns in 30 min.
    pub poll_agent: f64,
    /// Fleet polling rate over the 7-day median.
    pub poll_rate_factor: f64,
    /// Fleet polling-rate floor.
    pub poll_rate_min: f64,
    /// Opus share of units over 3 h.
    pub opus_share: f64,
    /// Concurrent live subagents.
    pub fanout_cap: f64,
    /// Hourly units over the 24 h mean.
    pub burn_factor: f64,
    /// Main-session starts per project in 3 h.
    pub session_start_churn: f64,
    /// Main-session cache TTL, seconds.
    pub ttl_main: f64,
    /// Subagent cache TTL, seconds.
    pub ttl_sub: f64,
    /// Share of a turn's context that is a rewrite.
    pub rewrite_frac: f64,
    /// Least context for a rewrite or hit rule.
    pub rewrite_min_ctx: f64,
    /// Unexpected misses alerted per session hour.
    pub miss_burst_cap: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            ctx_main: vec![200_000, 315_000, 350_000, 400_000],
            ctx_sub: vec![150_000, 250_000],
            hit_low: 0.90,
            miss_unexpected: 100_000.0,
            miss_idle: 150_000.0,
            poll_agent: 12.0,
            poll_rate_factor: 2.0,
            poll_rate_min: 60.0,
            opus_share: 0.80,
            fanout_cap: 3.0,
            burn_factor: 1.5,
            session_start_churn: 6.0,
            ttl_main: 3600.0,
            ttl_sub: 300.0,
            rewrite_frac: 0.5,
            rewrite_min_ctx: 50_000.0,
            miss_burst_cap: 3.0,
        }
    }
}

impl Thresholds {
    /// `thresholds(path)`: the defaults, overlaid by the file's numeric keys;
    /// a missing or bad file, or a key of the wrong type, keeps the default.
    pub fn load(path: &Path) -> Self {
        let mut t = Self::default();
        let doc: Option<Map<String, Value>> = fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        let Some(doc) = doc else { return t };
        let list =
            |v: &Value| -> Option<Vec<i64>> { v.as_array()?.iter().map(Value::as_i64).collect() };
        for (k, v) in &doc {
            if let Some(l) = list(v) {
                match k.as_str() {
                    "ctx_main" => t.ctx_main = l,
                    "ctx_sub" => t.ctx_sub = l,
                    _ => {}
                }
                continue;
            }
            let Some(n) = v.as_f64() else { continue };
            let slot = match k.as_str() {
                "hit_low" => &mut t.hit_low,
                "miss_unexpected" => &mut t.miss_unexpected,
                "miss_idle" => &mut t.miss_idle,
                "poll_agent" => &mut t.poll_agent,
                "poll_rate_factor" => &mut t.poll_rate_factor,
                "poll_rate_min" => &mut t.poll_rate_min,
                "opus_share" => &mut t.opus_share,
                "fanout_cap" => &mut t.fanout_cap,
                "burn_factor" => &mut t.burn_factor,
                "session_start_churn" => &mut t.session_start_churn,
                "ttl_main" => &mut t.ttl_main,
                "ttl_sub" => &mut t.ttl_sub,
                "rewrite_frac" => &mut t.rewrite_frac,
                "rewrite_min_ctx" => &mut t.rewrite_min_ctx,
                "miss_burst_cap" => &mut t.miss_burst_cap,
                _ => continue,
            };
            *slot = n;
        }
        t
    }
}

/// `pct(vals, p)`: the value at rank `int(p * n)`, 0 when empty.
fn pct(vals: &[u64], p: f64) -> u64 {
    let mut s = vals.to_vec();
    s.sort_unstable();
    let i = ((p * s.len() as f64) as usize).min(s.len().saturating_sub(1));
    s.get(i).copied().unwrap_or(0)
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// `split_open_key`: six parts; a five-part key gets effort `unknown`.
pub fn split_open_key(key: &str) -> [String; 6] {
    let mut p: Vec<String> = key.splitn(6, '|').map(str::to_owned).collect();
    while p.len() < 5 {
        p.push(String::new());
    }
    if p.len() == 5 {
        p.push("unknown".to_owned());
    }
    let mut it = p.into_iter();
    std::array::from_fn(|_| it.next().unwrap_or_default())
}

/// `rollup_row`: one `token-hourly.jsonl` row.
pub fn rollup_row(key: &str, o: &OpenHour) -> Value {
    let [hour, project, session, atype, model, effort] = split_open_key(key);
    let tot = o.rd + o.cc + o.inp;
    let hit = (tot > 0).then(|| (o.rd as f64 / tot as f64 * 10_000.0).round() / 10_000.0);
    let fam = family(&model);
    let units = round3(o.units);
    serde_json::json!({
        "hour": hour, "project": project, "session": session, "agent_type": atype,
        "model": model, "effort": effort, "thinking_tokens": o.think, "turns": o.turns,
        "ctx_p50": pct(&o.ctx, 0.5), "ctx_p90": pct(&o.ctx, 0.9), "out_tokens": o.out,
        "hit_ratio": hit, "rewrites_expected": o.rw_exp, "rewrites_unexpected": o.rw_unexp,
        "poll_turns": o.polls, "units": units,
        "units_opus": if fam == "opus" { units } else { 0.0 },
        "units_unknown": super::units::unpriced(fam),
    })
}

/// `flush_hours`: appends a row per completed-hour key not already in the
/// file and drops every completed key from `open`; returns rows written.
///
/// # Errors
/// [`StateError::Io`] when the file cannot be appended.
pub fn flush_hours(st: &mut TokenState, now: f64, path: &Path) -> Result<usize, StateError> {
    let cur = hour_of(now);
    let due: Vec<String> = st
        .open
        .keys()
        .filter(|k| k.split('|').next().unwrap_or("") < cur.as_str())
        .cloned()
        .collect();
    if due.is_empty() {
        return Ok(0);
    }
    let mut have = BTreeSet::new();
    for line in fs::read_to_string(path).unwrap_or_default().lines() {
        let Ok(r) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let s = |k: &str| r[k].as_str().unwrap_or("").to_owned();
        let effort = r["effort"].as_str().filter(|e| !e.is_empty());
        let parts = [
            s("hour"),
            s("project"),
            s("session"),
            s("agent_type"),
            s("model"),
        ];
        have.insert(format!(
            "{}|{}",
            parts.join("|"),
            effort.unwrap_or("unknown")
        ));
    }
    let io_err = |source| StateError::Io {
        what: format!("append {}", path.display()),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_err)?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io_err)?;
    let mut n = 0;
    for k in due {
        if let Some(o) = st.open.remove(&k)
            && !have.contains(&k)
        {
            writeln!(f, "{}", rollup_row(&k, &o)).map_err(io_err)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Drops completed hours from `open` without writing them: a shadow run's
/// own state must not grow, and it never writes the hourly file.
pub fn drop_done_hours(st: &mut TokenState, now: f64) {
    let cur = hour_of(now);
    st.open
        .retain(|k, _| k.split('|').next().unwrap_or("") >= cur.as_str());
}

/// `prune`: the last 5000 ids, files seen within a day, fired keys within
/// [`FIRED_TTL`], eight days of fleet hours, two hours of miss counts.
pub fn prune(st: &mut TokenState, now: f64) {
    let skip = st.seen.len().saturating_sub(5000);
    st.seen.drain(..skip);
    st.files
        .retain(|_, f| now - f.seen_at.unwrap_or(now) < 86_400.0);
    st.fired.retain(|_, t| now - *t < FIRED_TTL);
    let week = hour_of(now - 8.0 * 86_400.0);
    st.fleet.retain(|h, _| h.as_str() >= week.as_str());
    let two = hour_of(now - 7200.0);
    st.miss_count
        .retain(|k, _| k.rsplit('|').next().unwrap_or("") >= two.as_str());
}
