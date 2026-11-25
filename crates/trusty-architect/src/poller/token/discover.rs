//! Transcript discovery and session names: `discover`, `agent_type`,
//! `session_names` from `token_watch.py`.
//!
//! Why: the watcher reads every main and subagent transcript touched in the
//! last hour; a project directory holds thousands of files, so it lists two
//! levels and never walks deeper.
//! What: [`discover`] yields `(path, kind, project, session, agent_id)` for
//! `*.jsonl` mains modified within [`LOOKBACK`] and `agent-*.jsonl` files in
//! `<session>/subagents/` (the directory itself touched within 6 h), sorted
//! by path. [`agent_type`] reads the sibling `.meta.json`; [`session_names`]
//! maps `sessionId` to its tmux session from the live-session registry.
//! Test: `ctx_sub_uses_subagent_buckets`, `fanout` (token/tests.rs).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde_json::Value;

/// `LOOKBACK`: first-sight lookback and file mtime cutoff, seconds.
pub const LOOKBACK: f64 = 3600.0;

/// One transcript to read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Found {
    /// Its path.
    pub path: String,
    /// `main` or `sub`.
    pub kind: &'static str,
    /// The project directory name.
    pub project: String,
    /// The session id.
    pub session: String,
    /// The subagent id, empty for a main.
    pub agent_id: String,
}

fn mtime(p: &Path) -> Option<f64> {
    let m = fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `discover(base, now)`, sorted by path.
pub fn discover(base: &Path, now: f64) -> Vec<Found> {
    let cutoff = now - LOOKBACK;
    let mut out = Vec::new();
    let Ok(projects) = fs::read_dir(base) else {
        return out;
    };
    for pd in projects.flatten() {
        let pd_path = pd.path();
        if !pd_path.is_dir() {
            continue;
        }
        let project = name_of(&pd_path);
        let Ok(entries) = fs::read_dir(&pd_path) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let name = name_of(&path);
            if path.is_file() {
                if let Some(sid) = name.strip_suffix(".jsonl")
                    && mtime(&path).is_some_and(|m| m >= cutoff)
                {
                    out.push(Found {
                        path: path.to_string_lossy().into_owned(),
                        kind: "main",
                        project: project.clone(),
                        session: sid.to_owned(),
                        agent_id: String::new(),
                    });
                }
            } else if path.is_dir() {
                let sub = path.join("subagents");
                if mtime(&sub).is_none_or(|m| m < now - 6.0 * 3600.0) {
                    continue;
                }
                let Ok(agents) = fs::read_dir(&sub) else {
                    continue;
                };
                for a in agents.flatten() {
                    let ap = a.path();
                    let an = name_of(&ap);
                    let Some(aid) = an
                        .strip_prefix("agent-")
                        .and_then(|r| r.strip_suffix(".jsonl"))
                    else {
                        continue;
                    };
                    if mtime(&ap).is_some_and(|m| m >= cutoff) {
                        out.push(Found {
                            path: ap.to_string_lossy().into_owned(),
                            kind: "sub",
                            project: project.clone(),
                            session: name.clone(),
                            agent_id: aid.to_owned(),
                        });
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// `agent_type(path)`: the sibling `.meta.json`'s `agentType`, else `subagent`.
pub fn agent_type(path: &str) -> String {
    let meta = format!("{}.meta.json", path.strip_suffix(".jsonl").unwrap_or(path));
    let doc: Option<Value> = fs::read_to_string(meta)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    match doc.as_ref().and_then(|d| d.get("agentType")) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(v) if !v.is_null() && v != &Value::Bool(false) => v.to_string(),
        _ => "subagent".to_owned(),
    }
}

/// `session_names(dir)`: `sessionId` to the tmux session before `:`.
pub fn session_names(dir: &Path) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return names;
    };
    for e in entries.flatten() {
        let p = e.path();
        if !name_of(&p).ends_with(".json") {
            continue;
        }
        let Some(d) = fs::read_to_string(&p)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        let tmux = match &d["tmux"] {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            v => v.to_string(),
        };
        if let (Some(sid), Some((sess, _))) = (d["sessionId"].as_str(), tmux.split_once(':'))
            && !sid.is_empty()
        {
            names.insert(sid.to_owned(), sess.to_owned());
        }
    }
    names
}
