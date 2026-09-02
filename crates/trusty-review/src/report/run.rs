//! The report pipeline as a library entry point (#6669).
//!
//! Why: this pipeline used to live in `trusty-review`'s own binary
//! (`src/cli_report.rs`), reachable only by spawning that binary. `trusty-analyze`
//! already embeds this crate for its `tr_review_*` tools, so a second front door
//! there would have meant a second implementation of manifest loading, template
//! precedence, the credential preflight, investigation and synthesis — five
//! places to drift. Moving the pipeline into the library gives both front doors
//! one implementation: `trusty-review report` and `trusty-analyze report` now
//! differ only in how they parse their arguments.
//! What: [`ReportRequest`] is the argument-parser-free description of one run,
//! and [`run_report`] executes it and returns the paths written. The clap
//! `ReportArgs` in the binary maps onto `ReportRequest` and calls this.
//! Test: `run_tests.rs` covers the precedence rules and the credential
//! preflight; the render path end to end by `tests/report_e2e.rs`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::config::{Provider, ReviewConfig};
use crate::llm::{build_provider, resolve_model, resolve_provider_and_model};
use crate::report::investigate::{
    Investigation, Verifier, apply_investigation, merge_investigation_prose,
};
use crate::report::manifest::Manifest;
use crate::report::model::{InferenceAttribution, ReportModel, RoleAttribution};
use crate::report::synthesize::{Synthesis, Synthesizer, ground_investigation_prose};
use crate::report::template::{DEFAULT_TEMPLATE, TemplateLoader, resolve_template_alias};
use crate::report::{
    Budget, CorpusSnapshot, Instructions, Reporter, benchmark, discover_manifest_instructions,
    load_instructions, load_manifest, parse_section_instructions, run_investigation,
    section_instructions,
};

/// One report run, described without reference to any argument parser.
///
/// Why: the CLI, the MCP tool and a future in-process caller all describe the
/// same run; a plain struct is what lets them share [`run_report`] without the
/// library depending on the binary's clap types.
/// What: the manifest path is the only required field; [`ReportRequest::new`]
/// fills every other with the same default the CLI has.
/// Test: `run_tests::defaults_match_the_cli`.
///
/// `#[non_exhaustive]`: this grows a field whenever the pipeline gains an
/// option, and a struct literal outside this crate would break on each one.
/// Build it with [`ReportRequest::new`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReportRequest {
    /// Path to the report manifest TOML file.
    pub manifest: PathBuf,
    /// Template name or alias; `None` defers to the manifest then the default.
    pub template: Option<String>,
    /// Render the non-code sections as stated out-of-scope boundaries (#6669).
    pub code_only: bool,
    /// Analyst instructions markdown file, overriding the manifest key.
    pub instructions: Option<PathBuf>,
    /// Output directory for the generated report pair.
    pub out: PathBuf,
    /// Investigation cap: files sent per repository.
    pub investigate_max_files: Option<usize>,
    /// Investigation cap: total content bytes sent per repository.
    pub investigate_max_bytes: Option<usize>,
    /// Benchmark corpus directory.
    pub corpus: Option<PathBuf>,
    /// Append each analyzed repository's snapshot to the corpus after a run.
    pub corpus_add: bool,
    /// Compute cross-repo percentile/quartile placement against the corpus.
    pub benchmark: bool,
    /// Disable Mermaid chart rendering under the graph-appendix tables.
    pub no_mermaid: bool,
    /// Populate metrics deterministically from the trusty-analyze daemon.
    pub analyze: bool,
}

impl ReportRequest {
    /// A run over `manifest` with every other option at its default.
    #[must_use]
    pub fn new(manifest: impl Into<PathBuf>) -> Self {
        Self {
            manifest: manifest.into(),
            template: None,
            code_only: false,
            instructions: None,
            out: PathBuf::from("./reports"),
            investigate_max_files: None,
            investigate_max_bytes: None,
            corpus: None,
            corpus_add: false,
            benchmark: false,
            no_mermaid: false,
            analyze: false,
        }
    }
}

/// Execute one report run and return the files written.
///
/// Why: the single implementation both `trusty-review report` and
/// `trusty-analyze report` drive, so the two cannot disagree about template
/// precedence, the credential preflight, or what a code-only render means.
/// What: loads and validates the manifest, resolves the template name and the
/// code-only decision by precedence, loads the template, builds the enriched
/// model, runs the investigation and synthesis passes, and writes the markdown
/// and JSON pair. Progress goes to STDERR; the returned paths are the caller's
/// to print.
///
/// # Errors
///
/// When the manifest will not load, when the inference credential is absent,
/// when the template name resolves to nothing, or when synthesis produces no
/// verified prose — #5454 made inference required, so none of those degrades to
/// a deterministic report.
///
/// Test: `the_request_template_wins`, `the_manifest_can_declare_code_only`; end
/// to end by `tests/report_e2e.rs`.
pub async fn run_report(config_path: Option<&Path>, req: &ReportRequest) -> Result<Vec<PathBuf>> {
    // #6135: the manifest is read BEFORE the config, because it is a config
    // layer — it carries the provider and per-role models of the run that
    // produced it, and those outrank this host's `config.toml`. Reading one
    // small TOML file keeps #5454's promise intact: the credential is still
    // checked before a single repository is walked.
    eprintln!(
        "[trusty-review report] Loading manifest: {}",
        req.manifest.display()
    );
    let manifest = load_manifest(&req.manifest)
        .with_context(|| format!("failed to load manifest {}", req.manifest.display()))?;
    let declared = manifest.inference.as_ref().map(|i| i.as_role_layer());
    let config = ReviewConfig::from_env_and_manifest(config_path, None, declared.as_ref());
    let attribution = resolve_attribution(
        &config,
        match &declared {
            Some(_) => "the manifest's [inference] section",
            None => "this host's environment and config",
        },
    )?;
    eprintln!("[trusty-review report] Inference: {}", attribution.line());

    // #5454: the credential is checked before any repository is walked. A DD
    // render takes minutes, and discovering an unset key at the end of it wastes
    // all of them.
    preflight_inference_credential(&config)?;

    let template_name = resolve_template_name(req, &manifest);
    // #6669: the scope decision, resolved on the same three tiers.
    let code_only = resolve_code_only(req, &manifest);
    eprintln!(
        "[trusty-review report] Template: {template_name}; repositories: {}; scope: {}",
        manifest.repositories.len(),
        if code_only { "code-only" } else { "full" }
    );

    let template = TemplateLoader::new()
        .load(&template_name)
        .with_context(|| format!("failed to load template '{template_name}'"))?;

    // Analyst instructions (#2340): CLI flag wins over the manifest key; a
    // relative manifest path resolves against the manifest directory.
    let instructions = load_report_instructions(req, &manifest)?;
    if let Some(instr) = &instructions {
        eprintln!(
            "[trusty-review report] Analyst instructions: {}",
            instr.source.display()
        );
    }

    let mut model = ReportModel::build(
        &manifest,
        &req.manifest,
        &template_name,
        instructions.as_ref(),
    )
    .context("failed to assemble report model")?;
    // #6135: what ran, on the page and in the JSON twin.
    model.inference = Some(attribution);

    // #2357 layered instructions: parse the active template's own
    // `<!-- instruct:<section_id> ... -->` overrides (if any) BEFORE the render
    // pipeline strips them as ordinary instructional comments; recorded on the
    // model so the synthesis prompt builder resolves template-override-else-
    // generic-default per section, and the JSON twin stays a faithful record.
    model.section_instructions = parse_section_instructions(&template);
    if code_only {
        // #6669: fully resolve here and write the result back as the
        // template tier, which is what lets the addendum reach every section
        // without threading a boolean through the whole prompt builder. The
        // analyst brief still layers additively on top.
        model.section_instructions =
            section_instructions::resolve_code_only(&model.section_instructions);
    }
    if !model.section_instructions.is_empty() {
        eprintln!(
            "[trusty-review report] Template section-instruction overrides: {}",
            model
                .section_instructions
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // Epic #2445: opt-in deterministic analyze fetch.  Runs AFTER the model is
    // built (so declared metrics files already loaded and win) and BEFORE
    // synthesis/benchmark/mermaid (so the complexity chart + finding bands see
    // the live metrics).  Fully fail-open — populates only local-path repos that
    // declared no metrics, and only when their index is served.
    if req.analyze {
        enrich_from_analyze(&mut model, &manifest, &config).await;
    }

    // LLM synthesis plus the repo-evidence investigation. #5454: required, so a
    // failure here ends the run — no report is written and the manifest the
    // caller passed in is untouched, which is what makes re-running this exact
    // command the recovery path.
    eprintln!("[trusty-review report] Synthesis — calling LLM provider...");
    let budget = resolve_budget(req, &manifest);
    // #6082: manifest-only, with no CLI flag — the producer that reached a
    // healthy search index is the only party that knows whether the declared
    // list is the whole intended sample.
    let attributed_only = manifest.report.attributed_only.unwrap_or(false);
    let synthesis = run_synthesis(&config, &mut model, budget, attributed_only, &req.out)
        .await
        .with_context(|| {
            format!(
                "inference is required for a due-diligence report, and this run produced none. \
                 Nothing collected is lost — re-run `trusty-review report --manifest {} --analyze \
                 --out {}` once the cause is addressed",
                req.manifest.display(),
                req.out.display()
            )
        })?;
    eprintln!(
        "[trusty-review report] synthesis complete — {} guardrail disclosure(s)",
        synthesis.notes.len()
    );
    model.synthesis = Some(synthesis);

    // M3: resolve the corpus directory once (shared by --benchmark and
    // --corpus-add).  A `--corpus` flag with neither consumer is a no-op — warn.
    let corpus_dir = if req.benchmark || req.corpus_add {
        Some(resolve_corpus_dir(req, &manifest)?)
    } else {
        if req.corpus.is_some() {
            eprintln!(
                "[trusty-review report] --corpus given without --benchmark or --corpus-add; ignoring"
            );
        }
        None
    };

    // M3: compute placement BEFORE writing so it renders into the report.  The
    // target's fresh snapshot is always included in the population; loading the
    // corpus here (before any --corpus-add write) avoids double-counting a stale
    // copy of the target.
    if req.benchmark {
        let dir = corpus_dir.as_ref().expect("resolved when --benchmark");
        eprintln!(
            "[trusty-review report] Benchmarking against corpus: {}",
            dir.display()
        );
        let corpus = benchmark::load_corpus(dir).context("failed to load benchmark corpus")?;
        let targets = target_snapshots(&model);
        let report = benchmark::build_benchmark_report(&corpus, &targets);
        eprintln!(
            "[trusty-review report] Benchmark: corpus size {}, {} target(s)",
            report.corpus_size,
            report.repositories.len()
        );
        model.benchmark = Some(report);
    }

    // #2366: Mermaid charts on by default; disabled by --no-mermaid OR the
    // manifest `[report] mermaid = false`.  The flag forces off unconditionally.
    let mermaid = manifest.report.mermaid.unwrap_or(true) && !req.no_mermaid;
    let reporter = Reporter::new(&req.out)
        .with_mermaid(mermaid)
        .with_code_only(code_only);
    let written = reporter
        .write(&model, &template)
        .context("failed to write report output")?;

    // M3: persist snapshots only after a successful write.
    if req.corpus_add {
        let dir = corpus_dir.as_ref().expect("resolved when --corpus-add");
        let mut added = 0usize;
        for snap in target_snapshots(&model) {
            let path = benchmark::write_snapshot(dir, &snap)
                .with_context(|| format!("failed to write corpus snapshot for {}", snap.slug))?;
            eprintln!(
                "[trusty-review report] Corpus += {}",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            );
            added += 1;
        }
        eprintln!(
            "[trusty-review report] Added {added} snapshot(s) to {}",
            dir.display()
        );
    }

    eprintln!("[trusty-review report] Wrote {} file(s):", written.len());
    Ok(written)
}

/// Resolve the template name by precedence, then expand any alias (#6669).
///
/// Why: three layers can name a template and one order must govern all of
/// them — an explicit request outranks the manifest that travelled with the
/// data, which outranks the environment an orchestrator injected.
/// What: request field > manifest `[report].template` >
/// `TRUSTY_AUDIT_REPORT_TEMPLATE` > [`DEFAULT_TEMPLATE`], with
/// [`resolve_template_alias`] applied last so `cast` works at every tier.
/// Test: `run_tests::{the_request_template_wins,
/// the_environment_template_is_read_below_the_manifest, the_cast_alias_expands}`.
fn resolve_template_name(req: &ReportRequest, manifest: &Manifest) -> String {
    let chosen = req
        .template
        .clone()
        .or_else(|| manifest.report.template.clone())
        .or_else(env_template)
        .unwrap_or_else(|| DEFAULT_TEMPLATE.to_string());
    resolve_template_alias(&chosen).to_string()
}

/// A non-blank `TRUSTY_AUDIT_REPORT_TEMPLATE`, or `None`.
fn env_template() -> Option<String> {
    let raw = std::env::var(trusty_common::env_vars::ENV_AUDIT_REPORT_TEMPLATE).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Resolve the code-only decision by precedence (#6669).
///
/// Why: the flag is a switch, not a tri-state, so it can only turn the mode ON —
/// which is the honest direction. A manifest or an engagement that declared a
/// code-only audit must not be silently widened by a caller who simply omitted
/// the flag.
/// What: true when the request asks for it, OR the manifest declares
/// `[report] code_only = true`, OR `TRUSTY_AUDIT_REPORT_CODE_ONLY` is set to a
/// recognised truthy value.
/// Test: `run_tests::{the_manifest_can_declare_code_only,
/// the_environment_can_declare_code_only, an_unrecognised_env_value_reads_as_absent}`.
fn resolve_code_only(req: &ReportRequest, manifest: &Manifest) -> bool {
    req.code_only
        || manifest.report.code_only.unwrap_or(false)
        || env_flag(trusty_common::env_vars::ENV_AUDIT_REPORT_CODE_ONLY)
}

/// True when `name` holds a recognised truthy value.
///
/// Why: an unrecognised value must read as absent rather than as `true` — a
/// typo silently narrowing what a report says its scope was is the one failure
/// this decision cannot afford.
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|raw| {
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Run the opt-in deterministic analyze fetch, recording what it could not
/// reach (#2445, #5239).
async fn enrich_from_analyze(model: &mut ReportModel, manifest: &Manifest, config: &ReviewConfig) {
    let analyze_socket = manifest
        .report
        .analyze_socket
        .clone()
        .map_or_else(|| config.analyzer_socket.clone(), std::path::PathBuf::from);
    eprintln!(
        "[trusty-review report] --analyze: fetching over {}",
        analyze_socket.display()
    );
    match crate::report::HttpAnalyzeMetricsSource::new(analyze_socket) {
        Ok(source) => {
            // #5239: every repo the fetch could not populate is named in the
            // report, not only warned about on stderr — a dimension missing
            // because the daemon was down must not read as a clean pass.
            let gaps = crate::report::enrich_with_analyze_gaps(model, &source).await;
            for gap in &gaps {
                eprintln!("[trusty-review report] --analyze gap: {gap}");
            }
            model.gaps.extend(gaps);
        }
        Err(e) => {
            eprintln!(
                "[trusty-review report] --analyze: could not build HTTP client ({e}); \
                 falling back to scan"
            );
            // #5239: the client never existed, so no repo was assessed — that is
            // a whole-report gap, not a per-repo one.
            model.gaps.push(
                "trusty-analyze data unavailable — the analysis client could not be built, so no \
                 application in this report was assessed against trusty-analyze. Findings, \
                 complexity, and health factors are not assessed, not clean."
                    .to_string(),
            );
        }
    }
}

/// Resolve the benchmark corpus directory or fail with a clear message.
///
/// Why: `--benchmark` / `--corpus-add` both require a corpus directory; the
/// resolution precedence (request field > manifest `[report].corpus` > XDG
/// default) lives here, and the only failure is a platform with no data dir and
/// no explicit source — which must be a clear error, not a panic.
/// What: delegates to [`benchmark::corpus_dir`] with the manifest directory as
/// the base for a relative manifest key; maps `None` to an actionable error.
///
/// # Errors
///
/// When no corpus directory can be resolved from any tier.
///
/// Test: `run_tests::the_explicit_corpus_beats_the_manifest`.
fn resolve_corpus_dir(req: &ReportRequest, manifest: &Manifest) -> Result<PathBuf> {
    let manifest_dir = req.manifest.parent().unwrap_or_else(|| Path::new("."));
    benchmark::corpus_dir(
        req.corpus.as_deref(),
        manifest.report.corpus.as_deref(),
        manifest_dir,
    )
    .ok_or_else(|| {
        anyhow::anyhow!(
            "no benchmark corpus directory: pass --corpus <dir> or set [report].corpus (the per-user default is unavailable on this platform)"
        )
    })
}

/// Resolve and load the auditor instructions brief, honouring precedence
/// (#2340, extended by #6180).
///
/// Why: the brief may come from the request, the manifest
/// `[report].instructions` key, or — since #6180 — an `instructions.md` the
/// engagement author dropped beside the manifest with nothing declaring it. The
/// request wins, then the key, then discovery; a relative key is resolved
/// against the manifest directory so authors write portable paths.
/// What: returns `Ok(None)` when no source is configured AND no file was
/// discovered; otherwise loads via [`load_instructions`] (missing file → error;
/// empty file → warn + `None`) or [`discover_manifest_instructions`] (absent →
/// `None`; present but unreadable → error).
///
/// # Errors
///
/// When a declared brief cannot be read.
///
/// Test: instructions loading/validation is covered by `instructions_tests.rs`.
fn load_report_instructions(
    req: &ReportRequest,
    manifest: &Manifest,
) -> Result<Option<Instructions>> {
    let manifest_dir = req.manifest.parent().unwrap_or_else(|| Path::new("."));
    let resolved: Option<PathBuf> = match &req.instructions {
        Some(p) => Some(p.clone()),
        None => manifest.report.instructions.as_ref().map(|rel| {
            let p = PathBuf::from(rel);
            if p.is_absolute() {
                p
            } else {
                manifest_dir.join(p)
            }
        }),
    };
    match resolved {
        Some(path) => Ok(load_instructions(&path)
            .with_context(|| format!("failed to load analyst instructions {}", path.display()))?),
        // #6180: nothing was declared, so look for the file that travels with
        // the engagement. Absent is the normal case and changes nothing.
        None => Ok(discover_manifest_instructions(&req.manifest)?),
    }
}

/// Build one corpus snapshot per analyzed repository that has metrics.
fn target_snapshots(model: &ReportModel) -> Vec<CorpusSnapshot> {
    model
        .repositories
        .iter()
        .filter_map(|r| CorpusSnapshot::from_repository(r, &model.generated_date))
        .collect()
}

/// Resolve the investigation file/byte budget by precedence (#2357, #6082).
///
/// Why: the wave-3 investigation caps how much of a repo is sent to the LLM;
/// operators tune it via the request or a manifest key, falling back to sane
/// defaults, in one place so both budget dimensions resolve consistently. The
/// environment tier sits BELOW both because it crosses the two process
/// boundaries the manifest cannot — see
/// `trusty_common::env_vars::ENV_AUDIT_INVESTIGATE_MAX_FILES`.
/// What: request field > manifest `[report].investigate_max_*` >
/// `TRUSTY_AUDIT_INVESTIGATE_MAX_*` > [`Budget::default`].
/// Test: `run_tests::the_environment_budget_is_read_below_the_manifest`.
fn resolve_budget(req: &ReportRequest, manifest: &Manifest) -> Budget {
    resolve_budget_from(
        req,
        manifest,
        env_budget(trusty_common::env_vars::ENV_AUDIT_INVESTIGATE_MAX_FILES),
        env_budget(trusty_common::env_vars::ENV_AUDIT_INVESTIGATE_MAX_BYTES),
    )
}

/// [`resolve_budget`]'s precedence rule, over values already read.
///
/// Why: taking the environment tier as a parameter is what lets the precedence
/// be tested without `std::env::set_var`, which is `unsafe` in edition 2024 and
/// unsound under the parallel test harness.
/// Test: `run_tests::the_environment_budget_is_read_below_the_manifest`.
fn resolve_budget_from(
    req: &ReportRequest,
    manifest: &Manifest,
    env_files: Option<usize>,
    env_bytes: Option<usize>,
) -> Budget {
    let default = Budget::default();
    Budget {
        max_files: req
            .investigate_max_files
            .or(manifest.report.investigate_max_files)
            .or(env_files)
            .unwrap_or(default.max_files),
        max_bytes: req
            .investigate_max_bytes
            .or(manifest.report.investigate_max_bytes)
            .or(env_bytes)
            .unwrap_or(default.max_bytes),
    }
}

/// A positive integer from `name`, or `None` for absent/unusable.
fn env_budget(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()?
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
}

/// Fail before any work when the required inference credential is absent.
///
/// Why: DOC-67 binds `tga audit` to one shot with no prompts, and an audit sweep
/// runs for minutes before it reaches this pipeline. #5454 made inference
/// required, so the one failure that IS knowable up front — no credential — must
/// be raised up front rather than after a full render's worth of work.
/// What: OpenRouter is the only provider this preflights, per the #5454 owner
/// decision that it is the only inference path for a DD report. A reviewer role
/// resolved to Bedrock or Fireworks is left to fail at the provider-build site,
/// which is also fatal now — this check narrows the window, it does not open a
/// second path. The key's VALUE is never read, printed, or compared here; only
/// whether it is blank.
///
/// # Errors
///
/// When the resolved provider is OpenRouter and its key is blank.
///
/// Test: the rule itself, via [`credential_rule`].
fn preflight_inference_credential(config: &ReviewConfig) -> Result<()> {
    let role = &config.role_models.reviewer;
    // #6135: the id resolves rather than refusing, so what this checks is the
    // credential for the provider the resolution landed on.
    let (provider, _) = resolve_provider_and_model(&role.model, &role.provider)?;
    credential_rule(provider, &config.openrouter_api_key)
}

/// Resolve every role and record what will run (#6135).
///
/// Why: owner ruling 2026-08-21 — the report states which models are used, and
/// an id the resolver adjusted shows both halves. Resolving all three roles here
/// (rather than only the reviewer the synthesis pass builds) is what lets the
/// manifest's whole declared selection be attributed.
/// What: one resolution per role, folded into the record the page and the JSON
/// twin carry. `source` names the layer the selection came from, which is the
/// fact that distinguishes a portable render from one that inherited the host's
/// config.
///
/// # Errors
///
/// Only when a role's id names a provider this build cannot call and has no
/// verified equivalent — see [`crate::llm::resolve_model`].
///
/// Test: `run_tests::{attribution_names_every_role,
/// attribution_shows_a_translated_id_as_requested_then_ran}`.
fn resolve_attribution(config: &ReviewConfig, source: &str) -> Result<InferenceAttribution> {
    let roles = &config.role_models;
    let mut rows = Vec::with_capacity(3);
    for (name, role) in [
        ("reviewer", &roles.reviewer),
        ("verifier", &roles.verifier),
        ("summarizer", &roles.summarizer),
    ] {
        let resolved = resolve_model(&role.model, &role.provider)?;
        if let Some(note) = &resolved.note {
            eprintln!("[trusty-review report] {name} model: {note}");
        }
        rows.push(RoleAttribution::of(name, &role.model, &resolved));
    }
    Ok(InferenceAttribution::of(source, rows))
}

/// The preflight rule itself, as a pure function.
///
/// Why: taking the resolved provider and the key as parameters keeps the
/// decision testable without loading a `ReviewConfig` — which reads the real
/// process environment, so a test of it would pass or fail depending on whether
/// the machine running it happens to have a key exported.
/// What: `Err` only for OpenRouter with a blank key. The key is inspected for
/// emptiness and never copied into the message.
///
/// # Errors
///
/// When `provider` is OpenRouter and `openrouter_api_key` is blank.
///
/// Test: `run_tests::{preflight_rejects_a_blank_openrouter_key,
/// preflight_accepts_a_present_openrouter_key,
/// preflight_leaves_non_openrouter_providers_to_the_build_site,
/// preflight_message_never_echoes_the_key}`.
fn credential_rule(provider: Provider, openrouter_api_key: &str) -> Result<()> {
    if provider != Provider::OpenRouter {
        return Ok(());
    }
    if openrouter_api_key.trim().is_empty() {
        // #6135: the provider is named, because it may have come from the
        // manifest rather than from anything on this machine — an operator
        // reading "set OPENROUTER_API_KEY" on a host configured for Bedrock
        // needs to see WHICH provider asked for it.
        anyhow::bail!(
            "this render runs on the {provider} provider, and {key} is not set. A due-diligence \
             report requires inference, and no other provider is substituted for the one \
             selected. Set it before starting the run:\n\n    export {key}=<your OpenRouter API \
             key>\n\nKeys are issued at https://openrouter.ai/keys.",
            key = trusty_common::env_vars::ENV_OPENROUTER_API_KEY,
        );
    }
    Ok(())
}

/// Build the verifier-role client the trace-verdict pass calls (#6166 leg 2).
///
/// Why: the verdict pass must run on the model the manifest declares for the
/// verifier role, not on the reviewer already in hand — a second opinion from
/// the same call is not a second opinion. Failing to build it is NOT fatal:
/// `run_verdicts` records every traced finding UNVERIFIABLE, which the coverage
/// section and the gaps line both state, and that is strictly more honest than
/// losing a whole render over an optional annotation pass.
/// What: `None` on any build failure, with the reason on stderr so an operator
/// sees why no finding got a verdict.
/// Test: the build path is network-bound; the `None` branch is covered by
/// `verdict_tests::without_a_verifier_every_candidate_is_unverifiable`.
async fn build_verdict_verifier(config: &ReviewConfig) -> Option<Verifier> {
    let role = &config.role_models.verifier;
    match build_provider(&role.model, &role.provider, config).await {
        Ok(provider) => Some(Verifier {
            provider,
            model: role.model.clone(),
        }),
        Err(e) => {
            eprintln!(
                "[trusty-review report] verifier provider unavailable ({e}) — every traced \
                 finding will be recorded as unverifiable"
            );
            None
        }
    }
}

/// Build the LLM provider, run the repo-evidence investigation, then synthesis.
///
/// Why: keeps [`run_report`] readable and isolates the provider-build failure
/// path. #5454 turned that path fatal: a build error used to degrade the report
/// to deterministic output, which is the mode the owner decision removed. The
/// investigation (#2357) runs FIRST so its verified findings are injected into
/// the model before synthesis; its verified prose then wins over any synthesis
/// prose for the same finding.
///
/// # Errors
///
/// When the provider cannot be built, or when the synthesis pass produces no
/// verified prose.
///
/// Test: the build path is network-bound; the failure decisions are covered by
/// `report::synthesize::tests` and `tests/report_investigate.rs` with stubs.
async fn run_synthesis(
    config: &ReviewConfig,
    model: &mut ReportModel,
    budget: Budget,
    attributed_only: bool,
    out_dir: &Path,
) -> Result<Synthesis> {
    let role = &config.role_models.reviewer;
    let provider = build_provider(&role.model, &role.provider, config)
        .await
        .map_err(|e| anyhow::anyhow!("could not build the LLM provider: {e}"))?;

    // #6166 leg 2: the verdict pass runs on the VERIFIER role, built through the
    // same `build_provider` path as the reviewer. A build failure is not fatal
    // here (unlike the reviewer's): the pass fails closed, recording every traced
    // finding as unverifiable, which is a stated gap rather than a lost render.
    let verifier = build_verdict_verifier(config).await;
    if let Some(mut inv) = run_investigation(
        provider.clone(),
        &role.model,
        verifier.as_ref(),
        model,
        budget,
        attributed_only,
    )
    .await
    {
        let verified: usize = inv.repos.iter().map(|r| r.findings.len()).sum();
        eprintln!(
            "[trusty-review report] Investigation: {verified} verified finding(s) across {} local repo(s)",
            inv.repos.len()
        );
        // #6093: persist BEFORE synthesis. Synthesis can fail, and until this
        // landed a failure discarded the whole investigation — minutes of real
        // LLM spend, unrecoverable.
        persist_investigation(out_dir, &inv);
        // #6082 lap 6: guard the investigation's own prose BEFORE it is copied
        // anywhere. `merge_investigation_prose` below overwrites the guarded
        // synthesis prose with this text, so guarding only the synthesis half
        // left the rendered finding stating the claim the guard had rejected.
        let inv_notes = ground_investigation_prose(model, &mut inv);
        apply_investigation(model, &inv);
        // #6009: capture the raw response next to the report output on an
        // unparseable-response failure, so a future occurrence is diagnosable
        // without spending another live call to find out what the model sent.
        let mut synthesis = Synthesizer::new(provider, role.model.clone())
            .with_max_tokens(role.max_tokens)
            .with_raw_capture_dir(out_dir)
            .synthesize(model)
            .await?;
        synthesis.notes.extend(inv_notes);
        merge_investigation_prose(&mut synthesis, &inv);
        Ok(synthesis)
    } else {
        Ok(Synthesizer::new(provider, role.model.clone())
            .with_max_tokens(role.max_tokens)
            .with_raw_capture_dir(out_dir)
            .synthesize(model)
            .await?)
    }
}

/// Filename the pre-synthesis investigation snapshot is written under, inside
/// the report's own output directory.
pub const INVESTIGATION_SNAPSHOT_FILENAME: &str = "investigation.json";

/// Write the verified investigation next to the report output before synthesis
/// runs (#6093).
///
/// Why: the investigation is the expensive half of a report run — several
/// minutes of selection, LLM calls, and mechanical evidence verification. A
/// synthesis failure used to throw all of it away, so recovering meant paying
/// for the whole investigation again. The snapshot lands before the first
/// synthesis call, so it survives every failure downstream of it.
/// What: serialises the investigation to
/// `<out_dir>/`[`INVESTIGATION_SNAPSHOT_FILENAME`], creating the directory if
/// needed, and prints the path. Any failure is a warning, never an error: a
/// recovery aid must not itself abort a run that would otherwise succeed.
/// Test: `run_tests::{investigation_snapshot_is_written_and_reloadable,
/// investigation_snapshot_failure_is_not_fatal}`.
fn persist_investigation(out_dir: &Path, inv: &Investigation) {
    let path = out_dir.join(INVESTIGATION_SNAPSHOT_FILENAME);
    let json = match serde_json::to_string_pretty(inv) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "investigation snapshot: could not serialise");
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        tracing::warn!(error = %e, dir = %out_dir.display(), "investigation snapshot: could not create output directory");
        return;
    }
    match std::fs::write(&path, json) {
        Ok(()) => eprintln!(
            "[trusty-review report] Investigation snapshot: {}",
            path.display()
        ),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "investigation snapshot: could not write")
        }
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
