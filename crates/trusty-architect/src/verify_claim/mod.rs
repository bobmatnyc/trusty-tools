//! The port of `verify-claim.py`: check the claims in one PM-message event (#12028 P1g).
//!
//! Why: each PM message ("head X ready for clearance", "#N merged as SHA",
//! "published") costs the Architect a verify round of gh, git and crates.io
//! calls; this runs the round with no model and prints one line per claim.
//! What: [`Verifier::verify`] checks every claim it can and returns one
//! `MATCH`, `MISMATCH` or `UNCHECKED` line per claim, in the script's order:
//! PR fields over `gh pr view` (REST `pulls/<n>` on a GraphQL rate limit),
//! check buckets over `gh pr checks` (REST check-runs on a rate limit), the
//! CI run, issue states over REST, the local checkout's commits when no PR is
//! named, and the crates.io max version. Every command passes
//! [`input::guard_read_only`]; `VERIFY_CLAIM_OFFLINE=1` answers every gh and
//! curl call 127. A gh call runs with the caller's environment minus
//! `GH_TOKEN` and `GITHUB_TOKEN`, plus `GH_CONFIG_DIR` from the owner's
//! `repos` entry in `local/architect.json`; git and curl inherit it.
//! [`cli::run`] is the command line.
//! Test: `tests/replay_verify_claim.rs`, `tests/verify_claim_smoke.rs`.

pub mod cli;
pub mod input;
mod local;
mod pr;
mod pyops;
pub mod runner;

use crate::architect_config::{Config, ConfigError, expand_home};
use crate::pyjson_load::{PyValue, loads};
use crate::pystr::{py_repr, splitlines, strip};
use crate::pyval::{py_str, truthy};
pub use input::{BadInput, Event, LoadFailure, load_event};
use input::{guard_read_only, tool_is_network};
use pyops::{PyExc, dget, py_eq};

/// `MIN_SHA`.
const MIN_SHA: usize = 10;

/// `(repos, owners)` of `repo_maps`: bare name (lower case) to (slug,
/// checkout), and owner (lower case) to its gh config dir (`None` keeps the
/// inherited default). Both keep first-insertion order.
pub(crate) type RepoMaps = (
    Vec<(String, (String, String))>,
    Vec<(String, Option<String>)>,
);

/// `repo_maps(config)`.
///
/// # Errors
/// [`ConfigError`] when two repos of one owner name different gh config dirs.
pub(crate) fn repo_maps(config: &Config, home: Option<&str>) -> Result<RepoMaps, ConfigError> {
    let (mut repos, mut owners): RepoMaps = (Vec::new(), Vec::new());
    for (name, entry) in &config.repos {
        let checkout = if entry.checkout.is_empty() {
            String::new()
        } else {
            expand_home(&entry.checkout, home)
        };
        let key = name.to_lowercase();
        let value = (entry.slug.clone(), checkout);
        match repos.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => repos.push((key, value)),
        }
        let owner = entry.slug.split('/').next().unwrap_or("").to_lowercase();
        let gh_dir =
            (!entry.gh_config_dir.is_empty()).then(|| expand_home(&entry.gh_config_dir, home));
        let known = match owners.iter().find(|(o, _)| *o == owner) {
            Some((_, d)) => d.clone(),
            None => {
                owners.push((owner, gh_dir.clone()));
                gh_dir.clone()
            }
        };
        if known != gh_dir {
            return Err(ConfigError(format!(
                "{}: repos.{name}.gh_config_dir: differs from another repo of the same owner",
                config.path
            )));
        }
    }
    Ok((repos, owners))
}

/// `sha_state(claimed, actual)`: `ok`, `differs`, `short` or `notsha`.
pub(crate) fn sha_state(claimed: &str, actual: &str) -> &'static str {
    let c = strip(claimed).to_lowercase();
    if c.is_empty() || !c.chars().all(|ch| matches!(ch, '0'..='9' | 'a'..='f')) {
        return "notsha";
    }
    if c.chars().count() < MIN_SHA {
        return "short";
    }
    if actual.to_lowercase().starts_with(&c) {
        "ok"
    } else {
        "differs"
    }
}

/// A result line's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `MATCH`.
    Match,
    /// `MISMATCH`.
    Mismatch,
    /// `UNCHECKED`.
    Unchecked,
}

/// `Verifier`: runs every claim of one event.
pub(crate) struct Verifier<'a> {
    ev: &'a Event,
    repos: RepoMaps,
    environ: Vec<(String, String)>,
    offline: bool,
    results: Vec<(Status, String)>,
    slug: String,
    path: Option<String>,
    gh_env: Option<Vec<(String, String)>>,
    via: &'static str,
    last_err: String,
    last_rc: i32,
    rest_pr: Option<PyValue>,
    rest_pr_why: String,
}

impl<'a> Verifier<'a> {
    /// A verifier over `ev` with the maps of [`repo_maps`] and the caller's
    /// environment (`os.environ`).
    pub(crate) fn new(ev: &'a Event, repos: RepoMaps, environ: Vec<(String, String)>) -> Self {
        let offline = environ
            .iter()
            .any(|(k, v)| k == "VERIFY_CLAIM_OFFLINE" && v == "1");
        Self {
            ev,
            repos,
            environ,
            offline,
            results: Vec::new(),
            slug: String::new(),
            path: None,
            gh_env: None,
            via: "",
            last_err: String::new(),
            last_rc: 0,
            rest_pr: None,
            rest_pr_why: String::new(),
        }
    }

    fn claim(&self, key: &str) -> Option<&'a PyValue> {
        let ev: &'a Event = self.ev;
        dget(&ev.claims, key).ok().flatten()
    }

    fn claim_true(&self, key: &str) -> Option<&'a PyValue> {
        self.claim(key).filter(|v| truthy(v))
    }

    fn matched(&mut self, claim: &str) {
        let line = format!("MATCH {claim}{}", self.via);
        self.results.push((Status::Match, line));
    }

    fn mismatch(&mut self, claim: &str, expected: &str, got: &str) {
        let line = format!(
            "MISMATCH {claim}: expected {expected}, got {got}{}",
            self.via
        );
        self.results.push((Status::Mismatch, line));
    }

    fn unchecked(&mut self, claim: &str, reason: &str) {
        let line = format!("UNCHECKED {claim}: {reason}");
        self.results.push((Status::Unchecked, line));
    }

    fn run(
        &self,
        argv: &[String],
        env: Option<&[(String, String)]>,
    ) -> Result<(i32, String, String), PyExc> {
        guard_read_only(argv)?;
        if self.offline && tool_is_network(argv) {
            return Ok((
                127,
                String::new(),
                "offline (VERIFY_CLAIM_OFFLINE=1)".into(),
            ));
        }
        Ok(runner::run(argv, env))
    }

    /// `_err(err)`: the first line of `err`, at most 120 characters.
    fn err_line(err: &str) -> String {
        let s = strip(err);
        let first = splitlines(s).first().copied().unwrap_or("no output");
        first.chars().take(120).collect()
    }

    fn compare_sha(&mut self, claim: &str, claimed: &str, actual: &PyValue) -> Result<(), PyExc> {
        let actual_text = match actual {
            PyValue::Str(s) => s.as_str(),
            _ => "",
        };
        let st = sha_state(claimed, actual_text);
        // `actual` is already `x or ""`; a truthy non-str has no `.lower()`.
        if matches!(st, "ok" | "differs") && !matches!(actual, PyValue::Str(_)) {
            return Err(pyops::attr_err(actual, "lower"));
        }
        let label = format!("{claim} {claimed}");
        match st {
            "ok" => self.matched(&label),
            "differs" => self.mismatch(&label, claimed, &py_str(actual)),
            "short" => self.unchecked(
                &label,
                &format!("claimed sha has under {MIN_SHA} hex characters"),
            ),
            _ => self.unchecked(&label, "not a hex sha"),
        }
        Ok(())
    }

    fn compare_eq(&mut self, claim: &str, expected: &PyValue, got: &PyValue) {
        let e = py_str(expected);
        if py_eq(expected, got) {
            self.matched(&format!("{claim} {e}"));
        } else {
            self.mismatch(&format!("{claim} {e}"), &e, &py_str(got));
        }
    }

    fn no_account_reason(&self) -> String {
        match self.ev.repo.as_deref().filter(|r| !r.is_empty()) {
            Some(r) => format!("repo {} has no mapped gh account", py_repr(r)),
            None => "event has no repo".into(),
        }
    }

    /// `resolve_repo()`: the slug, checkout and gh environment of the event's repo.
    fn resolve_repo(&mut self) -> bool {
        let Some(repo) = self.ev.repo.as_deref().filter(|r| !r.is_empty()) else {
            return false;
        };
        let name = strip(repo);
        let lower = name.to_lowercase();
        if let Some((_, (slug, path))) = self.repos.0.iter().find(|(k, _)| *k == lower) {
            self.slug = slug.clone();
            self.path = Some(path.clone());
        } else if name.contains('/') {
            self.slug = name.to_owned();
            for (_, (slug, path)) in &self.repos.0 {
                if slug.to_lowercase() == lower {
                    self.slug = slug.clone();
                    self.path = Some(path.clone());
                }
            }
        } else {
            return false;
        }
        let owner = self.slug.split('/').next().unwrap_or("").to_lowercase();
        let Some((_, dir)) = self.repos.1.iter().find(|(o, _)| *o == owner) else {
            return false;
        };
        let mut env: Vec<(String, String)> = self
            .environ
            .iter()
            .filter(|(k, _)| k != "GH_TOKEN" && k != "GITHUB_TOKEN")
            .cloned()
            .collect();
        if let Some(dir) = dir.clone().filter(|d| !d.is_empty()) {
            env.retain(|(k, _)| k != "GH_CONFIG_DIR");
            env.push(("GH_CONFIG_DIR".into(), dir));
        }
        self.gh_env = Some(env);
        true
    }

    /// `_gh_json(argv)`: the decoded stdout (`None` for null or a decode
    /// failure) and the failure line.
    fn gh_json(&mut self, argv: &[String]) -> Result<(Option<PyValue>, String), PyExc> {
        let env = self.gh_env.clone();
        let (rc, out, err) = self.run(argv, env.as_deref())?;
        self.last_err = format!("{err}\n{out}");
        self.last_rc = rc;
        Ok(match loads(&out) {
            Ok(PyValue::Null) => (None, String::new()),
            Ok(v) => (Some(v), String::new()),
            Err(_) => (
                None,
                Self::err_line(if err.is_empty() { &out } else { &err }),
            ),
        })
    }

    /// `verify()`: every result line, in order.
    ///
    /// # Errors
    /// [`PyExc`] where the script raises (the guard's refusal, a reply or a
    /// claim of a type the script cannot use).
    pub(crate) fn verify(mut self) -> Result<Vec<(Status, String)>, PyExc> {
        let ev = self.ev;
        let pr_on = ev.pr.as_ref().is_some_and(int_truthy);
        let ci_on = ev.ci_run.as_ref().is_some_and(int_truthy);
        let has_gh = pr_on || ci_on || !ev.issues.is_empty();
        let repo_ok = self.resolve_repo();
        if has_gh && !repo_ok {
            let why = self.no_account_reason();
            if let (true, Some(pr)) = (pr_on, &ev.pr) {
                self.unchecked(&format!("pr {}", pyops::int_str(pr)), &why);
            }
            if let (true, Some(ci)) = (ci_on, &ev.ci_run) {
                self.unchecked(&format!("ci_run {}", pyops::int_str(ci)), &why);
            }
            self.check_issues()?;
        } else if pr_on {
            self.check_pr()?;
        }
        if ci_on && repo_ok {
            self.check_ci()?;
        }
        if repo_ok {
            self.check_issues()?;
        }
        if !pr_on {
            self.check_local()?;
        }
        self.check_crate()?;
        if self.results.is_empty() {
            self.unchecked("event", "no checkable claims");
        }
        Ok(self.results)
    }
}

/// `bool(i)` for an integer.
fn int_truthy(i: &crate::pyjson_load::PyInt) -> bool {
    !matches!(i, crate::pyjson_load::PyInt::Small(0))
}
