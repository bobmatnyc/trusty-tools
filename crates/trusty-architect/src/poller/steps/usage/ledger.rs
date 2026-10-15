//! The usage ledger, the statusline cache and the logged-in account: the I/O
//! half of `usage.py` that the poller runs.
//!
//! Why: the poller samples account usage into `local/usage/ledger.jsonl`
//! every cycle; the ledger's bytes are read back by the prototype's CLI and
//! by `usage-probe.sh`'s ingest, so they must stay `json.dumps` lines (#9801).
//! What: [`Reading`] is one ledger row kept as its JSON object, with the
//! typed reads `usage.py` makes; [`read_ledger`] / [`rows_from_values`] keep
//! rows with a truthy `account` and `ts`, sorted by `ts`; [`append_ledger`];
//! [`read_cache`] (the statusline cache), [`detect_account`] (claude.json)
//! and [`sample_cache`] / [`record_reading`], which append a reading unless
//! it repeats the last one within [`WRITE_EVERY`].
//! Test: `tests/parity_usage.rs` (`read_cache_matches_python_goldens`,
//! `poll_step_matches_python_goldens`), `tests/usage_ported.rs`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use super::UsagePaths;
use super::utc::UtcTime;
use crate::error::UsageError;
use crate::pyjson::to_python_json;
use crate::pystr::strip;

/// A statusline cache older than this many seconds is not a reading.
pub const CACHE_MAX_AGE: f64 = 30.0 * 60.0;
/// An unchanged reading is re-recorded after this many seconds.
pub const WRITE_EVERY: f64 = 30.0 * 60.0;

/// A usage percent as the ledger holds it: `int` or `float`, printed as
/// Python prints each (`100` vs `100.0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pct {
    /// A JSON integer.
    Int(i64),
    /// A JSON float.
    Float(f64),
}

impl Pct {
    /// The value as a float, for comparisons and arithmetic.
    pub fn get(self) -> f64 {
        match self {
            // Percents are small; the conversion is exact.
            Self::Int(n) => n as f64,
            Self::Float(x) => x,
        }
    }

    /// Python's `str()`.
    pub fn py_str(self) -> String {
        match self {
            Self::Int(n) => n.to_string(),
            Self::Float(x) => crate::poller::pyfmt::py_float_repr(x),
        }
    }
}

impl Serialize for Pct {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Int(n) => s.serialize_i64(*n),
            Self::Float(x) => s.serialize_f64(*x),
        }
    }
}

fn wrong(key: &str, value: &Value, want: &'static str) -> UsageError {
    UsageError::WrongType {
        key: key.to_owned(),
        value: value.to_string(),
        want,
    }
}

/// Python truthiness of a JSON value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `==` on two JSON values: numbers by value (`84 == 84.0`).
pub(crate) fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

/// A JSON number as `Pct`; `None` for null.
///
/// # Errors
/// [`UsageError::WrongType`] for anything else. The prototype accepts a
/// numeric string here and compares a bool as an int; the port refuses both.
pub(crate) fn pct_of(key: &str, v: &Value) -> Result<Option<Pct>, UsageError> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => Ok(Some(match n.as_i64() {
            Some(i) => Pct::Int(i),
            None => Pct::Float(n.as_f64().ok_or_else(|| wrong(key, v, "a number"))?),
        })),
        _ => Err(wrong(key, v, "a number")),
    }
}

/// `usage.parse_iso` on a JSON value: falsy is `None`, a string parses.
///
/// # Errors
/// [`UsageError::BadTime`] for a string that does not parse; `WrongType`
/// for a truthy value that is not a string.
pub(crate) fn time_of(key: &str, v: &Value) -> Result<Option<UtcTime>, UsageError> {
    match v {
        Value::String(s) if !s.is_empty() => UtcTime::parse_iso(s).map(Some),
        v if !truthy(v) => Ok(None),
        _ => Err(wrong(key, v, "an ISO time string")),
    }
}

/// One ledger row: its JSON object, and its `account` and `ts` strings.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// `account`.
    pub account: String,
    /// `ts`, as written.
    pub ts: String,
    /// The whole row.
    pub row: Map<String, Value>,
}

impl Reading {
    fn get(&self, key: &str) -> &Value {
        self.row.get(key).unwrap_or(&Value::Null)
    }

    /// `parse_iso(r["ts"])`.
    ///
    /// # Errors
    /// [`UsageError::BadTime`] when `ts` does not parse.
    pub fn time(&self) -> Result<UtcTime, UsageError> {
        UtcTime::parse_iso(&self.ts)
    }

    /// The row's `key` percent (`pct5h`, `pctwk`).
    ///
    /// # Errors
    /// As [`pct_of`].
    pub fn pct(&self, key: &str) -> Result<Option<Pct>, UsageError> {
        pct_of(key, self.get(key))
    }

    /// True when `r.get(key) is not None`.
    pub fn has(&self, key: &str) -> bool {
        !self.get(key).is_null()
    }

    /// The row's `key` reset time (`reset5h`, `resetwk`).
    ///
    /// # Errors
    /// As [`time_of`].
    pub fn reset(&self, key: &str) -> Result<Option<UtcTime>, UsageError> {
        time_of(key, self.get(key))
    }

    /// True unless `source` is the string `manual`.
    pub fn not_manual(&self) -> bool {
        self.get("source").as_str() != Some("manual")
    }
}

/// `read_ledger`'s filter and sort over parsed lines: objects with a truthy
/// `account` and `ts`, stably sorted by `ts`.
///
/// # Errors
/// [`UsageError::WrongType`] for a truthy `account` or `ts` that is not a
/// string (the prototype keeps such a row and fails or misbehaves later).
pub fn rows_from_values(values: Vec<Value>) -> Result<Vec<Reading>, UsageError> {
    let mut rows = Vec::new();
    for v in values {
        let Value::Object(row) = v else { continue };
        let text = |key: &str| match row.get(key) {
            Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
            Some(v) if truthy(v) => Err(wrong(key, v, "a string")),
            _ => Ok(None),
        };
        if let (Some(account), Some(ts)) = (text("account")?, text("ts")?) {
            rows.push(Reading { account, ts, row });
        }
    }
    rows.sort_by(|a, b| a.ts.cmp(&b.ts));
    Ok(rows)
}

/// `read_ledger`: a missing or unreadable file is no rows; a line that is not
/// JSON is skipped. Lines split on `\n`, `\r\n` and `\r`, as Python's text
/// mode reads them.
///
/// # Errors
/// [`UsageError::NotUtf8`] for a file that is not UTF-8 (Python raises
/// `UnicodeDecodeError` there); as [`rows_from_values`].
pub fn read_ledger(path: &Path) -> Result<Vec<Reading>, UsageError> {
    let Ok(bytes) = fs::read(path) else {
        return Ok(Vec::new());
    };
    let text = String::from_utf8(bytes).map_err(|_| UsageError::NotUtf8(path.to_owned()))?;
    let values = text
        .replace("\r\n", "\n")
        .split(['\n', '\r'])
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    rows_from_values(values)
}

/// `append_ledger`: creates the directory, appends one `json.dumps` line
/// (`ensure_ascii=False`).
///
/// # Errors
/// [`UsageError::Io`] when the directory or file cannot be written.
pub fn append_ledger<T: Serialize + ?Sized>(path: &Path, rec: &T) -> Result<(), UsageError> {
    let io = |what| {
        move |source| UsageError::Io {
            what,
            path: path.to_owned(),
            source,
        }
    };
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(io("create the directory of"))?;
    }
    let mut line = to_python_json(rec, false).map_err(|e| io("encode a row for")(e.into()))?;
    line.push('\n');
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(io("append to"))
}

/// The newest row of each account, in order of each account's first row
/// (a Python dict built by assignment).
pub(crate) fn latest_by_account(rows: &[Reading]) -> Vec<(&str, &Reading)> {
    let mut out: Vec<(&str, &Reading)> = Vec::new();
    for r in rows {
        match out.iter_mut().find(|(a, _)| *a == r.account) {
            Some(slot) => slot.1 = r,
            None => out.push((&r.account, r)),
        }
    }
    out
}

/// The newest row of `account`.
pub(crate) fn latest<'a>(rows: &'a [Reading], account: &str) -> Option<&'a Reading> {
    rows.iter().rev().find(|r| r.account == account)
}

/// The figures a cache sample carries (`read_cache`'s `data`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CacheData {
    /// `five_hour.used_percentage`, as read.
    pub pct5h: Value,
    /// `five_hour.resets_at`, as an ISO time.
    pub reset5h: Option<String>,
    /// `seven_day.used_percentage`, as read.
    pub pctwk: Value,
    /// `seven_day.resets_at`, as an ISO time.
    pub resetwk: Option<String>,
    /// Always `{}` from the cache.
    pub weekly_models: Map<String, Value>,
}

/// One statusline-cache sample.
#[derive(Debug, Clone, PartialEq)]
pub struct CacheReading {
    /// Line 1, stripped; `None` when blank.
    pub account: Option<String>,
    /// The file's mtime.
    pub mtime: UtcTime,
    /// The figures.
    pub data: CacheData,
}

/// `_epoch_or_iso`: a number is epoch seconds (milliseconds above 1e11), a
/// string an ISO time; null or missing is `None`.
fn epoch_or_iso(key: &str, v: &Value) -> Result<Option<String>, UsageError> {
    let t = match v {
        Value::Null => None,
        Value::Number(n) => {
            let x = n.as_f64().ok_or_else(|| wrong(key, v, "a number"))?;
            Some(UtcTime::from_unix(if x > 1e11 { x / 1000.0 } else { x })?)
        }
        Value::String(s) => time_of(key, &Value::String(s.clone()))?,
        _ => return Err(UsageError::BadTime(v.to_string())),
    };
    Ok(t.map(UtcTime::iso))
}

/// `read_cache`: line 1 the account, the rest Claude Code's statusLine JSON.
/// `None` when the file is missing, unreadable, not JSON, older than
/// `max_age` seconds, or without a used percentage.
///
/// # Errors
/// Where the prototype raises: a truthy `five_hour`/`seven_day` that is not
/// an object, or a `resets_at` that is not a time.
pub fn read_cache(
    now: UtcTime,
    path: &Path,
    max_age: f64,
) -> Result<Option<CacheReading>, UsageError> {
    let Ok(modified) = fs::metadata(path).and_then(|m| m.modified()) else {
        return Ok(None);
    };
    let unix = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    };
    let mtime = UtcTime::from_unix(unix)?;
    let Some(text) = fs::read(path).ok().and_then(|b| String::from_utf8(b).ok()) else {
        return Ok(None);
    };
    let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
    let Ok(Value::Object(top)) = serde_json::from_str::<Value>(rest) else {
        return Ok(None);
    };
    let Some(Value::Object(rl)) = top.get("rate_limits") else {
        return Ok(None);
    };
    if now.seconds_since(mtime) > max_age {
        return Ok(None);
    }
    let window = |key: &str| match rl.get(key) {
        Some(Value::Object(m)) => Ok(m.clone()),
        Some(v) if truthy(v) => Err(wrong(key, v, "an object")),
        _ => Ok(Map::new()),
    };
    let (five, week) = (window("five_hour")?, window("seven_day")?);
    let pct = |m: &Map<String, Value>| m.get("used_percentage").cloned().unwrap_or(Value::Null);
    let at = |m: &Map<String, Value>| m.get("resets_at").cloned().unwrap_or(Value::Null);
    if pct(&five).is_null() && pct(&week).is_null() {
        return Ok(None);
    }
    let data = CacheData {
        pct5h: pct(&five),
        reset5h: epoch_or_iso("five_hour.resets_at", &at(&five))?,
        pctwk: pct(&week),
        resetwk: epoch_or_iso("seven_day.resets_at", &at(&week))?,
        weekly_models: Map::new(),
    };
    let account = strip(first);
    Ok(Some(CacheReading {
        account: (!account.is_empty()).then(|| account.to_owned()),
        mtime,
        data,
    }))
}

/// `detect_account`: `oauthAccount.emailAddress` in claude.json, or `None`
/// when the file, the key or a non-empty string is missing.
pub fn detect_account(path: &Path) -> Option<String> {
    let v: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let email = v.get("oauthAccount")?.get("emailAddress")?.as_str()?;
    (!email.is_empty()).then(|| email.to_owned())
}

/// One ledger line in `make_record`'s key order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LedgerRecord {
    /// When it was read.
    pub ts: String,
    /// The account.
    pub account: String,
    /// 5-hour percent, as read.
    pub pct5h: Value,
    /// 5-hour reset.
    pub reset5h: Option<String>,
    /// Weekly percent, as read.
    pub pctwk: Value,
    /// Weekly reset.
    pub resetwk: Option<String>,
    /// Per-model weekly percents.
    pub weekly_models: Map<String, Value>,
    /// `statusline-cache`, `usage-cmd` or `manual`.
    pub source: String,
}

/// `record_reading` with `force=False` and no note: appends `data` for
/// `account` unless it repeats that account's last reading within
/// [`WRITE_EVERY`].
///
/// # Errors
/// A stored `ts` that does not parse; the append failing.
pub fn record_reading(
    ledger: &Path,
    account: &str,
    source: &str,
    when: UtcTime,
    data: &CacheData,
    rows: &[Reading],
) -> Result<Option<LedgerRecord>, UsageError> {
    if let Some(prev) = latest(rows, account) {
        let same = ["pct5h", "pctwk", "reset5h", "resetwk"].iter().all(|k| {
            let new = match *k {
                "pct5h" => data.pct5h.clone(),
                "pctwk" => data.pctwk.clone(),
                "reset5h" => data.reset5h.clone().map_or(Value::Null, Value::String),
                _ => data.resetwk.clone().map_or(Value::Null, Value::String),
            };
            py_eq(prev.get(k), &new)
        });
        if same && when.seconds_since(prev.time()?) < WRITE_EVERY {
            return Ok(None);
        }
    }
    let rec = LedgerRecord {
        ts: when.iso(),
        account: account.to_owned(),
        pct5h: data.pct5h.clone(),
        reset5h: data.reset5h.clone(),
        pctwk: data.pctwk.clone(),
        resetwk: data.resetwk.clone(),
        weekly_models: data.weekly_models.clone(),
        source: source.to_owned(),
    };
    append_ledger(ledger, &rec)?;
    Ok(Some(rec))
}

/// `sample_cache`: one no-typing sample from the statusline cache, recorded
/// when the cache is fresh, written under the logged-in account, and not a
/// carry-over of another account's windows right after a `/login`. `rows`
/// is the ledger already read, else it is read here.
///
/// # Errors
/// As [`read_cache`], [`read_ledger`] and [`record_reading`].
pub fn sample_cache(
    paths: &UsagePaths,
    now: UtcTime,
    rows: Option<&[Reading]>,
) -> Result<Option<LedgerRecord>, UsageError> {
    let account = detect_account(&paths.claude_json);
    let got = read_cache(now, &paths.cache, CACHE_MAX_AGE)?;
    let (Some(account), Some(got)) = (account, got) else {
        return Ok(None);
    };
    if got.account.as_deref() != Some(account.as_str()) {
        return Ok(None);
    }
    let read;
    let rows = match rows {
        Some(r) => r,
        None => {
            read = read_ledger(&paths.ledger)?;
            &read
        }
    };
    if same_windows_as_other_account(rows, &account, &got.data) {
        return Ok(None);
    }
    record_reading(
        &paths.ledger,
        &account,
        "statusline-cache",
        now,
        &got.data,
        rows,
    )
}

/// True when the newest reading of a different account has the same reset
/// times: right after a `/login` the cache can still carry the old figures.
fn same_windows_as_other_account(rows: &[Reading], account: &str, data: &CacheData) -> bool {
    let Some(other) = rows.iter().rev().find(|r| r.account != account) else {
        return false;
    };
    let as_value = |t: &Option<String>| t.clone().map_or(Value::Null, Value::String);
    data.resetwk.is_some()
        && py_eq(other.get("reset5h"), &as_value(&data.reset5h))
        && py_eq(other.get("resetwk"), &as_value(&data.resetwk))
}
