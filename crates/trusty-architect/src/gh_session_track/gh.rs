//! `gh_json` and the four read-only gh queries of `gh-session-track.py` (#12087).
//!
//! Why: every gh call runs with the token variables removed and the owner's
//! gh account directory, and fails as one `GhError` line.
//! What: [`gh_json`] runs `gh ARGS` with `env` as the whole environment and
//! a 120 s bound, and decodes stdout with [`loads`]. [`fetch`] makes the
//! script's four reads: open PRs, open issues, PRs merged since seven days
//! before `now` (`merged:>=YYYY-MM-DD`), and the issues closed since then
//! over REST (`--paginate --slurp`; pull-request rows dropped).
//! Known gap: output that is not UTF-8 is decoded lossily, where the script
//! raises `UnicodeDecodeError`.
//! Test: `gh_session_track_recordings_replay_clean_against_the_port` (every
//! call's argv), `shipped_gh_session_track_strips_tokens_from_gh`.

use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::time::Duration;

use super::plan::{Fetched, MERGED_DAYS, MERGED_LIMIT};
use crate::classify::alerts::iso;
use crate::process::{RunFailure, run_bounded};
use crate::pyjson_load::{PyValue, loads};
use crate::pystr::{py_repr, strip};
use crate::pyval::{get, type_name};
use crate::writers::{errno_text, universal_newlines};

/// `subprocess.run(..., timeout=120)`.
const TIMEOUT: Duration = Duration::from_secs(120);
const PR_FIELDS: &str = "number,title,url,isDraft,labels,mergeStateStatus,headRefName,baseRefName,statusCheckRollup,updatedAt";
const ISSUE_FIELDS: &str = "number,title,url,labels";
const MERGED_FIELDS: &str = "number,labels,mergedAt,title,headRefName,closingIssuesReferences";

fn decode(bytes: &[u8]) -> String {
    universal_newlines(String::from_utf8_lossy(bytes).into_owned())
}

/// `gh_json(args)`: the decoded reply, or the `GhError` text.
///
/// Why: see the module doc.
/// What: `env` replaces the child's environment; the caller has removed
/// `GH_TOKEN` and `GITHUB_TOKEN` and set `GH_CONFIG_DIR`.
/// Test: `shipped_gh_session_track_strips_tokens_from_gh`.
pub(crate) fn gh_json(args: &[String], env: &[(String, String)]) -> Result<PyValue, String> {
    let head = format!(
        "gh {} {}",
        args.first().map_or("", String::as_str),
        args.get(1).map_or("", String::as_str)
    );
    let mut cmd = Command::new("gh");
    cmd.args(args).env_clear();
    cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let done = match run_bounded(cmd, TIMEOUT) {
        Ok(done) => done,
        Err(RunFailure::Spawn(e) | RunFailure::Wait(e)) => {
            return Err(format!("{head} failed: {}", errno_text(&e, "gh")));
        }
        Err(RunFailure::Timeout) => {
            let shown: Vec<String> = std::iter::once("gh")
                .chain(args.iter().map(String::as_str))
                .map(py_repr)
                .collect();
            return Err(format!(
                "{head} failed: Command '[{}]' timed out after 120 seconds",
                shown.join(", ")
            ));
        }
    };
    let rc = done
        .status
        .code()
        .or_else(|| done.status.signal().map(|s| -s))
        .unwrap_or(-1);
    if rc != 0 {
        let err: String = strip(&decode(&done.stderr)).chars().take(200).collect();
        return Err(format!("{head} failed (exit {rc}): {err}"));
    }
    loads(&decode(&done.stdout))
        .map_err(|e| format!("{head} returned invalid JSON: {}", e.py_str()))
}

/// The list a reply must be; anything else is the script's traceback, worded here.
fn list(v: PyValue, what: &str) -> Result<Vec<PyValue>, String> {
    match v {
        PyValue::List(items) => Ok(items),
        PyValue::Null if what == "merged" => Ok(Vec::new()),
        other => Err(format!(
            "TypeError: the gh {what} reply is a {}, not a list",
            type_name(&other)
        )),
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

/// The script's `fetch`, `fetch_merged` and `fetch_closed`, in its order.
///
/// # Errors
/// `Ok(Err(line))` for a gh failure (`GhError`); `Err(line)` for a reply of a
/// shape the script cannot use (its traceback's last line, worded here).
pub(crate) fn fetch(
    repo: &str,
    now_secs: i64,
    env: &[(String, String)],
) -> Result<Result<Fetched, String>, String> {
    let since = now_secs - MERGED_DAYS * 86_400;
    let stamp = iso(since as f64);
    let day = stamp.get(..10).unwrap_or(&stamp).to_owned();
    let limit = MERGED_LIMIT.to_string();
    let calls = [
        argv(&[
            "pr", "list", "-R", repo, "--state", "open", "-L", "200", "--json", PR_FIELDS,
        ]),
        argv(&[
            "issue",
            "list",
            "-R",
            repo,
            "--state",
            "open",
            "-L",
            "1000",
            "--json",
            ISSUE_FIELDS,
        ]),
        argv(&[
            "pr",
            "list",
            "-R",
            repo,
            "--state",
            "merged",
            "--search",
            &format!("merged:>={day}"),
            "-L",
            &limit,
            "--json",
            MERGED_FIELDS,
        ]),
        argv(&[
            "api",
            &format!("repos/{repo}/issues?state=closed&since={stamp}&per_page=100"),
            "--paginate",
            "--slurp",
        ]),
    ];
    let mut replies = Vec::new();
    for call in &calls {
        match gh_json(call, env) {
            Ok(v) => replies.push(v),
            Err(e) => return Ok(Err(e)),
        }
    }
    let mut it = replies.into_iter();
    let mut next = || it.next().unwrap_or(PyValue::Null);
    let (prs, issues, merged, pages) = (next(), next(), next(), next());
    let mut closed = Vec::new();
    for page in list(pages, "api")? {
        for i in list(page, "api page")? {
            if !matches!(&i, PyValue::Dict(p) if p.iter().any(|(k, _)| k == "pull_request")) {
                closed.push(get(&i, "closed_at").cloned().unwrap_or(PyValue::Null));
            }
        }
    }
    Ok(Ok(Fetched {
        prs: list(prs, "pr list")?,
        issues: list(issues, "issue list")?,
        merged: list(merged, "merged")?,
        closed,
    }))
}
