//! PR claims: `gh pr view`, `gh pr checks` and their REST fallback (#12028 P1g).
//!
//! Why: a GraphQL rate limit must not turn a checkable claim into a guess,
//! so the same fields are read over REST; a failed REST read leaves the
//! claim `UNCHECKED`.
//! What: `check_pr`, `_compare_pr`, `_check_buckets`, `_check_runs_rest`,
//! `_rest_get` and `_fetch_rest_pr` of the script, line for line.
//! Test: the `pr-*` and `rate-limit-*` recordings in
//! `tests/replay_verify_claim.rs`.

use super::Verifier;
use super::pyops::{PyExc, count_or_len, dget, gt_int, has_key, or_get, or_value, text};
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::contains_ci;
use crate::pyval::{py_str, truthy};

const VIA_REST: &str = " (via rest)";

fn s(v: &str) -> PyValue {
    PyValue::Str(v.to_owned())
}

/// `x or ""` as a value.
fn or_empty(x: Option<&PyValue>) -> PyValue {
    or_value(x).cloned().unwrap_or_else(|| s(""))
}

impl Verifier<'_> {
    fn pr_text(&self) -> String {
        self.ev
            .pr
            .as_ref()
            .map(super::pyops::int_str)
            .unwrap_or_default()
    }

    fn rate_limited(&self) -> bool {
        let chars: Vec<char> = self.last_err.chars().collect();
        contains_ci(&chars, "rate limit")
    }

    /// `_rest_get(path)`: `GET repos/<slug>/<path>`; `None` data on failure.
    pub(super) fn rest_get(&mut self, path: &str) -> Result<(Option<PyValue>, String), PyExc> {
        let argv = [
            "gh".into(),
            "api".into(),
            format!("repos/{}/{path}", self.slug),
        ];
        let (data, why) = self.gh_json(&argv)?;
        if let Some(d) = &data
            && self.last_rc != 0
        {
            let msg = match d {
                PyValue::Dict(_) => or_value(dget(d, "message")?).map(py_str),
                _ => None,
            };
            let text = msg.unwrap_or_else(|| self.last_err.clone());
            return Ok((None, Self::err_line(&text)));
        }
        Ok((data, why))
    }

    /// `_fetch_rest_pr()`: the PR over REST, shaped like `gh pr view --json`; cached.
    fn fetch_rest_pr(&mut self) -> Result<Option<PyValue>, PyExc> {
        if self.rest_pr.is_none() && self.rest_pr_why.is_empty() {
            let (d, why) = self.rest_get(&format!("pulls/{}", self.pr_text()))?;
            let d = match d {
                Some(d @ PyValue::Dict(_)) if has_key(&d, "head") => d,
                _ => {
                    self.rest_pr_why = if why.is_empty() {
                        "unexpected REST reply".into()
                    } else {
                        why
                    };
                    return Ok(None);
                }
            };
            let merged = dget(&d, "merged")?.is_some_and(truthy);
            let merge_sha = dget(&d, "merge_commit_sha")?.filter(|v| truthy(v));
            let merge = match (merged, merge_sha) {
                (true, Some(m)) => PyValue::Dict(vec![("oid".into(), m.clone())]),
                _ => PyValue::Null,
            };
            let state = if merged {
                "MERGED"
            } else if matches!(dget(&d, "state")?, Some(PyValue::Str(x)) if x == "closed") {
                "CLOSED"
            } else {
                "OPEN"
            };
            let opt = |v: Option<&PyValue>| v.cloned().unwrap_or(PyValue::Null);
            self.rest_pr = Some(PyValue::Dict(vec![
                ("headRefOid".into(), opt(or_get(dget(&d, "head")?, "sha")?)),
                ("baseRefOid".into(), opt(or_get(dget(&d, "base")?, "sha")?)),
                ("mergeCommit".into(), merge),
                ("state".into(), s(state)),
                ("mergedAt".into(), opt(dget(&d, "merged_at")?)),
                ("changedFiles".into(), opt(dget(&d, "changed_files")?)),
                ("commits".into(), opt(dget(&d, "commits")?)),
            ]));
        }
        Ok(self.rest_pr.clone())
    }

    /// `check_pr()`.
    pub(super) fn check_pr(&mut self) -> Result<(), PyExc> {
        let (ev, pr) = (self.ev, self.pr_text());
        let state = match self.claim_true("pr_state") {
            Some(v) => Some(v.clone()),
            None => (ev.kind == "merged").then(|| s("MERGED")),
        };
        let filled = |o: &Option<String>| o.as_deref().is_some_and(|x| !x.is_empty());
        let mut want: Vec<(&str, &str)> = Vec::new();
        if filled(&ev.head) {
            want.push(("headRefOid", "head"));
        }
        if filled(&ev.base) {
            want.push(("baseRefOid", "base"));
        }
        if filled(&ev.merge_sha) {
            want.push(("mergeCommit", "merge_sha"));
        }
        if state.is_some() {
            want.push(("state", "pr_state"));
        }
        if self.claim_true("merged_at").is_some() {
            want.push(("mergedAt", "merged_at"));
        }
        if self.claim("files").is_some() {
            want.push(("changedFiles", "files"));
        }
        if self.claim("commits").is_some() {
            want.push(("commits", "commits"));
        }
        if !want.is_empty() {
            let fields: Vec<&str> = want.iter().map(|(f, _)| *f).collect();
            let argv: Vec<String> = ["gh", "pr", "view", &pr, "--repo", &self.slug.clone()]
                .iter()
                .map(|x| (*x).to_owned())
                .chain(["--json".to_owned(), fields.join(",")])
                .collect();
            let (mut data, mut why) = self.gh_json(&argv)?;
            if data.is_none() && self.rate_limited() {
                match self.fetch_rest_pr()? {
                    None => why = format!("{why}; REST fallback failed: {}", self.rest_pr_why),
                    Some(rest) => {
                        data = Some(rest);
                        self.via = VIA_REST;
                    }
                }
            }
            match data {
                None => {
                    for (_, label) in &want {
                        self.unchecked(
                            &format!("pr {pr} {label}"),
                            &format!("gh pr view failed: {why}"),
                        );
                    }
                }
                Some(d) => {
                    self.compare_pr(&d, state.as_ref())?;
                    self.via = "";
                }
            }
        }
        if let Some(PyValue::Dict(chk)) = self.claim("checks") {
            self.check_buckets(chk)?;
        }
        Ok(())
    }

    /// `_compare_pr(data, state)`.
    fn compare_pr(&mut self, data: &PyValue, state: Option<&PyValue>) -> Result<(), PyExc> {
        let (ev, pr) = (self.ev, self.pr_text());
        for (field, key, claimed) in [
            ("head", "headRefOid", &ev.head),
            ("base", "baseRefOid", &ev.base),
        ] {
            if let Some(c) = claimed.as_deref().filter(|x| !x.is_empty()) {
                let actual = or_empty(dget(data, key)?);
                self.compare_sha(&format!("pr {pr} {field}"), c, &actual)?;
            }
        }
        if let Some(m) = ev.merge_sha.as_deref().filter(|x| !x.is_empty()) {
            let merge = or_empty(or_get(dget(data, "mergeCommit")?, "oid")?);
            if truthy(&merge) {
                self.compare_sha(&format!("pr {pr} merge_sha"), m, &merge)?;
            } else {
                self.mismatch(&format!("pr {pr} merge_sha {m}"), m, "no merge commit");
            }
        }
        if let Some(st) = state {
            let upper = s(&text(st, "upper")?.to_uppercase());
            let got = dget(data, "state")?.cloned().unwrap_or(PyValue::Null);
            self.compare_eq(&format!("pr {pr} state"), &upper, &got);
        }
        if let Some(m) = self.claim_true("merged_at") {
            let claimed = text(m, "replace")?;
            let want = claimed.replace("+00:00", "Z");
            let got = or_value(dget(data, "mergedAt")?)
                .cloned()
                .unwrap_or_else(|| s("null"));
            let ok = want.chars().count() >= 10 && text(&got, "startswith")?.starts_with(&want);
            let label = format!("pr {pr} merged_at {claimed}");
            if ok {
                self.matched(&label);
            } else {
                self.mismatch(&label, claimed, &py_str(&got));
            }
        }
        if let Some(files) = self.claim("files") {
            let got = dget(data, "changedFiles")?
                .cloned()
                .unwrap_or(PyValue::Null);
            self.compare_eq(&format!("pr {pr} files"), files, &got);
        }
        if let Some(commits) = self.claim("commits") {
            let n = count_or_len(dget(data, "commits")?)?;
            self.compare_eq(&format!("pr {pr} commits"), commits, &n);
        }
        Ok(())
    }

    /// `_check_buckets(want)`.
    fn check_buckets(&mut self, want: &[(String, PyValue)]) -> Result<(), PyExc> {
        let pr = self.pr_text();
        let argv: Vec<String> = [
            "gh",
            "pr",
            "checks",
            &pr,
            "--repo",
            &self.slug.clone(),
            "--json",
            "bucket",
        ]
        .iter()
        .map(|x| (*x).to_owned())
        .collect();
        let (mut data, mut why) = self.gh_json(&argv)?;
        if !matches!(data, Some(PyValue::List(_))) && self.rate_limited() {
            (data, why) = self.check_runs_rest(&why)?;
            self.via = if data.is_some() { VIA_REST } else { "" };
        }
        let Some(PyValue::List(rows)) = data else {
            for (k, _) in want {
                self.unchecked(
                    &format!("pr {pr} checks {k}"),
                    &format!("gh pr checks failed: {why}"),
                );
            }
            return Ok(());
        };
        let mut got = [("pass", 0i64), ("fail", 0), ("pending", 0)];
        for row in &rows {
            match dget(row, "bucket")? {
                Some(PyValue::Str(b)) => {
                    if let Some(slot) = got.iter_mut().find(|(k, _)| k == b) {
                        slot.1 += 1;
                    }
                }
                Some(v @ (PyValue::List(_) | PyValue::Dict(_))) => {
                    return Err(PyExc(format!(
                        "TypeError: unhashable type: '{}'",
                        crate::pyval::type_name(v)
                    )));
                }
                _ => {}
            }
        }
        for (k, n) in want {
            let count = got.iter().find(|(g, _)| g == k).map_or(0, |(_, c)| *c);
            self.compare_eq(
                &format!("pr {pr} checks {k}"),
                n,
                &PyValue::Int(PyInt::Small(count)),
            );
        }
        self.via = "";
        Ok(())
    }

    /// `_check_runs_rest(why)`: the head's check runs as `gh pr checks` rows.
    fn check_runs_rest(&mut self, why: &str) -> Result<(Option<PyValue>, String), PyExc> {
        let Some(pr) = self.fetch_rest_pr()? else {
            return Ok((
                None,
                format!("{why}; REST fallback failed: {}", self.rest_pr_why),
            ));
        };
        let head = py_str(dget(&pr, "headRefOid")?.unwrap_or(&PyValue::Null));
        let (d, rwhy) = self.rest_get(&format!("commits/{head}/check-runs?per_page=100"))?;
        let runs = match &d {
            Some(d @ PyValue::Dict(_)) => match dget(d, "check_runs")? {
                Some(PyValue::List(r)) => Some(r.clone()),
                _ => None,
            },
            _ => None,
        };
        let (Some(d), Some(runs)) = (d, runs) else {
            let r = if rwhy.is_empty() {
                "unexpected REST reply".to_owned()
            } else {
                rwhy
            };
            return Ok((None, format!("{why}; REST fallback failed: {r}")));
        };
        // `d.get("total_count", 0)`: a missing key is 0, a JSON null is None.
        let total = match &d {
            PyValue::Dict(pairs) => pairs
                .iter()
                .find(|(k, _)| k == "total_count")
                .map_or(PyValue::Int(PyInt::Small(0)), |(_, v)| v.clone()),
            _ => PyValue::Int(PyInt::Small(0)),
        };
        if gt_int(&total, runs.len())? {
            return Ok((
                None,
                format!(
                    "{why}; REST fallback failed: more than {} check runs",
                    runs.len()
                ),
            ));
        }
        let mut rows = Vec::new();
        for run in &runs {
            let status = dget(run, "status")?;
            let conclusion = dget(run, "conclusion")?;
            let is = |v: Option<&PyValue>, set: &[&str]| matches!(v, Some(PyValue::Str(x)) if set.contains(&x.as_str()));
            let bucket = if !is(status, &["completed"]) {
                "pending"
            } else if is(conclusion, &["success"]) {
                "pass"
            } else if is(
                conclusion,
                &["failure", "timed_out", "startup_failure", "action_required"],
            ) {
                "fail"
            } else {
                "skipping"
            };
            rows.push(PyValue::Dict(vec![("bucket".into(), s(bucket))]));
        }
        Ok((Some(PyValue::List(rows)), String::new()))
    }
}
