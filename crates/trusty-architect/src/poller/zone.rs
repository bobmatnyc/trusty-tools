//! The hourly kernel-zone watch: macOS 26 leaks `data.kalloc.1024` (Bob D73,
//! 2026-10-25; df-T7i P1b).
//!
//! Why: the leak grows until the host must reboot; the poller logs one
//! reading an hour and tells the Architect once per threshold per boot.
//! What: [`zone_alerts`] reads the in-use element count (`zprint` field 7)
//! and the boot time (`sysctl -n kern.boottime`) at most every
//! [`ZONE_EVERY`] seconds, appends `{ts, elements, uptime_s}` to the zone
//! log, and raises one `host_zone` alert per threshold per boot: 8M elements,
//! 12M (reboot now; replaces the 8M one), and uptime over 14 days. The fired
//! thresholds live in `state["zone"]["fired"]`, reset when the boot time
//! changes. Both readings missing is a log line and no alert; a log write
//! failure is a log line. Not run in a dry or shadow run.
//! Test: `zone_alerts_match_python_goldens` (tests/parity_p1b.rs),
//! `test_reads_field_seven_of_zprint`,
//! `test_hourly_log_and_threshold_dedup_per_boot`,
//! `test_uptime_over_14_days_alerts_once` (tests/p1b_ported.rs).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::steps::scripts::Subprocess;
use crate::classify::alerts::iso;
use crate::classify::{Alert, alert};

/// Seconds between readings.
pub const ZONE_EVERY: f64 = 3600.0;
/// In-use elements at which the owner is told.
pub const ZONE_WARN: u64 = 8_000_000;
/// In-use elements at which a reboot is due now.
pub const ZONE_REBOOT: u64 = 12_000_000;
/// Uptime after which a planned reboot is due.
pub const ZONE_UPTIME_MAX: i64 = 14 * 86_400;

/// `read_zone_elements`: field 7 of the `data.kalloc.1024` row, or `None`.
pub fn parse_zprint(out: &str) -> Option<u64> {
    out.lines().find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        (f.first() == Some(&"data.kalloc.1024")
            && f.len() > 6
            && f[6].bytes().all(|b| b.is_ascii_digit())
            && !f[6].is_empty())
        .then(|| f[6].parse().ok())
        .flatten()
    })
}

/// `read_boot_time`: the first `sec = N` of `kern.boottime`, or `None`.
pub fn parse_boottime(out: &str) -> Option<i64> {
    let i = out.find("sec = ")?;
    let digits: String = out[i + 6..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The two readings through `runner`; a failed command reads as `None`.
pub fn read_zone(runner: &dyn Subprocess) -> (Option<u64>, Option<i64>) {
    let run = |argv: &[&str], secs| {
        let argv: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
        runner
            .output(&argv, Duration::from_secs(secs))
            .ok()
            .map(|o| o.stdout)
    };
    let elements = run(&["zprint"], 30).and_then(|o| parse_zprint(&o));
    let boot = run(&["sysctl", "-n", "kern.boottime"], 5).and_then(|o| parse_boottime(&o));
    (elements, boot)
}

/// Python's `f"{n:,}"`.
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `zone_alerts(state, now, reader, boot, path)`; `read` supplies the two
/// readings (called only when a reading is due). Returns the alerts and the
/// `poll.log` lines.
pub fn zone_alerts(
    other: &mut Map<String, Value>,
    now: f64,
    read: impl FnOnce() -> (Option<u64>, Option<i64>),
    log_path: &Path,
) -> (Vec<Alert>, Vec<String>) {
    let mut z = match other.remove("zone") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let out = zone_step(&mut z, now, read, log_path);
    other.insert("zone".to_owned(), Value::Object(z));
    out
}

fn zone_step(
    z: &mut Map<String, Value>,
    now: f64,
    read: impl FnOnce() -> (Option<u64>, Option<i64>),
    log_path: &Path,
) -> (Vec<Alert>, Vec<String>) {
    let last = z.get("last").and_then(Value::as_f64).unwrap_or(0.0);
    if now - last < ZONE_EVERY {
        return (Vec::new(), Vec::new());
    }
    z.insert("last".to_owned(), json!(now));
    let (elements, bt) = read();
    let uptime = bt.map(|b| (now - b as f64).trunc() as i64);
    if elements.is_none() && uptime.is_none() {
        return (Vec::new(), vec!["zone reading unavailable".to_owned()]);
    }
    let mut log = Vec::new();
    // The prototype's key order: ts, elements, uptime_s.
    let num = |v: Option<String>| v.unwrap_or_else(|| "null".to_owned());
    let line = format!(
        "{{\"ts\": \"{}\", \"elements\": {}, \"uptime_s\": {}}}",
        iso(now),
        num(elements.map(|e| e.to_string())),
        num(uptime.map(|u| u.to_string()))
    );
    let appended = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| f.write_all(format!("{line}\n").as_bytes()));
    if let Err(e) = appended {
        log.push(format!("zone log ERROR {e}"));
    }
    let boot = bt.map_or(Value::Null, Value::from);
    if z.get("boot") != Some(&boot) {
        z.insert("boot".to_owned(), boot);
        z.insert("fired".to_owned(), json!([]));
    }
    let mut due = Vec::new();
    match elements {
        Some(e) if e >= ZONE_REBOOT => due.push((
            "reboot",
            format!(
                "data.kalloc.1024 at {} elements (>= 12M): reboot now",
                thousands(e)
            ),
        )),
        Some(e) if e >= ZONE_WARN => due.push((
            "warn",
            format!(
                "data.kalloc.1024 at {} elements (>= 8M): alert Bob",
                thousands(e)
            ),
        )),
        _ => {}
    }
    if let Some(u) = uptime.filter(|u| *u > ZONE_UPTIME_MAX) {
        due.push((
            "uptime",
            format!(
                "uptime {:.1} days (> 14): planned reboot due",
                u as f64 / 86_400.0
            ),
        ));
    }
    let bt_text = bt.map_or_else(|| "None".to_owned(), |b| b.to_string());
    let mut fired: Vec<Value> = z
        .get("fired")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for (name, detail) in due {
        if !fired.iter().any(|f| f.as_str() == Some(name)) {
            fired.push(name.into());
            out.push(alert(
                "host_zone",
                &format!("{name}-{bt_text}"),
                &detail,
                "",
                "",
                now,
            ));
        }
    }
    z.insert("fired".to_owned(), Value::Array(fired));
    (out, log)
}
