//! The fleet half of `token_watch.Scan`: `fleet_rules` and `degraded`.
//!
//! Why: some token behaviour only shows across the fleet (polling rate, Opus
//! share, burn, session churn, fan-out), and a watcher that cannot parse its
//! input must say so rather than go quiet.
//! What: [`fleet_rules`] applies `poll_rate` (last completed hour over the
//! larger of the floor and 2x the median of up to 168 prior hours, with at
//! least 24), `opus_share` (3 h, at least 10 units), `burn` (the last two
//! completed hours both over 1.5x the trailing 24 h mean, with 8 hours),
//! `session_start_churn` (distinct main starts per project in 3 h) and
//! `fanout` (live subagents per session in the last 5 min). [`degraded`] is
//! `watcher_degraded` over 5% bad lines of at least 20.
//! Test: `opus_share`, `poll_rate_and_burn`, `session_start_churn`,
//! `session_start_churn_skips_triage_gate`, `fanout`,
//! `fanout_counts_live_subagents_only`, `watcher_degraded_on_bad_lines`
//! (token/tests.rs).

use std::collections::BTreeMap;

use super::clock::{compact, hour_fp, hour_of};
use super::project_label;
use super::scan::Scan;
use super::state::FileState;

/// `statistics.median`.
fn median(v: &[u64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    let n = s.len();
    match n {
        0 => 0.0,
        _ if n % 2 == 1 => s[n / 2] as f64,
        _ => (s[n / 2 - 1] + s[n / 2]) as f64 / 2.0,
    }
}

/// `Scan.fleet_rules`.
pub fn fleet_rules(sc: &mut Scan<'_>) {
    let (th, now) = (sc.th.clone(), sc.now);
    let cur = hour_of(now);
    let hours: Vec<String> = sc.st.fleet.keys().cloned().collect();
    let done: Vec<String> = hours.iter().filter(|h| **h < cur).cloned().collect();
    if let Some(last) = done.last() {
        let prior_hours = &done[..done.len() - 1];
        let from = prior_hours.len().saturating_sub(168);
        let prior: Vec<u64> = prior_hours[from..]
            .iter()
            .map(|h| sc.st.fleet[h].polls)
            .collect();
        if prior.len() >= 24 {
            let lim = th.poll_rate_min.max(th.poll_rate_factor * median(&prior));
            let n = sc.st.fleet[last].polls;
            if n as f64 > lim {
                let fp = format!("poll_rate:{}", compact(last));
                let detail = format!("{n} polling turns/h > {lim:.0} (2x 7d median) hour {last}");
                sc.emit(None, "poll_rate", &fp, &detail, Some("fleet"));
            }
        }
    }
    let since = hour_of(now - 2.0 * 3600.0);
    let recent: Vec<(f64, f64)> = hours
        .iter()
        .filter(|h| **h >= since)
        .map(|h| (sc.st.fleet[h].units, sc.st.fleet[h].opus))
        .collect();
    let tot: f64 = recent.iter().map(|r| r.0).sum();
    if tot >= 10.0 {
        let share = recent.iter().map(|r| r.1).sum::<f64>() / tot;
        if share > th.opus_share {
            let fp = format!("opus_share:{}", hour_fp(now));
            let detail = format!(
                "opus share {share:.2} > {} over 3h units={tot:.0}",
                th.opus_share
            );
            sc.emit(None, "opus_share", &fp, &detail, Some("fleet"));
        }
    }
    if done.len() >= 8 {
        let two = &done[done.len() - 2..];
        let rest = &done[..done.len() - 2];
        let trail = &rest[rest.len().saturating_sub(24)..];
        let mean = trail.iter().map(|h| sc.st.fleet[h].units).sum::<f64>() / trail.len() as f64;
        let hot = two
            .iter()
            .all(|h| sc.st.fleet[h].units > th.burn_factor * mean);
        if mean > 0.0 && hot {
            let (a, b) = (sc.st.fleet[&two[1]].units, sc.st.fleet[&two[0]].units);
            let fp = format!("burn:{}", compact(&two[1]));
            let detail = format!(
                "fleet units/h {a:.0} and {b:.0} > {}x 24h mean {mean:.0}",
                th.burn_factor
            );
            sc.emit(None, "burn", &fp, &detail, Some("fleet"));
        }
    }
    let projects: Vec<String> = sc.st.starts.keys().cloned().collect();
    for proj in projects {
        let n = {
            let rows = sc.st.starts.entry(proj.clone()).or_default();
            rows.retain(|r| r.0 >= now - 3.0 * 3600.0);
            let mut ids: Vec<&String> = rows.iter().map(|r| &r.1).collect();
            ids.sort();
            ids.dedup();
            ids.len()
        };
        if n as f64 > th.session_start_churn {
            let label = project_label(&proj);
            let scope = if label.is_empty() {
                "fleet".to_owned()
            } else {
                label
            };
            let fp = format!("session_start_churn:{proj}:{}", hour_fp(now));
            let detail = format!(
                "{n} main-session starts in 3h > {} project {}",
                th.session_start_churn,
                strip_user_prefix(&proj)
            );
            sc.emit(None, "session_start_churn", &fp, &detail, Some(&scope));
        }
    }
    let mut live: BTreeMap<String, Vec<FileState>> = BTreeMap::new();
    for f in sc.st.files.values() {
        let fresh = f.last_ts.is_some_and(|t| t != 0.0 && t >= now - 300.0);
        if f.kind == "sub" && f.done != Some(true) && fresh {
            live.entry(f.session.clone()).or_default().push(f.clone());
        }
    }
    for (sid, fl) in live {
        if fl.len() as f64 > th.fanout_cap {
            let sid8: String = sid.chars().take(8).collect();
            let fp = format!("fanout:{}", fl.len());
            let detail = format!(
                "{} concurrent subagents > cap {} session {sid8}",
                fl.len(),
                th.fanout_cap
            );
            sc.emit(Some(&fl[0]), "fanout", &fp, &detail, None);
        }
    }
}

/// `SESSION_DIR_RE.sub("", project)`: drops a leading `-Users-<name>-`.
pub fn strip_user_prefix(project: &str) -> &str {
    project
        .strip_prefix("-Users-")
        .and_then(|r| {
            let (name, rest) = r.split_once('-')?;
            (!name.is_empty()).then_some(rest)
        })
        .unwrap_or(project)
}

/// `Scan.degraded`.
pub fn degraded(sc: &mut Scan<'_>) {
    if sc.lines >= 20 && sc.errors as f64 / sc.lines as f64 > 0.05 {
        let fp = format!("watcher_degraded:{}", hour_fp(sc.now));
        let detail = format!(
            "{}/{} transcript lines failed to parse",
            sc.errors, sc.lines
        );
        sc.emit(None, "watcher_degraded", &fp, &detail, Some("fleet"));
    }
}
