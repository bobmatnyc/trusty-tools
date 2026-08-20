//! Grounding the code analysis in a real index and real complexity numbers (#6081).
//!
//! Why: `trusty-review report --analyze` reads its findings, complexity
//! distribution and health factors from a trusty-analyze daemon, and
//! trusty-analyze answers from a trusty-search index. Neither is started by
//! anything this crate runs on its own, and the miss is fail-open at every hop:
//! an unindexed repository produces `AnalyzeGap::NotIndexed`, one gap line, exit
//! 0, and a report whose code-analysis sections are empty. Empty reads as a
//! clean bill of health. That is what this module exists to stop.
//!
//! It is also the PRODUCER for #6078. trusty-review's `inspect_priority` is the
//! stable interface an external ranker writes its selection intelligence to
//! (owner ruling 2026-08-19: all audit intelligence lives in trusty-audit, the
//! manifest is the interface). #6078 shipped the consumer; nothing wrote it.
//! [`ground_manifest`] does: it ranks the repository's files by the complexity
//! trusty-analyze measured and writes that ranking into the manifest, so the
//! investigation pass inspects the code a tool already flagged rather than the
//! code whose PATH NAME looked interesting.
//!
//! What: four legs, in prerequisite order, each with one job.
//!
//! | Module | Owns |
//! |---|---|
//! | [`daemons`] | is trusty-search up, is trusty-analyze up — and starting them |
//! | [`index`] | is this checkout in trusty-search, and indexing it if not |
//! | [`hotspots`] | asking trusty-analyze which files are worst, and ranking them |
//! | [`evidence`] | asking trusty-search where each DD dimension's evidence is (#6082) |
//! | [`priority`] | writing that ranking, and the gaps, into the manifest |
//!
//! ## Fail-open, and never silently
//!
//! Every leg degrades rather than failing the run: a recipient's one-shot sweep
//! over an org cannot spend its one shot on a daemon that will not boot. But
//! every degradation is NAMED twice — once in the [`Grounding::gaps`] the caller
//! surfaces on the console, and once in the manifest's own `[report].gaps`, so
//! the rendered report states it too. A gap this module produces always says
//! which repository, which tool, and what the report will therefore not carry.
//!
//! ## What it deliberately does not do
//!
//! It starts no daemon of its own and adds no configuration. The two addresses
//! come from where every other client of those daemons reads them — the
//! trusty-search discovery files via [`trusty_common::daemon_guard`], and
//! `TRUSTY_ANALYZE_URL` for trusty-analyze — and the two binaries from the
//! engagement's pinned copies, falling back to the same `TRUSTY_SEARCH_BIN` /
//! `TRUSTY_ANALYZE_BIN` overrides `tga audit` honours.
//!
//! Test: `super::grounding_tests`, plus each submodule's own.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::tools::RequiredTool;
use crate::workdir::WorkDir;

pub mod daemons;
pub mod evidence;
pub mod hotspots;
pub mod index;
pub mod priority;

#[cfg(test)]
mod grounding_tests;

/// The two daemons this module drives: where they are, and what starts them.
///
/// Why: every address and binary is a VALUE rather than something the functions
/// read from the environment themselves. That is what lets the failure arms be
/// tested against a dead port and a stub executable without any test touching
/// `std::env::set_var`, which is `unsafe` in edition 2024 and unsound under the
/// parallel harness — the same split `tga::audit::AnalyzeGuard` takes.
/// What: a binary and a base URL per daemon, plus the shared readiness budget.
/// Test: `super::grounding_tests`, which constructs one directly for every arm.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Tools {
    /// The `trusty-search` binary to run — indexing, and starting the daemon.
    pub search: PathBuf,
    /// Base address of the trusty-search daemon, e.g. `http://127.0.0.1:7878`.
    pub search_url: String,
    /// The `trusty-analyze` binary to spawn when its daemon is absent.
    pub analyze: PathBuf,
    /// Base address of the trusty-analyze daemon, e.g. `http://127.0.0.1:7879`.
    pub analyze_url: String,
    /// Wall-clock budget for a freshly-spawned daemon to BIND its port.
    pub bind_timeout: Duration,
    /// Wall-clock budget for a listening daemon to answer `/health`.
    pub startup_timeout: Duration,
    /// Interval between readiness probes.
    pub poll_interval: Duration,
}

impl Tools {
    /// The tools an engagement runs with: its pinned binaries, live addresses.
    ///
    /// Why: the sweep already resolved and VERIFIED the pinned pair, so handing
    /// them in keeps the daemon this starts and the binary the sweep checked
    /// from ever being two different installs — the same reason
    /// `tga::audit::SearchGuard` resolves its binary once at the entry point.
    /// What: the two paths as given, with both addresses resolved by
    /// [`daemons::search_base_url`] and [`daemons::analyze_base_url`].
    /// Test: `super::grounding_tests::pinned_tools_keep_the_paths_they_were_given`.
    #[must_use]
    pub fn pinned(search: PathBuf, analyze: PathBuf) -> Self {
        Self {
            search,
            search_url: daemons::search_base_url(),
            analyze,
            analyze_url: daemons::analyze_base_url(),
            bind_timeout: daemons::BIND_TIMEOUT,
            startup_timeout: daemons::STARTUP_TIMEOUT,
            poll_interval: trusty_common::daemon_guard::DEFAULT_POLL_INTERVAL,
        }
    }

    /// The tools a re-render runs with: whatever is installed here.
    ///
    /// Why: `crate::rerender` runs on the machine that HOLDS the package, which
    /// may not be the one that produced it, and #6080's owner ruling is that the
    /// verb runs at current versions with no pin check. So the working
    /// directory's copy is preferred (a recipient who ran the engagement
    /// re-renders with the binaries that produced the reports, without saying
    /// so) and the bare name is the fallback for the OS to resolve on `PATH`.
    /// What: `tools/<name>` when it is a file, else the environment override,
    /// else the bare binary name.
    /// Test: `super::grounding_tests::the_installed_copies_are_preferred_over_the_path`.
    #[must_use]
    pub fn resolved(work: &WorkDir) -> Self {
        Self::pinned(
            resolve_binary(work, RequiredTool::TrustySearch, daemons::ENV_SEARCH_BIN),
            resolve_binary(work, RequiredTool::TrustyAnalyze, daemons::ENV_ANALYZE_BIN),
        )
    }
}

/// One tool's binary: the installed copy, else an override, else the bare name.
fn resolve_binary(work: &WorkDir, tool: RequiredTool, env: &str) -> PathBuf {
    let installed = tool.path_in(work);
    if installed.is_file() {
        return installed;
    }
    match std::env::var(env) {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(tool.binary_name()),
    }
}

/// What grounding one repository produced.
///
/// Why: the two outputs are consumed by different readers. `priorities` is for
/// the manifest — the interface trusty-review's investigation pass reads
/// (#6078). `gaps` is for the operator, and for the report's own Gaps & Caveats
/// section, and it is the whole reason this is fail-open rather than silent.
/// What: the trusty-search index id (`None` when the checkout yielded no
/// basename), the ranked repo-relative paths, and one line per degradation.
/// Test: `super::grounding_tests`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Grounding {
    /// The trusty-search index id this repository was indexed under.
    pub index_id: Option<String>,
    /// Ranked repo-relative paths, each naming the signal that chose it
    /// (#6082). Empty only when neither daemon produced anything.
    pub priorities: Vec<priority::Priority>,
    /// One line per leg that degraded, naming the repository and the cost.
    pub gaps: Vec<String>,
}

impl Grounding {
    /// A grounding that produced nothing but the reason it produced nothing.
    fn gap(reason: String) -> Self {
        Self {
            index_id: None,
            priorities: Vec::new(),
            gaps: vec![reason],
        }
    }
}

/// Index `checkout`, search it per DD dimension, measure it, and rank its files.
///
/// Why: see the module docs — without this the code-analysis leg of the report
/// reads nothing and says nothing, over a clean exit. #6082 adds the discovery
/// leg: complexity alone ranked one dimension's worth of files, so the ranking
/// steered the whole inference budget at the same corner of the repository.
/// What: trusty-search must answer before a repository can be indexed, and a
/// repository must be indexed before either later leg can read it. From there
/// the two ranking legs are INDEPENDENT — search-driven per-dimension evidence
/// needs only trusty-search, so a dead trusty-analyze costs the complexity
/// ranking and the `--analyze` enrichment, not the evidence discovery.
/// [`evidence::blend`] interleaves whichever of the two produced anything.
///
/// # Postconditions
/// Never panics and never returns an error: every failure is a line in
/// [`Grounding::gaps`], one line, safe to show the recipient, naming `display`.
/// [`Grounding::priorities`] is empty only when neither leg produced a path,
/// and that case is itself a named gap.
///
/// Test: `super::grounding_tests`, which drives one arm per leg.
pub async fn ground(
    tools: &Tools,
    checkout: &Path,
    display: &str,
    instructions: Option<&str>,
) -> Grounding {
    let Some(index_id) = index::index_id_for(checkout) else {
        return Grounding::gap(format!(
            "{display}: {} has no final path component, so no trusty-search index id could be \
             derived — the report's code-analysis sections are not assessed for it",
            checkout.display()
        ));
    };

    if let Err(cause) = daemons::ensure_search(tools).await {
        return Grounding::gap(format!(
            "{display}: {cause} — the report's findings, complexity distribution and health \
             factors are not assessed for it, and its investigation pass ranks files by path \
             name alone"
        ));
    }

    if let Err(cause) = index::ensure_indexed(&tools.search, checkout, &index_id) {
        return Grounding {
            index_id: Some(index_id),
            priorities: Vec::new(),
            gaps: vec![format!(
                "{display}: {cause} — the report's findings, complexity distribution and health \
                 factors are not assessed for it, and its investigation pass ranks files by path \
                 name alone"
            )],
        };
    }

    // #6082: the discovery leg. It asks the index where each DD dimension's
    // evidence is, so a selected file arrives already attributed to what it is
    // evidence FOR.
    let discovery = match evidence::HttpSearch::new(&tools.search_url, &index_id, checkout) {
        Ok(client) => evidence::discover(&client, instructions).await,
        Err(cause) => evidence::Discovery {
            dimensions: Vec::new(),
            failures: vec![cause],
        },
    };
    let mut gaps = discovery_gaps(&discovery, display);

    let hotspots = complexity(tools, &index_id, checkout, display, &mut gaps).await;
    let priorities = evidence::blend(
        &hotspots,
        &discovery.dimensions,
        evidence::MAX_PRIORITY_PATHS,
    );
    if priorities.is_empty() {
        gaps.push(format!(
            "{display}: neither the search index nor trusty-analyze named a file to inspect, so \
             the investigation pass ranks its files by path name alone"
        ));
    }
    Grounding {
        index_id: Some(index_id),
        priorities,
        gaps,
    }
}

/// The complexity ranking, or the gap explaining why there is none.
async fn complexity(
    tools: &Tools,
    index_id: &str,
    checkout: &Path,
    display: &str,
    gaps: &mut Vec<String>,
) -> Vec<String> {
    if let Err(cause) = daemons::ensure_analyze(tools).await {
        gaps.push(format!(
            "{display}: {cause} — the report's findings, complexity distribution and health \
             factors are not assessed for it"
        ));
        return Vec::new();
    }
    match hotspots::fetch(&tools.analyze_url, index_id, checkout).await {
        Ok(ranked) if ranked.is_empty() => {
            gaps.push(format!(
                "{display}: trusty-analyze measured no complexity hotspot for it, so its ranking \
                 carries search-derived evidence only"
            ));
            ranked
        }
        Ok(ranked) => ranked,
        Err(cause) => {
            gaps.push(format!(
                "{display}: {cause} — its ranking carries search-derived evidence only"
            ));
            Vec::new()
        }
    }
}

/// What the discovery leg could not do, one line each.
///
/// Why: an empty discovery and a discovery whose queries all failed produce the
/// same ranking, and only the gap line tells the reader which happened. Both
/// degrade to the pre-#6082 behaviour, and neither may do so silently.
/// What: one line when no dimension produced evidence, and one line when any
/// query failed — naming the count and the first cause rather than repeating a
/// dead daemon fourteen times.
/// Test: `super::grounding_tests::a_search_that_answers_nothing_is_a_named_gap`.
fn discovery_gaps(discovery: &evidence::Discovery, display: &str) -> Vec<String> {
    let mut gaps = Vec::new();
    if let Some(first) = discovery.failures.first() {
        gaps.push(format!(
            "{display}: {} of this repository's evidence queries failed ({first}) — its \
             investigation covers fewer dimensions than the report's list",
            discovery.failures.len()
        ));
    }
    if discovery.dimensions.is_empty() {
        gaps.push(format!(
            "{display}: the search index matched no evidence for any due-diligence dimension, so \
             the investigation pass ranks its files by complexity and path name alone"
        ));
    }
    gaps
}

/// [`ground`], then write what it produced into `manifest`.
///
/// Why: the manifest is the interface (owner ruling 2026-08-19). A ranking this
/// process holds and does not write reaches no renderer — not the one the sweep
/// runs, not `crate::rerender`, and not the recipient's own re-render of the
/// delivered package, which is the whole point of putting it in the file that
/// ships rather than in a value that does not.
/// What: the gaps [`ground`] produced, plus one more when the manifest itself
/// could not be updated. A write failure is a gap rather than an error for the
/// same reason every other leg is: it costs this repository's ranking, not the
/// sweep.
/// Test: `super::grounding_tests::{hotspots_become_ranked_inspect_priority_in_the_manifest,
/// a_manifest_that_cannot_be_written_is_a_named_gap}`.
pub async fn ground_manifest(
    manifest: &Path,
    tools: &Tools,
    checkout: &Path,
    display: &str,
) -> Vec<String> {
    let instructions = instructions_from(manifest);
    let mut grounding = ground(tools, checkout, display, instructions.as_deref()).await;
    if let Err(cause) = priority::write_into(
        manifest,
        checkout,
        &grounding.priorities,
        Some(priority::Budget::from_env()),
        &grounding.gaps,
    ) {
        grounding.gaps.push(format!(
            "{display}: {cause} — the rendered report does not state this repository's \
             evidence ranking"
        ));
    }
    grounding.gaps
}

/// Longest analyst brief the discovery leg derives queries from.
const MAX_INSTRUCTION_CHARS: usize = 8 * 1024;

/// The analyst brief this manifest declares, when it declares one (#6082).
///
/// Why: the brief states what THIS engagement is worried about, and the
/// discovery leg turns its topics into queries. Reading it here rather than
/// taking it as a parameter keeps the sweep's call site unchanged — the whole
/// feature costs the operator no new step.
/// What: `[report].instructions`, resolved against the manifest directory when
/// relative (the same rule trusty-review applies), truncated to
/// [`MAX_INSTRUCTION_CHARS`]. Every failure — no key, no file, unreadable —
/// reads as "no brief", because a brief is optional and its absence costs only
/// the extra queries.
/// Test: `super::grounding_tests::the_brief_a_manifest_declares_is_read`.
fn instructions_from(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let doc: toml::Value = toml::from_str(&text).ok()?;
    let declared = doc.get("report")?.get("instructions")?.as_str()?;
    let declared = Path::new(declared);
    let resolved = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        manifest.parent()?.join(declared)
    };
    let brief = std::fs::read_to_string(resolved).ok()?;
    Some(brief.chars().take(MAX_INSTRUCTION_CHARS).collect())
}
