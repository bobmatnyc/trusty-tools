//! Turning an [`Outcome`] into the text the CLI prints.
//!
//! Why: split out of `super` when #5824's one-shot chain pushed it past the
//! 500-SLOC production cap. It earns its own file rather than an arbitrary cut:
//! `super` maps argv onto a capability, this maps a capability's result onto
//! text, and the two directions share nothing but the enums they name.
//!
//! What: [`render`], an exhaustive match over [`Outcome`] — so a capability that
//! arrives without a way to display its result is a compile error, the same
//! enforcement `super::Cli::to_command` provides on the input side — plus the
//! small formatters it needs.
//! Test: `super::cli_tests`.

use crate::chain::ChainReport;
use crate::clone::CloneState;
use crate::registry::TargetKind;
use crate::run::{RepoResult, RunStatus};
use crate::session::{NextStep, Outcome};

/// Render an outcome as the text the CLI prints.
///
/// Why: rendering is the front end's job, so it lives here rather than on
/// [`Outcome`] — the Tauri shell will render the same values as a window. It
/// returns a `String` instead of printing so it is assertable in a test.
/// What: one arm per [`Outcome`] variant.
/// Test: `super::cli_tests::rendering_names_the_root_and_every_area`.
pub fn render(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Guided(status) => {
            let mut out = format!("Working directory: {}\n", status.root.display());
            match &status.manifest {
                Some(m) => out.push_str(&format!(
                    "Engagement: {} ({} configured)\n",
                    m.report.title,
                    count_of(m.repositories.len(), "repository", "repositories")
                )),
                None => out.push_str("Engagement: no manifest yet — nothing has run here\n"),
            }
            // #5797: a download the operator did not ask for is otherwise just
            // a pause. Naming the versions is also what makes the auto-install
            // auditable after the fact.
            if let Some(placed) = &status.installed {
                out.push_str(&format!(
                    "Installed {}: {}\n",
                    count_of(placed.len(), "tool", "tools"),
                    placed
                        .iter()
                        .map(|t| format!("{} {}", t.crate_name, t.version))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            let installed = status.tools.iter().filter(|s| s.installed).count();
            out.push_str(&format!(
                "Tools: {installed}/{} installed\n",
                status.tools.len()
            ));
            out.push_str(&format!("Next: {}\n", describe_next(&status.next)));
            out
        }
        Outcome::WorkDir(report) => {
            let mut out = format!(
                "{}\n  (delete this directory to remove everything this client wrote)\n",
                report.root.display()
            );
            for (area, path) in &report.areas {
                out.push_str(&format!(
                    "  {:<8} {}\n           {}\n",
                    area.dir_name(),
                    path.display(),
                    area.description()
                ));
            }
            out
        }
        Outcome::Manifest(manifest) => {
            let mut out = format!("Title:  {}\n", manifest.report.title);
            if let Some(client) = &manifest.report.client {
                out.push_str(&format!("Client: {client}\n"));
            }
            if let Some(analyst) = &manifest.report.analyst {
                out.push_str(&format!("Analyst: {analyst}\n"));
            }
            out.push_str(&format!(
                "Repositories: {}\n",
                count_of(manifest.repositories.len(), "repository", "repositories")
            ));
            for gap in &manifest.report.gaps {
                out.push_str(&format!("Gap: {gap}\n"));
            }
            out
        }
        Outcome::Tools(statuses) => {
            let mut out = String::new();
            for status in statuses {
                // #5495: "installed" and "installed at a known version" are
                // different states, and the recipient has to be able to tell
                // them apart — a binary this client did not place is one it
                // cannot vouch for, so it is never shown carrying a version.
                let mark = match (status.installed, status.version.is_some()) {
                    (false, _) => "MISSING",
                    (true, true) => "ok",
                    (true, false) => "UNVERIFIED",
                };
                let version = status.version.as_deref().unwrap_or("-");
                out.push_str(&format!(
                    "{mark:<11} {:<16} {version:<12} {}\n",
                    status.tool.binary_name(),
                    status.path.display()
                ));
            }
            out
        }
        Outcome::Installed(installed) => {
            let mut out = format!(
                "Installed and verified {}:\n",
                count_of(installed.len(), "tool", "tools")
            );
            for tool in installed {
                out.push_str(&format!(
                    "  {:<16} {:<12} {}\n",
                    tool.crate_name,
                    tool.version,
                    tool.binary.display()
                ));
            }
            out
        }
        Outcome::Repos(repos) => {
            if repos.is_empty() {
                return "No repositories configured yet — run the guided flow to pick them.\n"
                    .to_string();
            }
            repos
                .iter()
                .map(|r| format!("{:<24} {}\n", r.name, r.path.display()))
                .collect()
        }
        Outcome::Discovered(repos) => {
            // #5487: an empty result here means the credential really can see
            // nothing — every failure is an error, never a short list — so the
            // wording says so rather than hedging.
            if repos.is_empty() {
                return "Your GitHub credential can reach no repositories.\n".to_string();
            }
            let mut out = format!(
                "{} your credential can reach:\n",
                count_of(repos.len(), "repository", "repositories")
            );
            for repo in repos {
                let mut marks = Vec::new();
                if repo.is_private {
                    marks.push("private");
                }
                if repo.is_archived {
                    marks.push("archived");
                }
                out.push_str(&format!(
                    "  {:<40} {}\n",
                    repo.name_with_owner,
                    marks.join(", ")
                ));
            }
            out
        }
        Outcome::Run(report) => render_run(report),
        Outcome::Cloned(report) => render_cloned(report),
        // #5822: an idempotent re-add and a fresh registration are different
        // facts, and an operator re-running `add` over a list needs to see
        // which one happened rather than the same line twice.
        Outcome::Registered(registration) => {
            let verb = if registration.already_registered {
                "already registered"
            } else {
                "registered"
            };
            format!(
                "{verb}: {:<8} {}\n",
                describe_kind(registration.target.kind()),
                registration.target
            )
        }
        Outcome::Targets(list) => {
            let mut out = if list.targets.is_empty() {
                "No targets registered yet — `trusty-audit add repo <owner>/<name>`.\n".to_string()
            } else {
                let mut out = format!(
                    "{} registered:\n",
                    count_of(list.targets.len(), "target", "targets")
                );
                for target in &list.targets {
                    out.push_str(&format!(
                        "  {:<8} {}\n",
                        describe_kind(target.kind()),
                        target
                    ));
                }
                out
            };
            // The registry supersedes the selection file as the record of what
            // this engagement targets, and the sweep still reads that file as
            // the record of what is on disk. Printing only one of the two would
            // read as though the other had been lost.
            if let Some((path, count)) = &list.legacy_selection {
                out.push_str(&format!(
                    "\n{} on disk from a previous `clone`, which the sweep reads from {}.\n",
                    count_of(*count, "repository", "repositories"),
                    path.display()
                ));
            }
            out
        }
        Outcome::Removed(removal) => {
            if removal.was_registered {
                format!("removed: {}\n", removal.target)
            } else {
                format!("{} was not registered — nothing changed.\n", removal.target)
            }
        }
        Outcome::Package(package) => render_package(package),
        // #5824: one section per phase, in the order they ran, so the operator
        // reads the chain the way they watched it. Each phase reuses the arm
        // that renders it on its own, because a chained clone must not read
        // differently from a `taudit clone`.
        Outcome::Audit(report) => render_chain(report),
    }
}

/// The one-shot chain, phase by phase.
///
/// Why: a chained run that printed only its last phase would hide the two
/// places an engagement most often comes up short — a repository that failed to
/// clone, and a registered target the chain cannot audit at all. Both are
/// reported here even though the package assembled fine.
/// What: the install line, the clone report, the sweep report, this chain's own
/// gaps, and the package — the same text each phase prints alone.
/// Test: `super::cli_tests::a_chained_run_names_every_phase`.
fn render_chain(report: &ChainReport) -> String {
    let mut out = String::new();
    if let Some(placed) = &report.installed {
        out.push_str(&format!(
            "Installed {}: {}\n\n",
            count_of(placed.len(), "tool", "tools"),
            placed
                .iter()
                .map(|t| format!("{} {}", t.crate_name, t.version))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(acquired) = &report.acquired {
        out.push_str(&render_cloned(acquired));
        out.push('\n');
    }
    out.push_str(&render_run(&report.run));
    // A gap here is a registered target the chain never attempted, which the
    // sweep's own report has no way to mention — it only knows what it swept.
    for gap in &report.gaps {
        out.push_str(&format!("Not audited: {gap}\n"));
    }
    out.push('\n');
    out.push_str(&render_package(&report.package));
    out
}

/// The deliverable, and what it does not cover.
///
/// Why: #5499 closure condition 3 — the recipient has to be able to find the
/// file. The path is the first and last line, and everything between is what
/// they can check before sending it.
/// Test: `super::cli_tests::a_package_that_omits_a_repository_does_not_exit_zero`.
fn render_package(package: &crate::package::ReturnPackage) -> String {
    let mut out = format!("Return package: {}\n", package.path.display());
    out.push_str(&format!(
        "  {} in {}, unencrypted — open it and read what you are sending\n",
        count_of(package.files.len(), "file", "files"),
        human_bytes(package.packaged_bytes)
    ));
    for file in &package.files {
        out.push_str(&format!(
            "  {:>10}  {}\n",
            human_bytes(file.bytes),
            file.entry
        ));
    }
    for line in &package.excluded {
        out.push_str(&format!("Not included: {line}\n"));
    }
    out.push_str(&format!(
        "\nSend this file back: {}\n",
        package.path.display()
    ));
    out
}

/// The sweep's per-repository results and its verdict.
///
/// Why: #5555 — a partial sweep must not read like a clean one. Each failure is
/// printed with its reason and its log path, and the verdict line says which of
/// the three states this run ended in. A named function rather than an inline
/// arm since #5824, so the chain renders the sweep identically to `taudit run`
/// rather than growing a second wording.
/// What: one line per repository, then the resumed count and the verdict.
/// Test: `super::cli_tests::a_partial_sweep_is_never_rendered_as_a_clean_one`.
fn render_run(report: &crate::run::RunReport) -> String {
    let mut out = String::new();
    for run in &report.repos {
        match &run.result {
            // #5494: a repository this run carried over from an earlier
            // one reads differently from one it audited. Silent
            // skipping is the defect the resume path exists not to be.
            RepoResult::Succeeded if run.resumed => out.push_str(&format!(
                "resumed {:<24} {}\n",
                run.repo.name,
                run.output.display()
            )),
            RepoResult::Succeeded => out.push_str(&format!(
                "ok      {:<24} {}\n",
                run.repo.name,
                run.output.display()
            )),
            RepoResult::Failed { reason } => {
                out.push_str(&format!("FAILED  {:<24} {reason}\n", run.repo.name))
            }
        }
        // #5555: a stated gap is a dimension the sweep could not assess
        // (DOC-67 §9). It does not fail the repository, and it must not
        // be invisible either — an audited repo with four gaps is not
        // the same result as one with none.
        for gap in &run.gaps {
            out.push_str(&format!("  gap   {gap}\n"));
        }
    }
    let audited = report.repos.len() - report.failures().count();
    // #5494: a run that audited nothing because everything was already
    // done is a legitimate outcome, and it has to say so — otherwise
    // "Audited 6 repositories" over a four-second run reads as a bug.
    let resumed = report.resumed().count();
    if resumed > 0 {
        out.push_str(&format!(
            "\nResumed {} from an earlier run; {} audited now.\n",
            count_of(resumed, "repository", "repositories"),
            audited - resumed
        ));
    }
    out.push_str(&match report.status {
        RunStatus::AllSucceeded => format!(
            "\nAudited {}.\n",
            count_of(audited, "repository", "repositories")
        ),
        RunStatus::Partial => format!(
            "\nPARTIAL: {} audited, {} failed. The report covers only what succeeded.\n",
            audited,
            report.failures().count()
        ),
        RunStatus::AllFailed => format!(
            "\nFAILED: no repository was audited ({} attempted).\n",
            report.repos.len()
        ),
    });
    out
}

/// What acquisition put on disk, and what it could not.
///
/// Test: `super::cli_tests::a_failed_clone_is_rendered_as_an_exclusion`.
fn render_cloned(report: &crate::clone::CloneReport) -> String {
    let mut out = String::new();
    for repo in &report.repos {
        // #5215: a repository that is NOT in the audit has to read as
        // excluded, never as a blank line the recipient scrolls past.
        let state = match &repo.state {
            CloneState::Cloned => "cloned".to_string(),
            CloneState::Reused => "already present".to_string(),
            CloneState::Failed(why) => format!("FAILED — {why}"),
            CloneState::Empty(why) => format!("NOTHING CLONED — {why}"),
            CloneState::Skipped(why) => format!("SKIPPED — {why}"),
        };
        out.push_str(&format!("  {:<40} {state}\n", repo.name_with_owner));
    }
    out.push_str(&format!(
        "{} on disk, using {}{}.\n",
        count_of(
            report.repos.iter().filter(|r| r.state.is_usable()).count(),
            "repository",
            "repositories"
        ),
        // #5215 review: a walk that hit something unreadable produces a
        // floor, and saying "using X" of a floor is a confident number
        // nothing measured.
        if report.total_bytes_complete {
            ""
        } else {
            "at least "
        },
        human_bytes(report.total_bytes)
    ));
    for gap in &report.gaps {
        out.push_str(&format!("Gap: {gap}\n"));
    }
    out
}

/// Bytes as something a recipient reads, not a raw count.
pub(super) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The column label for one target kind.
fn describe_kind(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Repo => "repo",
        TargetKind::Board => "board",
    }
}

/// `"1 repository"` / `"3 repositories"` — this text goes to the recipient.
pub(super) fn count_of(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

fn describe_next(next: &NextStep) -> String {
    match next {
        NextStep::SelectRepositories => {
            "pick the repositories to audit (`trusty-audit repos`)".to_string()
        }
        NextStep::InstallTools(missing) => {
            let names: Vec<&str> = missing.iter().map(|t| t.binary_name()).collect();
            format!(
                "install the pinned tools (`trusty-audit install`): {}",
                names.join(", ")
            )
        }
        NextStep::ReadyForRun => "run the audit sweep (`trusty-audit run`)".to_string(),
        NextStep::ReturnPackage => {
            "assemble the deliverable and send it back (`trusty-audit package`)".to_string()
        }
    }
}
