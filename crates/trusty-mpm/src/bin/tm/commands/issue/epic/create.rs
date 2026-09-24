//! `tm issue epic create` — file a tracker and its phase issues (#8447).
//!
//! Why: the hand-run procedure this replaces is eleven `gh` calls whose order
//! matters and whose partway failure is normal. Three properties make it safe
//! to run again rather than to finish by hand: the tracker is renamed to its
//! real title in the call immediately after it is filed, so a REPORTED failure
//! never leaves a placeholder-titled issue; a re-run adopts a placeholder one
//! anyway and finishes the retitle, which is what a KILLED process — SIGKILL,
//! no error, no unwinding — can leave between those two calls; and a phase
//! whose title already exists under the tracker is skipped, so re-running files
//! only what is missing.
//! What: [`create`] resolves the plan document's publish state, parses it,
//! files or adopts the tracker, files the missing phases as native sub-issues,
//! and regenerates the tracker's `phases` block through
//! [`super::sync::sync`].
//!
//! # Where failure is allowed to be partial
//!
//! Exactly one operation degrades instead of failing: a project attach the
//! token's SCOPE refuses, which [`is_scope_refusal`] identifies by the message
//! gh and GitHub both put it in. Any other attach failure — a 502, a rate
//! limit, a bad project number — fails the run, because it is retryable and a
//! waiver comment for it would be a permanent record of a transient outage.
//! The scope arm posts a `no-project: <reason>` comment, once per issue, and
//! the run continues. If the waiver comment ITSELF fails, the run fails: an
//! issue with no project and no waiver has nothing recording it. Everything
//! else fails closed, including the tracker lookup, whose failure would
//! otherwise file a duplicate tracker.
//!
//! That comment is a record for a human reader, not a machine-read waiver:
//! `core::issue_audit` has hatches for `no-milestone:` and
//! `no-component-label:` only, and its project row has none. Adding one is a
//! `TICKETING.md` change for the owner, not a side effect of this verb.
//!
//! Test: `create_repairs_a_missing_project_waiver_on_a_skipped_phase`,
//! `create_files_a_tracker_then_its_phases`,
//! `create_never_leaves_a_placeholder_title_when_a_phase_fails`,
//! `create_retitles_a_placeholder_tracker_on_re_run`,
//! `create_skips_a_phase_that_already_exists`,
//! `create_refuses_a_plan_doc_absent_from_origin_main`,
//! `create_refuses_a_plan_doc_with_no_epic_plan_heading`,
//! `create_waives_a_project_attach_the_token_refuses`,
//! `create_propagates_a_non_scope_attach_error`,
//! `create_leaves_one_waiver_per_issue_across_two_runs`,
//! `create_attaches_an_adopted_tracker_to_the_project`,
//! `create_fails_when_the_waiver_comment_fails`,
//! `create_refuses_when_the_tracker_search_fails`,
//! `create_labels_every_issue_it_files`.

use std::path::PathBuf;

use super::backend::{ChildIssue, EpicBackend, NewIssue, PUBLISH_REF};
use super::plan::{self, EpicPlan};
use super::render;
use crate::commands::issue::standard_live::one_line;

/// The comment prefix recording on the issue why it carries no project.
///
/// Shaped after the standard's `no-milestone:` and `no-component-label:`
/// hatches, and read the same way by THIS verb — which skips posting a second
/// one — but by nothing else: `core::issue_audit`'s project row has no waiver
/// arm, so this is a record for a human reader rather than something that turns
/// an audit FAIL into a SKIP. Kept byte-for-byte anyway, so a future reader on
/// either side matches it literally.
pub(crate) const NO_PROJECT_PREFIX: &str = "no-project:";

/// Whether an attach failure is the token's project scope refusing.
///
/// Why: this is the ONE attach failure that degrades to a waiver comment
/// instead of failing the run, so the line between it and everything else has
/// to be drawn on the error text rather than on "an attach failed". A 502 or a
/// rate limit is retryable, and writing `no-project: …502…` onto every issue
/// would make a transient outage a permanent record.
/// What: matches gh's handled wording (`your authentication token is missing
/// required scopes [project]`) and the raw GraphQL message it wraps, both of
/// which carry `required scopes`, case-insensitively.
/// Test: `a_scope_refusal_is_recognised_in_both_wordings`.
pub(crate) fn is_scope_refusal(text: &str) -> bool {
    let lowered = text.to_lowercase();
    lowered.contains("required scopes") || lowered.contains("missing required scope")
}

/// Everything `tm issue epic create` needs beyond the plan document.
///
/// Why: the labels, milestone and project an issue must carry are the
/// standard's requirements (#7067, #7097), and none of them is derivable from
/// a plan document — so they are inputs, and their absence is a refusal rather
/// than an issue filed without them.
/// What: the plan path, the milestone every issue takes, the component
/// label(s), the phase type label, the optional project number, the workstream
/// session name, an optional tracker to resume, and the dry-run flag.
/// Test: `create_labels_every_issue_it_files`.
#[derive(Debug, Clone)]
pub(crate) struct CreateOptions {
    /// Path to the committed plan document, as the operator named it.
    pub(crate) plan_path: PathBuf,
    /// Milestone title; the tracker's, and every phase's.
    pub(crate) milestone: String,
    /// Component label(s) — one or more, never zero.
    pub(crate) components: Vec<String>,
    /// The type label each phase carries (D6: one of the existing six).
    pub(crate) phase_type: String,
    /// Owner-scoped project number, when the run should attach one.
    pub(crate) project: Option<u64>,
    /// Workstream session name behind the `ws/<session>` label.
    pub(crate) session: String,
    /// An existing tracker to resume into, skipping the search.
    pub(crate) tracker: Option<u64>,
    /// Plan and report without mutating anything.
    pub(crate) dry_run: bool,
}

/// What one `create` run did.
///
/// Test: `create_files_a_tracker_then_its_phases`.
#[derive(Debug, Clone)]
pub(crate) struct CreateReport {
    /// The tracker, once known; `None` only on a dry run with no tracker yet.
    pub(crate) tracker: Option<u64>,
    /// The SHA-pinned plan permalink the tracker links.
    pub(crate) plan_url: String,
    /// `(phase number, issue number)` for each phase this run filed.
    pub(crate) filed: Vec<(u64, u64)>,
    /// Phase titles already present under the tracker.
    pub(crate) skipped: Vec<String>,
    /// One line per issue whose project attach was waived.
    pub(crate) waived: Vec<String>,
    /// Whether nothing was mutated.
    pub(crate) dry_run: bool,
}

/// File an epic tracker and its phases from a committed plan document.
///
/// Why: the entry point for the whole verb; the ordering constraints that make
/// it resumable live here and nowhere else.
/// What: refuses unless the plan is on [`PUBLISH_REF`], parses it, adopts or
/// files the tracker (filing is two calls — create with a placeholder, rename
/// with the real number — and nothing between them derives a title; a tracker
/// found still wearing the placeholder is adopted and retitled, since a killed
/// process can stop between those two calls), attaches the project once for the
/// tracker on every path, then files each plan phase that has no child with the
/// same title, numbering it max+1 over every existing child. Finishes by
/// regenerating the `phases` block so the tracker is complete when the command
/// returns.
/// Test: see the module doc.
pub(crate) fn create<B: EpicBackend>(
    backend: &B,
    opts: &CreateOptions,
) -> anyhow::Result<CreateReport> {
    let shown = opts.plan_path.display().to_string();
    let text = std::fs::read_to_string(&opts.plan_path)
        .map_err(|e| anyhow::anyhow!("could not read the plan document {shown}: {e}"))?;
    let rel = backend.repo_relative_path(&shown)?.ok_or_else(|| {
        anyhow::anyhow!("git does not track {shown} — commit it, then push it to {PUBLISH_REF}")
    })?;
    let sha = publish_sha(backend, &rel)?;
    let plan = plan::parse(&rel, &text)?;
    let repo = backend.repo_slug()?;
    let plan_url = render::plan_permalink(&repo, &sha, &rel);

    if opts.dry_run {
        return dry_run_report(backend, opts, &plan, plan_url);
    }

    let mut report = CreateReport {
        tracker: None,
        plan_url,
        filed: Vec::new(),
        skipped: Vec::new(),
        waived: Vec::new(),
        dry_run: false,
    };
    let tracker = match opts.tracker {
        Some(n) => n,
        None => adopt_or_file_tracker(backend, opts, &plan, &mut report)?,
    };
    report.tracker = Some(tracker);
    // #8447: ONE tracker attach, on every path. Doing it inside the filing
    // branch left an adopted tracker — and every `--tracker <n>` run — with
    // neither a project nor a waiver recording why.
    attach_project_or_waive(backend, &repo, tracker, opts.project, &mut report.waived)?;

    let mut children = backend.children(tracker)?;
    let total = plan.phases.len();
    for (index, phase) in plan.phases.iter().enumerate() {
        if let Some(existing) = children
            .iter()
            .find(|c| render::phase_what(&c.title) == phase.title)
        {
            // #8447: a run that died between `create_issue` and the project
            // attach left a child with no project AND no waiver comment — a
            // standard violation with nothing recording it. Skipping the
            // attach on the re-run would make that state permanent, so the
            // skip branch re-runs it and repairs what the interruption left.
            let number = existing.number;
            attach_project_or_waive(backend, &repo, number, opts.project, &mut report.waived)?;
            report.skipped.push(phase.title.clone());
            continue;
        }
        // #8447: max+1 over the children as they stand RIGHT NOW, recomputed
        // each iteration so two phases filed in one run cannot collide.
        let number = render::next_phase_number(&children);
        let body = render::phase_body(phase, tracker, index + 1, total);
        let spec = NewIssue {
            title: render::phase_title(tracker, number, &phase.title),
            body,
            labels: phase_labels(opts),
            milestone: opts.milestone.clone(),
            parent: Some(tracker),
        };
        let filed = backend.create_issue(&spec)?;
        attach_project_or_waive(backend, &repo, filed, opts.project, &mut report.waived)?;
        children.push(ChildIssue {
            number: filed,
            title: spec.title,
            state: "OPEN".to_string(),
            labels: spec.labels,
            body: spec.body,
        });
        report.filed.push((number, filed));
    }

    super::sync::sync(backend, tracker)?;
    Ok(report)
}

/// The commit the plan document reached [`PUBLISH_REF`] on, or the refusal.
///
/// Why: D4's refusal is only actionable when it says which SHA the operator
/// has locally and which ref it was compared against — "not on origin/main"
/// alone does not distinguish an unpushed commit from an unfetched remote.
/// Test: `create_refuses_a_plan_doc_absent_from_origin_main`.
fn publish_sha<B: EpicBackend>(backend: &B, rel: &str) -> anyhow::Result<String> {
    if let Some(sha) = backend.publish_sha(rel)? {
        return Ok(sha);
    }
    let local = backend.local_sha(rel)?;
    match local {
        Some(sha) => anyhow::bail!(
            "{rel} is committed locally at {sha} but absent from {PUBLISH_REF} — push it (and \
             `git fetch origin`) before filing the epic, so the tracker's plan link resolves for \
             everyone"
        ),
        None => anyhow::bail!(
            "{rel} is not committed on HEAD and absent from {PUBLISH_REF} — commit and push it \
             before filing the epic"
        ),
    }
}

/// Adopt the tracker this plan already has, or file a new one.
///
/// Why: resumability without a flag. A lookup that did not conclusively
/// enumerate the candidate set is NOT "no tracker exists" — treating it that
/// way files a duplicate tracker, which is the one mistake this verb cannot
/// undo — so any such lookup is a refusal that names `--tracker` as the way
/// past it. The backend contract puts that burden on the lookup itself:
/// `Ok(None)` is a positive answer, and anything less is `Err`.
/// What: [`EpicBackend::find_tracker`] over the tracker's own label set, then
/// the two-call filing sequence.
/// Test: `create_refuses_when_the_tracker_search_fails`,
/// `create_files_a_tracker_then_its_phases`.
fn adopt_or_file_tracker<B: EpicBackend>(
    backend: &B,
    opts: &CreateOptions,
    plan: &EpicPlan,
    report: &mut CreateReport,
) -> anyhow::Result<u64> {
    let labels = tracker_labels(opts);
    let found = backend.find_tracker(&plan.outcome, &labels).map_err(|e| {
        anyhow::anyhow!(
            "could not determine whether a tracker for this plan already exists ({}) — refusing \
             to file one that might be a duplicate; pass `--tracker <number>` to resume a known \
             tracker",
            one_line(&e)
        )
    })?;
    if let Some(existing) = found {
        // #8447: a placeholder title is what a KILLED run leaves between the
        // create and the retitle. Adopting it and finishing the retitle is
        // exactly the call that run owed — the same repair the phase-skip
        // branch makes for a missing project attach. Refusing instead would
        // strand the epic on a manual rename.
        if existing.placeholder {
            retitle(backend, existing.number, &plan.outcome)?;
        }
        return Ok(existing.number);
    }

    let spec = NewIssue {
        title: render::placeholder_tracker_title(&plan.outcome),
        body: render::tracker_body(plan, &report.plan_url),
        labels,
        milestone: opts.milestone.clone(),
        parent: None,
    };
    let tracker = backend.create_issue(&spec)?;
    // #8447: D1's second step runs IMMEDIATELY, before any phase work, so a
    // REPORTED failure never leaves a placeholder-titled issue. A killed
    // process still can, which is what the adopt-and-retitle path above is for.
    retitle(backend, tracker, &plan.outcome)?;
    Ok(tracker)
}

/// Give a tracker its real `[EPIC <n>] <outcome>` title.
///
/// Why: one wording for the failure, whether it happens on the filing run or on
/// a re-run finishing what a killed process left — an operator reading it needs
/// the number and the way back in, not which path produced it.
/// Test: `create_names_the_tracker_when_the_retitle_fails`,
/// `create_retitles_a_placeholder_tracker_on_re_run`.
fn retitle<B: EpicBackend>(backend: &B, tracker: u64, outcome: &str) -> anyhow::Result<()> {
    backend
        .set_title(tracker, &render::tracker_title(tracker, outcome))
        .map_err(|e| {
            anyhow::anyhow!(
                "#{tracker} was filed but still carries the placeholder title `{}` ({e}) — \
                 rename it, then re-run with `--tracker {tracker}`",
                render::placeholder_tracker_title(outcome)
            )
        })
}

/// Attach the project, or record on the issue why it has none.
///
/// Why: AC7's live arm — on this host the token's SCOPE refuses the project
/// write, and a run that failed there would file every issue and then exit
/// nonzero with the epic already half-built. The standard has a shape for a
/// requirement that cannot be met: a waiver comment. So THAT failure degrades
/// to one and the run exits 0. Every other attach failure is retryable, so it
/// fails the run rather than writing a permanent record of a transient outage.
/// What: `Ok` when there is no project to attach or the attach succeeded;
/// `Err` when the failure is not [`is_scope_refusal`]; otherwise posts
/// `no-project: <reason>` — skipping the post when the issue already carries
/// one, so two runs leave one waiver, not two — and records the degradation
/// either way, so the report stays truthful about an issue with no project. A
/// failed waiver comment is propagated: that arm leaves nothing on the record.
/// Test: `create_waives_a_project_attach_the_token_refuses`,
/// `create_propagates_a_non_scope_attach_error`,
/// `create_leaves_one_waiver_per_issue_across_two_runs`,
/// `create_fails_when_the_waiver_comment_fails`.
fn attach_project_or_waive<B: EpicBackend>(
    backend: &B,
    repo: &str,
    issue: u64,
    project: Option<u64>,
    waived: &mut Vec<String>,
) -> anyhow::Result<()> {
    let Some(number) = project else {
        return Ok(());
    };
    let Err(e) = backend.attach_project(repo, issue, number) else {
        return Ok(());
    };
    let reason = one_line(&e);
    // #8447: only a scope refusal degrades. A 502 or a rate limit is retryable,
    // and `no-project: …502…` on every issue would outlive the outage.
    if !is_scope_refusal(&reason) {
        return Err(e.context(format!(
            "could not attach #{issue} to project {number}, and the failure is not the token's \
             project scope — refusing to record a retryable outage as a permanent \
             `{NO_PROJECT_PREFIX}` comment"
        )));
    }
    // #8447: a second run with the scope still missing must not post a second
    // waiver. The backend has a comment read for exactly this check.
    let already = backend
        .comments(issue)?
        .iter()
        .any(|c| c.trim_start().starts_with(NO_PROJECT_PREFIX));
    if !already {
        backend.comment(issue, &format!("{NO_PROJECT_PREFIX} {reason}"))?;
    }
    waived.push(format!("#{issue}: {reason}"));
    Ok(())
}

/// Every label the tracker carries: the `epic` type, the workstream, the
/// components (AC7).
fn tracker_labels(opts: &CreateOptions) -> Vec<String> {
    labels_with_type(opts, "epic")
}

/// Every label a phase carries: its type (D6), the workstream, the components.
fn phase_labels(opts: &CreateOptions) -> Vec<String> {
    labels_with_type(opts, &opts.phase_type)
}

/// The shared label set, with `type` first.
///
/// Test: `create_labels_every_issue_it_files`.
fn labels_with_type(opts: &CreateOptions, type_label: &str) -> Vec<String> {
    let mut labels = vec![type_label.to_string(), workstream_label(&opts.session)];
    labels.extend(opts.components.iter().cloned());
    labels
}

/// The `ws/<session>` label name, truncated and hashed the way the harness
/// seeds it.
fn workstream_label(session: &str) -> String {
    trusty_mpm::core::policy_labels::workstream_label(session)
        .map_or_else(|| format!("ws/{session}"), |l| l.name)
}

/// Report what a `--dry-run` would do, having read and mutated nothing.
///
/// Test: `create_dry_run_files_nothing`.
fn dry_run_report<B: EpicBackend>(
    backend: &B,
    opts: &CreateOptions,
    plan: &EpicPlan,
    plan_url: String,
) -> anyhow::Result<CreateReport> {
    let children = match opts.tracker {
        Some(n) => backend.children(n)?,
        None => Vec::new(),
    };
    let mut skipped = Vec::new();
    let mut filed = Vec::new();
    let mut next = render::next_phase_number(&children);
    for phase in &plan.phases {
        if children
            .iter()
            .any(|c| render::phase_what(&c.title) == phase.title)
        {
            skipped.push(phase.title.clone());
        } else {
            filed.push((next, 0));
            next += 1;
        }
    }
    Ok(CreateReport {
        tracker: opts.tracker,
        plan_url,
        filed,
        skipped,
        waived: Vec::new(),
        dry_run: true,
    })
}
