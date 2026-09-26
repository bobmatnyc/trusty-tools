//! Which `gh` invocations a read-only dispatch may run (#8567).
//!
//! Why: the #8439 allowlist refused every `gh` call, so a `research` brief
//! that opened with `gh issue view N --comments` could not run, and a
//! `code-critic` could not read a PR's checks; the PM relayed each GitHub read
//! through a writing agent.
//! What: [`check_gh`] names the reads and refuses everything else:
//! - `gh issue view|list`, `gh pr view|list|diff|checks` and
//!   `gh run view|list`, with any option but `--web`/`-w`, which starts a
//!   browser program, and `--watch`, which blocks on CI. A short cluster
//!   holding `w` is refused, so `-Rowner/web` is over-refused;
//! - `gh api <endpoint>` that sends GET: `-X`/`--method` only `GET`, the
//!   options in [`API_FLAGS`] and [`API_VALUED`] only (so `-f`, `-F`,
//!   `--field`, `--raw-field` and `--input`, which send a body and switch the
//!   default method to POST, are refused), a `-H` header only `Accept:` or
//!   `X-GitHub-Api-Version:`, and never the `graphql` endpoint, whose query
//!   can hold a mutation. A `for` variable is refused anywhere in `gh api`.
//!
//! Every other verb — `pr create|merge|review|comment|edit|checkout`,
//! `issue create|comment|edit|close`, `repo`, `release`, `auth`, and any verb
//! gh adds later — is refused.
//! Out of scope: a program run by configuration that already exists, such as
//! `GH_PAGER` or `PAGER`.
//! Test: `read_only_allow_tests::gh_read_verbs_are_allowed`,
//! `read_only_allow_tests::gh_mutating_and_unknown_verbs_are_refused`,
//! `read_only_allow_tests::gh_api_get_forms_are_allowed`,
//! `read_only_allow_tests::gh_api_writes_are_refused`.

use super::read_only_programs::Arg;

type Verdict = Result<(), String>;

/// Each resource and the verbs on it that only read.
const READ_VERBS: &[(&str, &[&str])] = &[
    ("issue", &["view", "list"]),
    ("pr", &["view", "list", "diff", "checks"]),
    ("run", &["view", "list"]),
];

/// Valueless `gh api` options that change only what is printed.
const API_FLAGS: &[&str] = &[
    "--paginate",
    "--slurp",
    "-i",
    "--include",
    "--silent",
    "--verbose",
];

/// `gh api` options that take one value; `-X`/`-H` values are judged too.
const API_VALUED: &[&str] = &[
    "-X",
    "--method",
    "-H",
    "--header",
    "-q",
    "--jq",
    "-t",
    "--template",
    "-p",
    "--preview",
    "--hostname",
];

/// Judge a `gh` argv (`rest` excludes `gh` itself).
///
/// Why: see the module doc.
/// What: `Ok` for an allowlisted read; `Err` naming the refusal otherwise.
/// Test: as the module doc.
pub(super) fn check_gh(rest: &[Arg]) -> Verdict {
    let resource = rest
        .first()
        .and_then(Arg::text)
        .ok_or("`gh` without a literal command")?;
    if resource == "api" {
        return api(&rest[1..]);
    }
    let verb = rest.get(1).and_then(Arg::text).unwrap_or_default();
    let reads = READ_VERBS
        .iter()
        .any(|(r, verbs)| *r == resource && verbs.contains(&verb));
    if !reads {
        return Err(format!(
            "`gh {resource} {verb}`, which is not a read verb (a read-only agent runs gh issue \
             view/list, pr view/list/diff/checks, run view/list and api GET only)"
        ));
    }
    for t in rest[2..].iter().filter_map(Arg::text) {
        let name = t.split('=').next().unwrap_or(t);
        let short_web = t
            .strip_prefix('-')
            .is_some_and(|c| !c.starts_with('-') && c.contains('w'));
        if matches!(name, "--web" | "--watch") || short_web {
            return Err(format!(
                "`gh {resource} {verb} {t}`, which opens a browser or blocks on CI"
            ));
        }
    }
    Ok(())
}

/// `gh api` that sends a GET and no request body.
fn api(rest: &[Arg]) -> Verdict {
    let mut endpoint = None;
    let mut i = 0;
    while let Some(arg) = rest.get(i) {
        let t = arg
            .text()
            .ok_or("a `for` variable in `gh api`, whose endpoint or method it could be")?;
        i += 1;
        if !t.starts_with('-') {
            if endpoint.replace(t).is_some() {
                return Err("`gh api` with more than one endpoint".into());
            }
            continue;
        }
        if API_FLAGS.contains(&t) {
            continue;
        }
        // A joined value: `--name=value`, or `-Xvalue` for a short option.
        let (name, joined) = match t.split_once('=') {
            Some((n, v)) if t.starts_with("--") => (n, Some(v)),
            _ if !t.starts_with("--") && t.len() > 2 => (&t[..2], Some(&t[2..])),
            _ => (t, None),
        };
        if !API_VALUED.contains(&name) {
            return Err(format!(
                "`gh api {name}`, which is not a GET-only option (a field or `--input` sends a \
                 request body)"
            ));
        }
        let value = match joined {
            Some(v) => v,
            None => {
                i += 1;
                rest.get(i - 1)
                    .and_then(Arg::text)
                    .ok_or_else(|| format!("`gh api {name}` without a literal value"))?
            }
        };
        api_value(name, value)?;
    }
    let endpoint = endpoint.ok_or("`gh api` without an endpoint")?;
    let path = endpoint.trim_start_matches('/');
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if path.eq_ignore_ascii_case("graphql") {
        return Err("`gh api graphql`, whose query can carry a mutation".into());
    }
    Ok(())
}

/// Judge the value of a valued `gh api` option.
fn api_value(name: &str, value: &str) -> Verdict {
    match name {
        "-X" | "--method" if !value.eq_ignore_ascii_case("GET") => {
            Err(format!("`gh api {name} {value}`, a method other than GET"))
        }
        "-H" | "--header" => {
            let header = value.to_ascii_lowercase();
            if header.starts_with("accept:") || header.starts_with("x-github-api-version:") {
                return Ok(());
            }
            Err("a `gh api` header other than `Accept:` or `X-GitHub-Api-Version:`".into())
        }
        _ => Ok(()),
    }
}
