//! The plan as text, as JSON and as store ops (#12087).
//!
//! Why: the script prints the plan (`format_plan`, or `json.dumps(plan,
//! indent=2)` with `--json`) and writes it as one batch (`build_ops`); the
//! three must read the same plan.
//! What: [`Plan::to_py`] is the plan dict in the script's key order;
//! [`format_plan`] its text lines; [`build_ops`] one `update` per planned
//! session (the old `details` with `prs`, `issues`, `pr_stats` and `gh_at`
//! replaced, [`merged_details`]) and one `set` of `meta/gh_track`, each with
//! the `if_version` read with the plan.
//! Test: `gh_session_track_recordings_replay_clean_against_the_port`,
//! `merged_details_keeps_other_keys_and_drops_empty_lists`.

use serde_json::{Map, Number, Value};

use super::plan::{Entry, PR_CAP, Plan, PrStats, Session, Summary};
use crate::dashboard::Op;
use crate::pyjson_load::{PyInt, PyValue};
use crate::pyval::{py_str, repr};

fn int(n: i64) -> PyValue {
    PyValue::Int(PyInt::Small(n))
}

fn text(s: &str) -> PyValue {
    PyValue::Str(s.to_owned())
}

fn dict(pairs: Vec<(&str, PyValue)>) -> PyValue {
    PyValue::Dict(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn counts(c: &[(String, i64)]) -> PyValue {
    PyValue::Dict(c.iter().map(|(k, n)| (k.clone(), int(*n))).collect())
}

impl Summary {
    fn to_py(&self) -> PyValue {
        let top = self
            .top
            .iter()
            .map(|t| {
                dict(vec![
                    ("n", t.n.clone()),
                    ("title", text(&t.title)),
                    ("url", t.url.clone()),
                    ("tag", text(t.tag)),
                ])
            })
            .collect();
        dict(vec![
            ("open", int(self.open)),
            ("bugs", int(self.bugs)),
            ("bugs_p1", int(self.bugs_p1)),
            ("p1", int(self.p1)),
            ("p2", int(self.p2)),
            ("security", int(self.security)),
            ("merged_open", int(self.merged_open)),
            ("top", PyValue::List(top)),
        ])
    }

    fn line(&self) -> String {
        format!(
            "{} open, {} bugs (P1 {}), P1 {}, P2 {}, security {}, merged-open {}",
            self.open, self.bugs, self.bugs_p1, self.p1, self.p2, self.security, self.merged_open
        )
    }
}

impl PrStats {
    fn to_py(&self) -> PyValue {
        dict(vec![
            ("open", int(self.open)),
            ("by_state", counts(&self.by_state)),
            ("merged_24h", int(self.merged_24h)),
            ("merged_7d", int(self.merged_7d)),
            ("merged_7d_capped", PyValue::Bool(self.capped)),
        ])
    }
}

impl Entry {
    fn prs_py(&self) -> PyValue {
        PyValue::List(
            self.prs
                .iter()
                .map(|p| {
                    dict(vec![
                        ("n", p.n.clone()),
                        ("state", text(&p.state)),
                        ("url", p.url.clone()),
                    ])
                })
                .collect(),
        )
    }

    fn to_py(&self) -> PyValue {
        dict(vec![
            ("prs", self.prs_py()),
            (
                "issues",
                self.issues.as_ref().map_or(PyValue::Null, Summary::to_py),
            ),
            ("pr_stats", self.pr_stats.to_py()),
        ])
    }
}

impl Plan {
    /// The totals dict, `repo` first when given (`{"repo": repo, **totals}`).
    fn totals_py(&self, repo: Option<&str>) -> PyValue {
        let t = &self.totals;
        let mut pairs = Vec::new();
        if let Some(r) = repo {
            pairs.push(("repo", text(r)));
        }
        pairs.extend([
            ("at", text(&t.at)),
            ("prs_open", int(t.prs_open)),
            ("prs_cap", int(PR_CAP)),
            ("prs_by_state", counts(&t.prs_by_state)),
            ("merged_24h", int(t.merged_24h)),
            ("merged_7d", int(t.merged_7d)),
            ("merged_7d_capped", PyValue::Bool(t.capped)),
            ("issues_open", int(t.issues_open)),
            ("bugs", int(t.all.bugs)),
            ("bugs_p1", int(t.all.bugs_p1)),
            ("p1", int(t.all.p1)),
            ("p2", int(t.all.p2)),
            ("security", int(t.all.security)),
            ("merged_open", int(t.all.merged_open)),
            ("closed_24h", int(t.closed_24h)),
            ("closed_7d", int(t.closed_7d)),
            ("awaiting_release", int(t.awaiting_release)),
        ]);
        dict(pairs)
    }

    /// The plan dict: `{"sessions": {...}, "totals": {...}}`.
    pub(crate) fn to_py(&self) -> PyValue {
        let sessions = self
            .sessions
            .iter()
            .map(|(n, e)| (n.clone(), e.to_py()))
            .collect();
        dict(vec![
            ("sessions", PyValue::Dict(sessions)),
            ("totals", self.totals_py(None)),
        ])
    }
}

/// `format_plan(plan)` plus the newline `print` adds.
pub(crate) fn format_plan(plan: &Plan) -> String {
    let mut out = String::new();
    for (n, e) in &plan.sessions {
        let prs: Vec<String> = e
            .prs
            .iter()
            .map(|p| format!("#{} {}", py_str(&p.n), p.state))
            .collect();
        let pr = if prs.is_empty() {
            "-".to_owned()
        } else {
            prs.join(" ")
        };
        let iss = e
            .issues
            .as_ref()
            .map_or_else(|| "-".to_owned(), Summary::line);
        let ps = &e.pr_stats;
        out += &format!(
            "{n}: PRs {pr} (merged {}/24h {}/7d) | issues {iss}\n",
            ps.merged_24h, ps.merged_7d
        );
    }
    let (t, a) = (&plan.totals, &plan.totals.all);
    out += &format!(
        "totals: {} PRs (cap {PR_CAP}) {}, merged {} (24h) {} (7d); {} issues, \
         {} bugs (P1 {}), P1 {}, P2 {}, security {}, merged-open {}\n",
        t.prs_open,
        repr(&counts(&t.prs_by_state)),
        t.merged_24h,
        t.merged_7d,
        t.issues_open,
        a.bugs,
        a.bugs_p1,
        a.p1,
        a.p2,
        a.security,
        a.merged_open
    );
    out += &format!(
        "issues closed {} (24h) {} (7d) | merged, awaiting release {}\n",
        t.closed_24h, t.closed_7d, t.awaiting_release
    );
    out
}

/// `merged_details(old, entry, now)`: the old details with this pass's keys.
///
/// Why: other writers keep their own keys in `details`; only this script's
/// keys change.
/// What: `prs`, `issues` and `pr_stats` are dropped, then `pr_stats` is
/// set, `prs` and `issues` only when non-empty, and `gh_at` is `now`.
/// Test: `merged_details_keeps_other_keys_and_drops_empty_lists`.
pub(crate) fn merged_details(old: Option<&PyValue>, entry: &Entry, now: &str) -> PyValue {
    let mut d: Vec<(String, PyValue)> = match old {
        Some(PyValue::Dict(pairs)) => pairs.clone(),
        _ => Vec::new(),
    };
    d.retain(|(k, _)| !matches!(k.as_str(), "prs" | "issues" | "pr_stats"));
    let mut put = |k: &str, v: PyValue| match d.iter_mut().find(|(n, _)| n == k) {
        Some(slot) => slot.1 = v,
        None => d.push((k.to_owned(), v)),
    };
    put("pr_stats", entry.pr_stats.to_py());
    if !entry.prs.is_empty() {
        put("prs", entry.prs_py());
    }
    if let Some(s) = &entry.issues {
        put("issues", s.to_py());
    }
    put("gh_at", text(now));
    PyValue::Dict(d)
}

/// A plan value as `serde_json` holds it; `Err` names a value it cannot hold
/// (an integer past 64 bits, a non-finite float).
pub(crate) fn to_json(v: &PyValue) -> Result<Value, String> {
    Ok(match v {
        PyValue::Null => Value::Null,
        PyValue::Bool(b) => Value::Bool(*b),
        PyValue::Int(PyInt::Small(n)) => Value::from(*n),
        PyValue::Int(PyInt::Big(s)) => s
            .parse::<u64>()
            .map(Value::from)
            .map_err(|_| format!("integer {s} is past what the store writes"))?,
        PyValue::Float(f) => Number::from_f64(*f)
            .map(Value::Number)
            .ok_or_else(|| format!("float {f} is not finite; the store refuses it"))?,
        PyValue::Str(s) => Value::String(s.clone()),
        PyValue::List(items) => Value::Array(items.iter().map(to_json).collect::<Result<_, _>>()?),
        PyValue::Dict(pairs) => {
            let mut m = Map::new();
            for (k, item) in pairs {
                m.insert(k.clone(), to_json(item)?);
            }
            Value::Object(m)
        }
    })
}

/// `build_ops(plan, sessions, repo, now, meta_version)` as store ops.
///
/// # Errors
/// A value [`to_json`] refuses.
pub(crate) fn build_ops(
    plan: &Plan,
    sessions: &[Session],
    repo: &str,
    now: &str,
    meta_version: u64,
) -> Result<Vec<Op>, String> {
    let mut ops = Vec::new();
    for (name, entry) in &plan.sessions {
        let Some(doc) = sessions.iter().find(|s| &s.name == name) else {
            continue;
        };
        let old = crate::pyval::get(&doc.data, "details");
        let data = dict(vec![("details", merged_details(old, entry, now))]);
        ops.push(Op::Update {
            collection: "sessions".into(),
            id: doc.id.clone(),
            data: to_json(&data)?,
            if_version: Some(doc.version),
        });
    }
    ops.push(Op::Set {
        collection: "meta".into(),
        id: "gh_track".into(),
        data: to_json(&plan.totals_py(Some(repo)))?,
        if_version: Some(meta_version),
    });
    Ok(ops)
}
