//! Issue, local-checkout, crates.io and CI claims (#12028 P1g).
//!
//! Why: these claims need no PR: an issue's state over REST, a branch's
//! commits in the local checkout (no fetch is ever run), the crates.io max
//! version and one Actions run.
//! What: `check_issues`, `check_local`, `check_crate` and `check_ci` of the
//! script, line for line.
//! Test: the `issues-*`, `local-*`, `published-*` and `ci-*` recordings in
//! `tests/replay_verify_claim.rs`.

use std::path::Path;

use super::input::CRATES_API;
use super::pyops::{PyExc, dget, int_from_digits, int_str, or_value, text};
use super::{Verifier, sha_state};
use crate::pyjson_load::{PyValue, loads};
use crate::pystr::{is_digit, is_space, splitlines, strip};
use crate::pyval::py_str;

/// `USER_AGENT`.
const USER_AGENT: &str = "trusty-architect";

fn s(v: &str) -> PyValue {
    PyValue::Str(v.to_owned())
}

/// `ev[field]` when truthy.
fn filled(o: &Option<String>) -> Option<&str> {
    o.as_deref().filter(|x| !x.is_empty())
}

/// `os.path.basename(p)`.
fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// `re.sub(r"[\s-]+", "_", text)`.
fn underscore_runs(text: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for c in text.chars() {
        if is_space(c) || c == '-' {
            if !in_run {
                out.push('_');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

impl Verifier<'_> {
    /// `check_issues()`.
    pub(super) fn check_issues(&mut self) -> Result<(), PyExc> {
        let ev = self.ev;
        for it in &ev.issues {
            let n = int_str(&it.n);
            let label = format!("issue {n}");
            if self.gh_env.is_none() {
                let why = self.no_account_reason();
                self.unchecked(&label, &why);
                continue;
            }
            let (d, why) = self.rest_get(&format!("issues/{n}"))?;
            let Some(d @ PyValue::Dict(_)) = d else {
                self.unchecked(&label, &format!("gh api failed: {why}"));
                continue;
            };
            if super::pyops::has_key(&d, "pull_request") {
                self.unchecked(&label, "number is a pull request, not an issue");
                continue;
            }
            let got = py_str(dget(&d, "state")?.unwrap_or(&PyValue::Null)).to_uppercase();
            self.compare_eq(
                &format!("{label} state"),
                &s(&it.state.to_uppercase()),
                &s(&got),
            );
            if !it.reason.is_empty() {
                let want = underscore_runs(&strip(&it.reason).to_lowercase());
                let got = or_value(dget(&d, "state_reason")?)
                    .cloned()
                    .unwrap_or_else(|| s("null"));
                let line = format!("{label} state_reason {}", it.reason);
                if matches!(&got, PyValue::Str(g) if *g == want) {
                    self.matched(&line);
                } else {
                    self.mismatch(&line, &it.reason, &py_str(&got));
                }
            }
        }
        Ok(())
    }

    fn git(&self, args: &[&str]) -> Result<(i32, String, String), PyExc> {
        let path = self.path.clone().unwrap_or_default();
        let argv: Vec<String> = ["git", "-C", &path]
            .iter()
            .chain(args)
            .map(|x| (*x).to_owned())
            .collect();
        self.run(&argv, None)
    }

    /// `_commit(ref)`: the full oid, `""` when absent, `None` when git failed.
    fn commit(&self, r: &str) -> Result<Option<String>, PyExc> {
        let spec = format!("{r}^{{commit}}");
        let (rc, out, _) = self.git(&["rev-parse", "--verify", "--quiet", &spec])?;
        Ok(if rc == 0 && !strip(&out).is_empty() {
            Some(strip(&out).to_owned())
        } else if rc == 1 {
            Some(String::new())
        } else {
            None
        })
    }

    /// `check_local()`.
    pub(super) fn check_local(&mut self) -> Result<(), PyExc> {
        let ev = self.ev;
        let need = filled(&ev.head).is_some()
            || filled(&ev.base).is_some()
            || filled(&ev.merge_sha).is_some()
            || self.claim("commits").is_some()
            || self.claim("files").is_some();
        if !need {
            return Ok(());
        }
        let path = self.path.clone().unwrap_or_default();
        if path.is_empty() || !Path::new(&path).is_dir() {
            let repo = ev
                .repo
                .as_deref()
                .map_or("None".into(), crate::pystr::py_repr);
            self.unchecked(
                "local checkout",
                &format!("no local checkout for repo {repo}"),
            );
            return Ok(());
        }
        let name = basename(&path).to_owned();
        let (mut head, mut base) = (None, None);
        for (field, r) in [("head", filled(&ev.head)), ("base", filled(&ev.base))] {
            let Some(r) = r else { continue };
            if matches!(sha_state(r, ""), "short" | "notsha") {
                self.compare_sha(field, r, &s(""))?;
                continue;
            }
            let full = self.commit(r)?;
            match full.as_deref() {
                None => self.unchecked(&format!("{field} {r}"), "git failed in the local checkout"),
                Some("") => self.mismatch(
                    &format!("{field} {r}"),
                    "commit present in local checkout",
                    "absent (no fetch was run)",
                ),
                Some(_) => self.matched(&format!("{field} {r} exists in {name}")),
            }
            let full = full.filter(|f| !f.is_empty());
            if field == "head" {
                head = full;
            } else {
                base = full;
            }
        }
        if let Some(m) = filled(&ev.merge_sha) {
            if sha_state(m, "") != "differs" {
                self.compare_sha("merge_sha", m, &s(""))?;
            } else if self.commit(m)?.is_some_and(|f| !f.is_empty()) {
                self.matched(&format!("merge_sha {m} exists in {name}"));
            } else {
                self.unchecked(
                    &format!("merge_sha {m}"),
                    "not in local checkout (it may be behind origin; no fetch was run)",
                );
            }
        }
        if let (Some(h), Some(b)) = (&head, &base) {
            let (rc, _, err) = self.git(&["merge-base", "--is-ancestor", b, h])?;
            let (eh, eb) = (
                filled(&ev.head).unwrap_or(""),
                filled(&ev.base).unwrap_or(""),
            );
            let label = format!("head {eh} descends from base {eb}");
            match rc {
                0 => self.matched(&label),
                1 => self.mismatch(
                    &label,
                    "head descends from base",
                    "head is not a descendant",
                ),
                _ => self.unchecked("descendant check", &Self::err_line(&err)),
            }
        }
        for key in ["commits", "files"] {
            let Some(want) = self.claim(key) else {
                continue;
            };
            let (Some(h), Some(b)) = (&head, &base) else {
                self.unchecked(
                    &format!("{key} {}", py_str(want)),
                    "needs a head and a base present in the checkout",
                );
                continue;
            };
            if key == "commits" {
                let (rc, out, err) = self.git(&["rev-list", "--count", &format!("{b}..{h}")])?;
                self.count("commits over base", want, rc, strip(&out), &err);
            } else {
                let (rc, out, err) = self.git(&["diff", "--numstat", b, h])?;
                let n = splitlines(&out)
                    .iter()
                    .filter(|l| !strip(l).is_empty())
                    .count();
                self.count("files changed over base", want, rc, &n.to_string(), &err);
            }
        }
        Ok(())
    }

    /// `_count(label, want, rc, got, err)`.
    fn count(&mut self, label: &str, want: &PyValue, rc: i32, got: &str, err: &str) {
        if rc != 0 || got.is_empty() || !got.chars().all(is_digit) {
            self.unchecked(&format!("{label} {}", py_str(want)), &Self::err_line(err));
        } else {
            self.compare_eq(label, want, &PyValue::Int(int_from_digits(got)));
        }
    }

    /// `check_crate()`.
    pub(super) fn check_crate(&mut self) -> Result<(), PyExc> {
        let ev = self.ev;
        let expected = match self.claim_true("crates_io_max") {
            Some(v) => Some(v.clone()),
            None if ev.kind == "published" => ev.version.clone().map(PyValue::Str),
            None => None,
        };
        let krate = ev.crate_name.as_deref().unwrap_or("");
        let Some(expected) = expected.filter(crate::pyval::truthy) else {
            return Ok(());
        };
        if krate.is_empty() {
            return Ok(());
        }
        if !krate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            self.unchecked(&format!("crate {krate}"), "not a valid crate name");
            return Ok(());
        }
        let argv: Vec<String> = ["curl", "-fsS", "-m", "20", "-A", USER_AGENT]
            .iter()
            .map(|x| (*x).to_owned())
            .chain([format!("{CRATES_API}/{krate}")])
            .collect();
        let (_, out, err) = self.run(&argv, None)?;
        let got = loads(&out).ok().and_then(|v| {
            let PyValue::Dict(top) = v else { return None };
            let (_, c) = top.into_iter().find(|(k, _)| k == "crate")?;
            let PyValue::Dict(c) = c else { return None };
            c.into_iter()
                .find(|(k, _)| k == "max_version")
                .map(|(_, v)| v)
        });
        let label = format!("crates_io_max {krate}");
        match got {
            Some(got) => self.compare_eq(&label, &expected, &got),
            None => {
                let e = Self::err_line(if err.is_empty() { &out } else { &err });
                self.unchecked(&label, &format!("crates.io lookup failed: {e}"));
            }
        }
        Ok(())
    }

    /// `check_ci()`.
    pub(super) fn check_ci(&mut self) -> Result<(), PyExc> {
        let Some(run) = self.ev.ci_run.as_ref() else {
            return Ok(());
        };
        let run = int_str(run);
        let label = format!("ci_run {run}");
        let (conclusion, head) = (self.claim_true("ci_conclusion"), self.claim_true("ci_head"));
        if conclusion.is_none() && head.is_none() {
            self.unchecked(&label, "no ci_conclusion or ci_head claim to compare");
            return Ok(());
        }
        let argv: Vec<String> = [
            "gh",
            "run",
            "view",
            &run,
            "--repo",
            &self.slug.clone(),
            "--json",
            "conclusion,headSha,status",
        ]
        .iter()
        .map(|x| (*x).to_owned())
        .collect();
        let (data, why) = self.gh_json(&argv)?;
        let Some(data) = data else {
            self.unchecked(&label, &format!("gh run view failed: {why}"));
            return Ok(());
        };
        if let Some(c) = conclusion {
            let got = match or_value(dget(&data, "conclusion")?) {
                Some(v) => v.clone(),
                None => or_value(dget(&data, "status")?)
                    .cloned()
                    .unwrap_or_else(|| s("unknown")),
            };
            self.compare_eq(&format!("{label} conclusion"), c, &got);
        }
        if let Some(h) = head {
            // `data.get("headSha")` is evaluated before `claimed.strip()`.
            let actual = or_value(dget(&data, "headSha")?)
                .cloned()
                .unwrap_or_else(|| s(""));
            let claimed = text(h, "strip")?;
            self.compare_sha(&format!("{label} head"), claimed, &actual)?;
        }
        Ok(())
    }
}
