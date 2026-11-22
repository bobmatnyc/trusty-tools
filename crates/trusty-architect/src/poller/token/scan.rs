//! The per-file half of `token_watch.Scan`: `emit`, `turn` and `read_file`.
//!
//! Why: the per-turn rules (context buckets, cache-hit ratio, cache misses,
//! polling) must fire on the same turns, with the same fingerprints and
//! dedupe keys, as the deployed watcher's, since the live poller and this
//! port share one state file and parity joins on the fingerprint.
//! What: [`Scan::read_file`] reads a transcript from its saved offset (a
//! first sight reads the last 4 MiB, a rotated or shrunk file restarts),
//! skips a partial last line, marks the triage gate's own sessions and a
//! subagent's `done`, and hands each assistant line to [`Scan::turn`], which
//! counts each message id once and applies the rules. [`Scan::emit`] dedupes
//! on `scope|fingerprint` (24 h) and the rule's cooldown. Never reads message
//! text beyond the Bash command shape of a polling turn.
//! Test: `ctx_main_buckets_once`, `miss_idle_vs_unexpected`,
//! `miss_unexpected_burst_is_capped`, `poll_agent_fires_at_twelve`,
//! `hit_low`, `truncated_last_line_is_picked_up_later`,
//! `duplicate_message_id_counts_once`, `no_message_text_in_alerts_or_state`
//! (token/tests.rs).

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use super::clock::{hour_fp, hour_of, parse_ts};
use super::discover::LOOKBACK;
use super::state::{FileState, OpenHour, Thresholds, TokenState};
use super::units::{effort_of, family, num, thinking_of, turn_units};
use super::{TokenAlert, is_poll_command, project_label};
use crate::poller::ctx_bar::main_buckets;

/// `READ_CAP`: most bytes read from one file per scan.
const READ_CAP: u64 = 16 * 1024 * 1024;
/// `FIRST_TAIL`: a first sight reads at most the last this many bytes.
const FIRST_TAIL: u64 = 4 * 1024 * 1024;
/// The triage gate's `claude -p` marker (`TRIAGE_ENTRYPOINT`, `TRIAGE_PREFIX`).
pub const TRIAGE_ENTRYPOINT: &str = "sdk-cli";
/// See [`TRIAGE_ENTRYPOINT`].
pub const TRIAGE_PREFIX: &str = "You triage alerts from a fleet poller for a supervisor.";

/// `COOLDOWN`: seconds before a rule may fire again for one scope.
pub fn cooldown(rule: &str) -> f64 {
    match rule {
        "hit_low"
        | "poll_rate"
        | "opus_share"
        | "burn"
        | "session_start_churn"
        | "watcher_degraded" => 21_600.0,
        "fanout" => 3600.0,
        _ => 0.0,
    }
}

/// One scan: `Scan` in the prototype.
pub struct Scan<'a> {
    /// The state, changed in place.
    pub st: &'a mut TokenState,
    /// The thresholds.
    pub th: Thresholds,
    /// Scan time.
    pub now: f64,
    /// The transcript base, for relative paths in details.
    pub base: String,
    /// `sessionId` to tmux session.
    pub names: BTreeMap<String, String>,
    /// `il-schedule.json`, for the Architect's own `ctx_main` buckets.
    pub schedule: &'a Path,
    /// Alerts raised.
    pub alerts: Vec<TokenAlert>,
    seen: HashSet<String>,
    /// Lines read.
    pub lines: u64,
    /// Lines that failed to parse.
    pub errors: u64,
    min_ts: f64,
}

impl<'a> Scan<'a> {
    /// A scan over `st`.
    pub fn new(
        st: &'a mut TokenState,
        th: Thresholds,
        now: f64,
        base: String,
        names: BTreeMap<String, String>,
        schedule: &'a Path,
    ) -> Self {
        let seen = st.seen.iter().cloned().collect();
        Self {
            st,
            th,
            now,
            base,
            names,
            schedule,
            alerts: Vec::new(),
            seen,
            lines: 0,
            errors: 0,
            min_ts: 0.0,
        }
    }

    /// `label(f)`: the tmux session, else `<project>:<session[:8]>`.
    pub fn label(&self, f: &FileState) -> String {
        self.names.get(&f.session).cloned().unwrap_or_else(|| {
            let sid: String = f.session.chars().take(8).collect();
            format!("{}:{sid}", project_label(&f.project))
        })
    }

    /// `emit(f, rule, fp, detail, scope)`.
    pub fn emit(
        &mut self,
        f: Option<&FileState>,
        rule: &str,
        fp: &str,
        detail: &str,
        scope: Option<&str>,
    ) {
        let scope = scope
            .map(str::to_owned)
            .or_else(|| f.map(|f| f.session.clone()))
            .unwrap_or_default();
        let (key, rkey) = (format!("{scope}|{fp}"), format!("{scope}|rule:{rule}"));
        let cool = cooldown(rule);
        let last = self.st.fired.get(&rkey).copied().unwrap_or(0.0);
        if self.st.fired.contains_key(&key) || (cool > 0.0 && self.now - last < cool) {
            return;
        }
        self.st.fired.insert(key, self.now);
        self.st.fired.insert(rkey, self.now);
        self.alerts.push(TokenAlert {
            rule: rule.to_owned(),
            session: f.map_or(scope, |f| self.label(f)),
            fingerprint: fp.to_owned(),
            detail: format!("{rule}: {detail}"),
        });
    }

    fn rel(&self, path: &str) -> String {
        match path.strip_prefix(&self.base) {
            Some(r) => r.trim_start_matches('/').to_owned(),
            None => path.to_owned(),
        }
    }

    /// One assistant line: state, hourly accumulators, per-turn rules.
    pub fn turn(&mut self, path: &str, f: &mut FileState, d: &Value) {
        let msg = &d["message"];
        let u = &msg["usage"];
        let ts = d["timestamp"].as_str().and_then(parse_ts);
        let Some(ts) = ts.filter(|t| u.is_object() && *t >= self.min_ts) else {
            return;
        };
        let mid = match (&msg["id"], &d["uuid"]) {
            (Value::String(s), _) if !s.is_empty() => Some(s.clone()),
            (_, Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        if let Some(m) = &mid {
            if !self.seen.insert(m.clone()) {
                return;
            }
            self.st.seen.push(m.clone());
        }
        let model = msg["model"].as_str().unwrap_or("").to_owned();
        let (rd, cc, inp) = (
            num(u.get("cache_read_input_tokens")),
            num(u.get("cache_creation_input_tokens")),
            num(u.get("input_tokens")),
        );
        let (out_t, ctx) = (num(u.get("output_tokens")), rd + cc + inp);
        let gap = f.last_ts.map(|l| ts - l);
        f.last_ts = Some(ts);
        let units = turn_units(&model, u);
        let fam = family(&model);
        let is_sub = f.kind != "main";
        let atype = if is_sub {
            f.agent_type
                .clone()
                .unwrap_or_else(|| "subagent".to_owned())
        } else {
            "main".to_owned()
        };
        if !is_sub && f.started != Some(true) {
            f.started = Some(true);
            if f.off0 == Some(true) && f.triage_gate != Some(true) {
                let rows = self.st.starts.entry(f.project.clone()).or_default();
                rows.push((ts, f.session.clone()));
            }
        }
        let poll = is_sub && is_poll_turn(msg);
        f.recent.push([rd, cc, inp]);
        let excess = f.recent.len().saturating_sub(20);
        f.recent.drain(..excess);
        let th = self.th.clone();
        let rewrite = cc as f64 > th.rewrite_frac * ctx as f64 && ctx as f64 > th.rewrite_min_ctx;
        let ttl = if is_sub { th.ttl_sub } else { th.ttl_main };
        let (expected, unexpected) = match gap {
            Some(g) if rewrite => (g > ttl, g <= ttl),
            _ => (false, false),
        };
        let h = hour_of(ts);
        let k = [
            h.as_str(),
            &f.project,
            &f.session,
            &atype,
            &model,
            &effort_of(d),
        ]
        .join("|");
        let o = self.st.open.entry(k).or_insert_with(OpenHour::default);
        o.turns += 1;
        o.ctx.push(ctx);
        o.out += out_t;
        o.think += thinking_of(u);
        (o.rd, o.cc, o.inp) = (o.rd + rd, o.cc + cc, o.inp + inp);
        o.rw_exp += u64::from(expected);
        o.rw_unexp += u64::from(unexpected);
        o.polls += u64::from(poll);
        o.units += units;
        let fl = self.st.fleet.entry(h.clone()).or_default();
        fl.units += units;
        fl.opus += if fam == "opus" { units } else { 0.0 };
        fl.polls += u64::from(poll);
        let ident: String = if is_sub { &f.agent_id } else { &f.session }
            .chars()
            .take(8)
            .collect();
        let short = model.split('-').nth(1).unwrap_or(&model);
        let who = format!("{} {short}", if is_sub { atype.as_str() } else { "main" });
        let rel = self.rel(path);
        // ctx buckets: a jump past several alerts once, at the top one.
        let rule = if is_sub { "ctx_sub" } else { "ctx_main" };
        let buckets = if is_sub {
            th.ctx_sub.clone()
        } else {
            main_buckets(&self.label(f), &th.ctx_main, self.schedule)
        };
        let crossed: Vec<i64> = buckets.into_iter().filter(|b| ctx as i64 >= *b).collect();
        if let Some((top, below)) = crossed.split_last() {
            for b in below {
                let key = format!("{}|{rule}:{}k", f.session, b / 1000);
                self.st.fired.entry(key).or_insert(self.now);
            }
            let fp = format!("{rule}:{}k", top / 1000);
            let detail = format!("ctx {ctx} >= {top} {who} out={out_t} {rel}");
            self.emit(Some(f), rule, &fp, &detail, None);
        }
        // hit ratio over the last 20 turns
        if f.recent.len() >= 20 && ctx as f64 > th.rewrite_min_ctx {
            let tot: u64 = f.recent.iter().map(|r| r[0] + r[1] + r[2]).sum();
            let reads: u64 = f.recent.iter().map(|r| r[0]).sum();
            let ratio = if tot > 0 {
                reads as f64 / tot as f64
            } else {
                1.0
            };
            if ratio < th.hit_low {
                let who_id = if is_sub { ident.as_str() } else { "main" };
                let fp = format!("hit_low:{who_id}:{}", hour_fp(ts));
                let detail = format!(
                    "hit {ratio:.2} < {} over 20 turns ctx={ctx} {who} {rel}",
                    th.hit_low
                );
                self.emit(Some(f), "hit_low", &fp, &detail, None);
            }
        }
        // cache misses
        let g = gap.unwrap_or(0.0);
        if unexpected && cc as f64 >= th.miss_unexpected {
            let mk = format!("{}|{h}", f.session);
            let n = self.st.miss_count.get(&mk).copied().unwrap_or(0) + 1;
            self.st.miss_count.insert(mk, n);
            let cap = th.miss_burst_cap;
            if n as f64 <= cap {
                let id8: String = mid.as_deref().unwrap_or(&ident).chars().take(8).collect();
                let detail = format!(
                    "rewrote {cc} tokens after {g:.0}s gap (ttl {ttl}) ctx={ctx} {who} {rel}"
                );
                let fp = format!("miss_unexpected:{id8}");
                self.emit(Some(f), "miss_unexpected", &fp, &detail, None);
            } else if n as f64 == cap + 1.0 {
                let fp = format!("miss_unexpected:burst:{}", hour_fp(ts));
                let detail = format!("burst: over {cap} unexpected rewrites this hour {who} {rel}");
                self.emit(Some(f), "miss_unexpected", &fp, &detail, None);
            }
        }
        if expected && cc as f64 >= th.miss_idle && fam == "opus" {
            let detail =
                format!("rewrote {cc} tokens after {g:.0}s idle gap ctx={ctx} {who} {rel}");
            let fp = format!("miss_idle:{ident}");
            self.emit(Some(f), "miss_idle", &fp, &detail, None);
        }
        if poll {
            f.polls.push(ts);
            f.polls.retain(|t| *t > ts - 1800.0);
            let n = f.polls.len();
            if n as f64 >= th.poll_agent {
                let detail = format!(
                    "{n} polling turns in 30 min >= {} {who} {rel}",
                    th.poll_agent
                );
                let fp = format!("poll_agent:{ident}");
                self.emit(Some(f), "poll_agent", &fp, &detail, None);
            }
        }
    }

    /// `read_file(path, f)`: the new complete lines since the last read.
    pub fn read_file(&mut self, path: &str, f: &mut FileState) {
        let Ok(meta) = fs::metadata(path) else { return };
        let (ino, size) = (inode(&meta), meta.len());
        f.seen_at = Some(self.now);
        let first = f.ino.is_none();
        self.min_ts = self.now - if first { LOOKBACK } else { 3.0 * LOOKBACK };
        if !first && (f.ino != Some(ino) || size < f.off.unwrap_or(0)) {
            f.off = Some(0);
        }
        let mut off = f.off.unwrap_or(0);
        if first && size > FIRST_TAIL {
            off = size - FIRST_TAIL;
        }
        if off >= size {
            (f.ino, f.size) = (Some(ino), Some(size));
            return;
        }
        f.off0.get_or_insert(off == 0);
        let mut data = Vec::new();
        let read = fs::File::open(path).and_then(|mut fh| {
            fh.seek(SeekFrom::Start(off))?;
            fh.take((size - off).min(READ_CAP)).read_to_end(&mut data)
        });
        if read.is_err() {
            return;
        }
        let Some(end) = data.iter().rposition(|b| *b == b'\n') else {
            (f.ino, f.size) = (Some(ino), Some(size));
            return;
        };
        let mut chunk = &data[..=end];
        if off > 0 && first {
            let cut = chunk.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
            chunk = &chunk[cut..];
        }
        for raw in chunk.split(|b| *b == b'\n') {
            if raw.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            self.lines += 1;
            let Ok(d) = serde_json::from_slice::<Value>(raw) else {
                self.errors += 1;
                continue;
            };
            if !d.is_object() {
                continue;
            }
            let typ = d["type"].as_str();
            if typ == Some("user")
                && f.triage_gate != Some(true)
                && d["entrypoint"].as_str() == Some(TRIAGE_ENTRYPOINT)
                && user_text(&d["message"]["content"]).starts_with(TRIAGE_PREFIX)
            {
                f.triage_gate = Some(true);
            }
            if f.kind == "sub" {
                // #fanout: done once it ends on an end_turn assistant turn.
                match typ {
                    Some("assistant") => {
                        let stop = d["message"]["stop_reason"].as_str();
                        f.done = Some(matches!(stop, Some("end_turn" | "stop_sequence")));
                    }
                    Some("user") => f.done = Some(false),
                    _ => {}
                }
            }
            if typ == Some("assistant") || d["message"]["role"].as_str() == Some("assistant") {
                self.turn(path, f, &d);
            }
        }
        (f.ino, f.size, f.off) = (Some(ino), Some(size), Some(off + end as u64 + 1));
    }
}

/// A user message's text: the string, or the joined `text` blocks.
fn user_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect(),
        _ => String::new(),
    }
}

/// One `tool_use` block, a `Bash` call whose command is a poll.
fn is_poll_turn(msg: &Value) -> bool {
    let blocks: Vec<&Value> = msg["content"]
        .as_array()
        .map(|c| c.iter().filter(|b| b["type"] == "tool_use").collect())
        .unwrap_or_default();
    match blocks.as_slice() {
        [b] if b["name"] == "Bash" => {
            let cmd = &b["input"]["command"];
            let text = match cmd {
                Value::String(s) => s.clone(),
                Value::Null => "None".to_owned(),
                v => v.to_string(),
            };
            is_poll_command(&text)
        }
        _ => false,
    }
}

#[cfg(unix)]
fn inode(m: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.ino()
}

#[cfg(not(unix))]
fn inode(_: &fs::Metadata) -> u64 {
    0
}
