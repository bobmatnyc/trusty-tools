//! `tm issue board check` — read-only drift report (#10276, AC5).
//!
//! Why: a board drifts from the spec by hand edits, and a repo's issues drift
//! off it when auto-add is off. `check` says which, and what the token can do,
//! without writing anything.
//! What: [`check_board`] compares a [`Board`] snapshot with the spec and the
//! repo's `project:` block into a [`CheckReport`]: drift (missing or wrong
//! standard fields, options and views, an unlinked repo, a closed board),
//! repo extras (declared extra fields — not drift), information (README,
//! settings the API cannot make, workflow states, extra views), open issues
//! absent from the board, the token line from `X-OAuth-Scopes`, SSO from a
//! 403 carrying `X-GitHub-SSO`, and a `default_project` mismatch (D8). It
//! never reads the token value: only response headers.
//! Test: `check_reports_drift_extras_info_and_missing_issues_without_mutating_10276`,
//! `check_reports_scopes_and_sso_from_headers_10276`,
//! `add_uses_the_project_block_and_ignores_default_project_10276`.

use std::fmt::Write as _;

use super::client::{TokenProbe, color_name};
use super::project::ProjectBlock;
use super::readme::{manual_steps, render_readme};
use super::reconcile::{data_type, desired_options, layout_name};
use super::snapshot::{Board, CUSTOM_TYPES};
use super::spec::{BoardSpec, ViewLayout};

/// What `check` found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CheckReport {
    /// The board, as `owner #n "title" (url)`.
    pub(crate) board: String,
    /// Departures from the spec.
    pub(crate) drift: Vec<String>,
    /// Declared extra fields present on the board.
    pub(crate) extras: Vec<String>,
    /// Information only.
    pub(crate) info: Vec<String>,
    /// Open issues of the repo not on the board.
    pub(crate) missing_issues: Vec<u64>,
    /// The token type and scopes line(s).
    pub(crate) token: Vec<String>,
    /// SSO authorisation required, with the header value.
    pub(crate) sso_required: Option<String>,
    /// `agents.ticketing.default_project` names a different board (D8).
    pub(crate) mismatch: Option<String>,
}

impl CheckReport {
    /// Clean: no drift, no missing issue, no SSO block.
    pub(crate) fn clean(&self) -> bool {
        self.drift.is_empty() && self.missing_issues.is_empty() && self.sso_required.is_none()
    }

    /// The printed report.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "board: {}", self.board);
        section(&mut out, "drift", &self.drift);
        section(&mut out, "repo extras (not drift)", &self.extras);
        section(&mut out, "information", &self.info);
        let missing: Vec<String> = self
            .missing_issues
            .iter()
            .map(|n| format!("#{n}"))
            .collect();
        section(&mut out, "open issues not on the board", &missing);
        section(&mut out, "token type and scopes", &self.token);
        if let Some(sso) = &self.sso_required {
            let _ = writeln!(out, "SSO authorisation required: {sso}");
        }
        if let Some(m) = &self.mismatch {
            let _ = writeln!(out, "attach-target mismatch: {m}");
        }
        let verdict = if self.clean() { "clean" } else { "drift" };
        let _ = writeln!(out, "verdict: {verdict}");
        out
    }
}

fn section(out: &mut String, title: &str, lines: &[String]) {
    let _ = writeln!(out, "{title} ({}):", lines.len());
    for line in lines {
        let _ = writeln!(out, "  - {line}");
    }
}

/// The token lines and the SSO state from a probe (AC5).
///
/// What: scopes present → a classic or OAuth token and its scopes, with a
/// note when `project` is absent; header absent → a fine-grained or App
/// token, which a user-owned board refuses. A 403 with `X-GitHub-SSO` is SSO.
pub(crate) fn token_lines(probe: &TokenProbe) -> (Vec<String>, Option<String>) {
    let mut lines = Vec::new();
    match &probe.scopes {
        Some(scopes) => {
            lines.push(format!(
                "type: classic or OAuth token (sends X-OAuth-Scopes); scopes: {}",
                if scopes.is_empty() {
                    "(none)".to_string()
                } else {
                    scopes.join(", ")
                }
            ));
            if !scopes.iter().any(|s| s == "project") {
                lines.push(
                    "no `project` scope: init, apply and add need it (`read:project` reads only)"
                        .to_string(),
                );
            }
        }
        None => lines.push(
            "type: fine-grained or GitHub App token (no X-OAuth-Scopes header); a user-owned \
             board needs a classic token"
                .to_string(),
        ),
    }
    let sso = match (probe.status, &probe.sso) {
        (Some(403), Some(sso)) => Some(sso.clone()),
        _ => None,
    };
    (lines, sso)
}

/// Compare `board` with `spec` and `block` for `repo`.
///
/// What: see the module doc. `on_board` is the issue numbers of `repo` on the
/// board; `open` the repo's open issue numbers.
pub(crate) fn check_board(
    spec: &BoardSpec,
    block: &ProjectBlock,
    board: &Board,
    repo: &str,
    open: &[u64],
    on_board: &[u64],
) -> CheckReport {
    let mut r = CheckReport {
        board: format!(
            "{} #{} \"{}\" ({})",
            block.owner, board.number, board.title, board.url
        ),
        ..CheckReport::default()
    };
    if board.closed {
        r.drift.push("the board is closed".to_string());
    }
    check_fields(spec, block, board, &mut r);
    check_views(spec, block, board, &mut r);
    if !board.repositories.iter().any(|x| x == repo) {
        r.drift.push(format!("`{repo}` is not linked to the board"));
    }
    if board.readme != render_readme(spec, repo, &block.drop_options) {
        r.info
            .push("README differs from the spec template (information only)".to_string());
    }
    for step in manual_steps(spec) {
        r.info.push(format!("manual: {step}"));
    }
    for (name, enabled) in &board.workflows {
        if let Some(want) = spec.workflows.iter().find(|w| &w.name == name)
            && want.enabled != *enabled
        {
            let state = if *enabled { "on" } else { "off" };
            r.info.push(format!(
                "workflow `{name}` is {state}; the spec wants it {} (the API cannot change it)",
                if want.enabled { "on" } else { "off" }
            ));
        }
    }
    r.missing_issues = open
        .iter()
        .copied()
        .filter(|n| !on_board.contains(n))
        .collect();
    r
}

fn check_fields(spec: &BoardSpec, block: &ProjectBlock, board: &Board, r: &mut CheckReport) {
    for field in &spec.fields {
        let Some(live) = board.field(&field.name) else {
            r.drift
                .push(format!("standard field `{}` is missing", field.name));
            continue;
        };
        if live.data_type != data_type(field.kind) {
            r.drift.push(format!(
                "field `{}` is {}, the spec says {}",
                field.name,
                live.data_type,
                data_type(field.kind)
            ));
            continue;
        }
        let want = desired_options(field, &block.drop_options);
        for o in &want {
            match live.options.iter().find(|l| l.name == o.name) {
                None => r
                    .drift
                    .push(format!("`{}` lacks option `{}`", field.name, o.name)),
                Some(l) if l.color != color_name(o.color) || l.description != o.description => {
                    r.drift.push(format!(
                        "`{}` option `{}` is {} \"{}\", the spec says {} \"{}\"",
                        field.name,
                        o.name,
                        l.color,
                        l.description,
                        color_name(o.color),
                        o.description
                    ));
                }
                Some(_) => {}
            }
        }
        for l in live
            .options
            .iter()
            .filter(|l| !want.iter().any(|o| o.name == l.name))
        {
            r.drift.push(format!(
                "`{}` carries option `{}`, which the spec and project.drop_options exclude",
                field.name, l.name
            ));
        }
        let live_order: Vec<&str> = live
            .options
            .iter()
            .map(|l| l.name.as_str())
            .filter(|n| want.iter().any(|o| o.name == *n))
            .collect();
        let want_order: Vec<&str> = want
            .iter()
            .map(|o| o.name.as_str())
            .filter(|n| live_order.contains(n))
            .collect();
        if live_order != want_order {
            r.drift.push(format!(
                "`{}` options are out of the spec's order",
                field.name
            ));
        }
    }
    for live in &board.fields {
        let standard = spec.field(&live.name).is_some();
        let builtin = spec.builtin_fields.contains(&live.name)
            || !CUSTOM_TYPES.contains(&live.data_type.as_str());
        if standard || builtin {
            continue;
        }
        if block.extra_fields.contains(&live.name) {
            r.extras.push(live.name.clone());
        } else {
            r.drift.push(format!(
                "field `{}` is not standard; declare it under project.extra_fields",
                live.name
            ));
        }
    }
    for extra in &block.extra_fields {
        if board.field(extra).is_none() {
            r.info.push(format!(
                "declared extra field `{extra}` is not on the board"
            ));
        }
    }
}

fn check_views(spec: &BoardSpec, block: &ProjectBlock, board: &Board, r: &mut CheckReport) {
    for view in &spec.views {
        let Some(live) = board.view(&view.name) else {
            r.drift.push(format!("view `{}` is missing", view.name));
            continue;
        };
        if live.layout != layout_name(view.layout) {
            r.drift.push(format!(
                "view `{}` is {}, the spec says {}",
                view.name,
                live.layout,
                layout_name(view.layout)
            ));
            continue;
        }
        if live.filter != view.filter {
            r.drift.push(format!(
                "view `{}` filter is \"{}\", the spec says \"{}\"",
                view.name, live.filter, view.filter
            ));
        }
        // Repo extras may be shown; they are not drift (D6).
        let shown: Vec<&String> = live
            .visible
            .iter()
            .filter(|f| !block.extra_fields.contains(f))
            .collect();
        let want: Vec<&String> = view.fields.iter().collect();
        if shown != want {
            let line = format!(
                "view `{}` shows [{}], the spec says [{}]",
                view.name,
                join(&shown),
                join(&want)
            );
            // Owner decision 1: Roadmap visible fields cannot be set by the API.
            if view.layout == ViewLayout::Roadmap {
                r.info.push(format!("{line} (set by hand)"));
            } else {
                r.drift.push(line);
            }
        }
    }
    for live in &board.views {
        if !spec.views.iter().any(|v| v.name == live.name) {
            r.info
                .push(format!("view `{}` is not in the spec", live.name));
        }
    }
}

fn join(names: &[&String]) -> String {
    names
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}
