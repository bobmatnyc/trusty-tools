//! `trusty-review report` CLI subcommand (M1, #2313).
//!
//! Why: the report subcommand is the single entry point for manifest-driven
//! technical-DD report generation — it loads a manifest, enriches each repo
//! deterministically (git provenance + pre-produced metrics), fills a bundled
//! template, and writes markdown + JSON.  No LLM (M1 is deterministic only).
//! What: defines [`ReportArgs`] (clap-derive) and [`cmd_report`] (the handler).
//! Template precedence is `--template` flag > manifest `[report].template` >
//! the default template.  Progress goes to STDERR; the written paths go to
//! STDOUT for scripting.
//! Test: `tests::report_args_parse_defaults` verifies clap parsing; the render
//! path is covered end to end by `tests/report_e2e.rs`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Parser;

use trusty_review::config::{Provider, ReviewConfig};
use trusty_review::llm::{build_provider, resolve_model, resolve_provider_and_model};
use trusty_review::report::{
    Budget, CorpusSnapshot, Instructions, Reporter, TemplateLoader, benchmark,
    discover_manifest_instructions,
    investigate::{Investigation, Verifier, apply_investigation, merge_investigation_prose},
    load_instructions, load_manifest,
    manifest::Manifest,
    model::{InferenceAttribution, ReportModel, RoleAttribution},
    parse_section_instructions, run_investigation,
    synthesize::Synthesis,
    synthesize::Synthesizer,
    template::DEFAULT_TEMPLATE,
};

// ─── Report args ──────────────────────────────────────────────────────────────

/// Arguments for the `report` subcommand.
///
/// Why: groups the manifest-driven report flags in one testable struct.
/// What: the manifest path (required), an optional template override, and the
/// output directory (default `./reports`).
/// Test: `tests::report_args_parse_defaults`.
#[derive(Debug, Parser)]
pub struct ReportArgs {
    /// Path to the report manifest TOML file (required).
    #[arg(long, value_name = "FILE")]
    pub manifest: PathBuf,

    /// Template name override (e.g. `report-technical-dd-cast`).
    /// Precedence: this flag > manifest `[report].template` > default.
    #[arg(long, value_name = "NAME")]
    pub template: Option<String>,

    /// Path to a free-form analyst instructions markdown file (#2340).  The brief
    /// is recorded verbatim in the report and, under `--synthesize`, injected as
    /// focus directives.  Precedence: this flag > manifest `[report].instructions`.
    /// A missing file is an error; an empty file is a warning (proceeds as absent).
    #[arg(long, value_name = "FILE")]
    pub instructions: Option<PathBuf>,

    /// Output directory for the generated report pair (`{slug}.md`/`.json`).
    #[arg(long, value_name = "DIR", default_value = "./reports")]
    pub out: PathBuf,

    /// Deprecated and ignored (#5454): synthesis is now unconditional.
    ///
    /// It is still accepted so that scripts, the `tga audit` invocation, and the
    /// recovery command printed on a failed render keep parsing against both the
    /// old and the new binary — `tga` resolves `trusty-review` from PATH, so the
    /// two versions are not released together and either may be installed.
    /// Passing it prints one deprecation line to stderr and changes nothing.
    #[arg(long)]
    pub synthesize: bool,

    /// Wave-3 investigation budget: max files sent per repository (#2357).
    /// Precedence: this flag > manifest `[report].investigate_max_files` > default.
    #[arg(long, value_name = "N")]
    pub investigate_max_files: Option<usize>,

    /// Wave-3 investigation budget: max total content bytes sent per repository
    /// (#2357).  Precedence: this flag > manifest `[report].investigate_max_bytes`
    /// > default.
    #[arg(long, value_name = "BYTES")]
    pub investigate_max_bytes: Option<usize>,

    /// Benchmark corpus directory (M3).  Overrides the manifest `[report].corpus`
    /// key and the per-user XDG default (`~/.local/share/trusty-review/benchmark/`).
    #[arg(long, value_name = "DIR")]
    pub corpus: Option<PathBuf>,

    /// After a successful run, append each analyzed repository's metrics snapshot
    /// to the corpus (one `<slug>-<sha-or-date>.json` per repo; overwrites the
    /// same key).  Repos without metrics contribute nothing.
    #[arg(long)]
    pub corpus_add: bool,

    /// Compute cross-repo percentile/quartile placement against the corpus and
    /// fill the benchmark tables.  Requires a resolvable corpus directory; a
    /// corpus with fewer than five peers is disclosed, never silently ranked.
    #[arg(long)]
    pub benchmark: bool,

    /// Disable Mermaid chart rendering under the Graph-Ready Data Appendix tables
    /// (#2366).  Charts are ON by default; this flag OR the manifest key
    /// `[report] mermaid = false` disables them, yielding byte-identical
    /// pre-wave-4 output.  Precedence: this flag forces off regardless of the
    /// manifest key.
    #[arg(long)]
    pub no_mermaid: bool,

    /// Populate the complexity-distribution chart and RED/AMBER finding bands
    /// deterministically from the trusty-analyze daemon (epic #2445).  OFF by
    /// default: a bare run stays scan-only.  Fills metrics ONLY for local-path
    /// repositories that declare no `metrics` file (declared metrics always win),
    /// and only when the repo is already indexed in trusty-search/trusty-analyze.
    /// Fully fail-open — an unindexed repo or an unreachable daemon logs a
    /// warning and falls through to the built-in scan; it never aborts the report.
    /// Daemon URL precedence: manifest `[report].analyze_url` > env
    /// `PR_INTELLIGENCE_ANALYZER_URL` > default `http://127.0.0.1:7879`.
    #[arg(long)]
    pub analyze: bool,
}

// ─── Command handler ──────────────────────────────────────────────────────────

/// Execute the `report` subcommand.
///
/// Why: orchestrates the deterministic report pipeline from a single CLI call so
/// the binary is a thin wrapper over the library.
/// What: loads + validates the manifest, resolves the template name by
/// precedence, loads the template source, builds the enriched model, and writes
/// the markdown + JSON pair.  Progress → STDERR; written paths → STDOUT.
/// Test: arg parsing via `tests::report_args_parse_defaults`; full render via
/// `tests/report_e2e.rs`.
pub async fn cmd_report(config_path: Option<&Path>, args: ReportArgs) -> Result<()> {
    if args.synthesize {
        eprintln!(
            "[trusty-review report] --synthesize is deprecated and ignored: synthesis is always on"
        );
    }

    // #6135: the manifest is read BEFORE the config, because it is a config
    // layer — it carries the provider and per-role models of the run that
    // produced it, and those outrank this host's `config.toml`. Reading one
    // small TOML file keeps #5454's promise intact: the credential is still
    // checked before a single repository is walked.
    eprintln!(
        "[trusty-review report] Loading manifest: {}",
        args.manifest.display()
    );
    let manifest = load_manifest(&args.manifest)
        .with_context(|| format!("failed to load manifest {}", args.manifest.display()))?;
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

    // Template precedence: CLI flag > manifest [report].template > default.
    let template_name = args
        .template
        .clone()
        .or_else(|| manifest.report.template.clone())
        .unwrap_or_else(|| DEFAULT_TEMPLATE.to_string());
    eprintln!(
        "[trusty-review report] Template: {template_name}; repositories: {}",
        manifest.repositories.len()
    );

    let template = TemplateLoader::new()
        .load(&template_name)
        .with_context(|| format!("failed to load template '{template_name}'"))?;

    // Analyst instructions (#2340): CLI flag wins over the manifest key; a
    // relative manifest path resolves against the manifest directory.
    let instructions = load_report_instructions(&args, &manifest)?;
    if let Some(instr) = &instructions {
        eprintln!(
            "[trusty-review report] Analyst instructions: {}",
            instr.source.display()
        );
    }

    let mut model = ReportModel::build(
        &manifest,
        &args.manifest,
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
    if args.analyze {
        let analyze_url = manifest
            .report
            .analyze_url
            .clone()
            .unwrap_or_else(|| config.analyzer_url.clone());
        eprintln!("[trusty-review report] --analyze: fetching from {analyze_url}");
        match trusty_review::report::HttpAnalyzeMetricsSource::new(analyze_url) {
            Ok(source) => {
                // #5239: every repo the fetch could not populate is named in the
                // report, not only warned about on stderr — a dimension missing
                // because the daemon was down must not read as a clean pass.
                let gaps =
                    trusty_review::report::enrich_with_analyze_gaps(&mut model, &source).await;
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
                // #5239: the client never existed, so no repo was assessed —
                // that is a whole-report gap, not a per-repo one.
                model.gaps.push(
                    "trusty-analyze data unavailable — the analysis client could not be \
                     built, so no application in this report was assessed against \
                     trusty-analyze. Findings, complexity, and health factors are not \
                     assessed, not clean."
                        .to_string(),
                );
            }
        }
    }

    // LLM synthesis plus the repo-evidence investigation. #5454: required, so a
    // failure here ends the run — no report is written and the manifest the
    // caller passed in is untouched, which is what makes re-running this exact
    // command the recovery path.
    eprintln!("[trusty-review report] Synthesis — calling LLM provider...");
    let budget = resolve_budget(&args, &manifest);
    // #6082: manifest-only, with no CLI flag — the producer that reached a
    // healthy search index is the only party that knows whether the declared
    // list is the whole intended sample.
    let attributed_only = manifest.report.attributed_only.unwrap_or(false);
    let synthesis = run_synthesis(&config, &mut model, budget, attributed_only, &args.out)
        .await
        .with_context(|| {
            format!(
                "inference is required for a due-diligence report, and this run produced none. \
                 Nothing collected is lost — re-run `trusty-review report --manifest {} --analyze \
                 --out {}` once the cause is addressed",
                args.manifest.display(),
                args.out.display()
            )
        })?;
    eprintln!(
        "[trusty-review report] {}",
        synthesis
            .status_lines()
            .first()
            .cloned()
            .unwrap_or_default()
    );
    model.synthesis = Some(synthesis);

    // M3: resolve the corpus directory once (shared by --benchmark and
    // --corpus-add).  A `--corpus` flag with neither consumer is a no-op — warn.
    let corpus_dir = if args.benchmark || args.corpus_add {
        Some(resolve_corpus_dir(&args, &manifest)?)
    } else {
        if args.corpus.is_some() {
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
    if args.benchmark {
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
    let mermaid = manifest.report.mermaid.unwrap_or(true) && !args.no_mermaid;
    let reporter = Reporter::new(&args.out).with_mermaid(mermaid);
    let written = reporter
        .write(&model, &template)
        .context("failed to write report output")?;

    // M3: persist snapshots only after a successful write.
    if args.corpus_add {
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
    for path in &written {
        // Paths to STDOUT so `$(trusty-review report ...)` is scriptable.
        println!("{}", path.display());
    }

    Ok(())
}

/// Resolve the benchmark corpus directory or fail with a clear message.
///
/// Why: `--benchmark` / `--corpus-add` both require a corpus directory; the
/// resolution precedence (CLI flag > manifest `[report].corpus` > XDG default)
/// lives in the library, and the only failure is a platform with no data dir and
/// no explicit source — which must be a clear CLI error, not a panic.
/// What: delegates to [`benchmark::corpus_dir`] with the manifest directory as
/// the base for a relative manifest key; maps `None` to an actionable error.
/// Test: covered by `tests::report_args_parse_benchmark` (parse) and the corpus
/// resolver unit tests in `benchmark_tests.rs`.
fn resolve_corpus_dir(args: &ReportArgs, manifest: &Manifest) -> Result<PathBuf> {
    let manifest_dir = args.manifest.parent().unwrap_or_else(|| Path::new("."));
    benchmark::corpus_dir(
        args.corpus.as_deref(),
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
/// Why: the brief may come from the `--instructions` flag, the manifest
/// `[report].instructions` key, or — since #6180 — an `instructions.md` the
/// engagement author dropped beside the manifest with nothing declaring it. The
/// flag wins, then the key, then discovery; a relative key is resolved against
/// the manifest directory so authors write portable paths.
/// What: returns `Ok(None)` when no source is configured AND no file was
/// discovered; otherwise loads via [`load_instructions`] (missing file → error;
/// empty file → warn + `None`) or [`discover_manifest_instructions`] (absent →
/// `None`; present but unreadable → error).
/// Test: instructions loading/validation is covered by `instructions_tests.rs`;
/// precedence is exercised by `tests::report_args_parse_defaults` shape.
fn load_report_instructions(
    args: &ReportArgs,
    manifest: &Manifest,
) -> Result<Option<Instructions>> {
    let manifest_dir = args.manifest.parent().unwrap_or_else(|| Path::new("."));
    let resolved: Option<PathBuf> = match &args.instructions {
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
        // #6180: nothing was declared, so look for the file that travels with the
        // engagement. Absent is the normal case and changes nothing.
        None => Ok(discover_manifest_instructions(&args.manifest)?),
    }
}

/// Build one corpus snapshot per analyzed repository that has metrics.
///
/// Why: both benchmarking (the target population) and `--corpus-add` (persisted
/// records) need the same identity+metrics snapshots derived from this run; the
/// run timestamp is the model's generated date (kept out of the core for
/// testability).
/// What: maps each repository with metrics to a [`CorpusSnapshot`]; metric-less
/// repos are skipped.
/// Test: exercised end to end by `tests/report_e2e.rs`.
fn target_snapshots(model: &ReportModel) -> Vec<CorpusSnapshot> {
    model
        .repositories
        .iter()
        .filter_map(|r| CorpusSnapshot::from_repository(r, &model.generated_date))
        .collect()
}

/// Resolve the investigation file/byte budget by precedence (#2357).
///
/// Why: the wave-3 investigation caps how much of a repo is sent to the LLM;
/// operators tune it via a CLI flag or a manifest key, falling back to sane
/// defaults, in one place so both budget dimensions resolve consistently.
///
/// #6082 adds the environment tier, BELOW both. `trusty-audit` writes the budget
/// it wants into `[report]`, but on the sweep path that write lands after the
/// `tga audit` child has already run this binary — so the key was in the shipped
/// manifest and never in the run that produced the report, and the investigation
/// silently used the 40-file default over a manifest declaring 240. The
/// environment crosses both process boundaries before the file does. It is the
/// lowest tier because an operator's flag and an operator's manifest key are
/// both explicit and this is not.
///
/// What: CLI flag > manifest `[report].investigate_max_*` >
/// `TRUSTY_AUDIT_INVESTIGATE_MAX_*` > [`Budget::default`]. A non-numeric, zero
/// or negative variable reads as absent rather than disabling the
/// investigation.
/// Test: `tests::{report_args_parse_investigate_budget,
/// the_environment_budget_is_read_below_the_manifest}`.
fn resolve_budget(args: &ReportArgs, manifest: &Manifest) -> Budget {
    resolve_budget_from(
        args,
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
/// Test: `tests::the_environment_budget_is_read_below_the_manifest`.
fn resolve_budget_from(
    args: &ReportArgs,
    manifest: &Manifest,
    env_files: Option<usize>,
    env_bytes: Option<usize>,
) -> Budget {
    let default = Budget::default();
    Budget {
        max_files: args
            .investigate_max_files
            .or(manifest.report.investigate_max_files)
            .or(env_files)
            .unwrap_or(default.max_files),
        max_bytes: args
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
/// runs for minutes before it reaches this binary. #5454 made inference required,
/// so the one failure that IS knowable up front — no credential — must be raised
/// up front rather than after a full render's worth of work.
/// What: OpenRouter is the only provider this preflights, per the #5454 owner
/// decision that it is the only inference path for a DD report. A reviewer role
/// resolved to Bedrock or Fireworks is left to fail at the provider-build site,
/// which is also fatal now — this check narrows the window, it does not open a
/// second path. The key's VALUE is never read, printed, or compared here; only
/// whether it is blank.
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
/// What: one [`ModelResolution`] per role, folded into the record the page and
/// the JSON twin carry. `source` names the layer the selection came from, which
/// is the fact that distinguishes a portable render from one that inherited the
/// host's config.
///
/// # Errors
///
/// Only when a role's id names a provider this build cannot call and has no
/// verified equivalent — see [`trusty_review::llm::resolve_model`].
///
/// Test: `tests::attribution_names_every_role`,
/// `tests::attribution_shows_a_translated_id_as_requested_then_ran`.
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
/// Why: taking the resolved provider and the key as parameters keeps the decision
/// testable without loading a `ReviewConfig` — which reads the real process
/// environment, so a test of it would pass or fail depending on whether the
/// machine running it happens to have a key exported.
/// What: `Err` only for OpenRouter with a blank key. The key is inspected for
/// emptiness and never copied into the message.
/// Test: `tests::{preflight_rejects_a_blank_openrouter_key,
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

/// Build the LLM provider, run the repo-evidence investigation, then synthesis.
///
/// Why: keeps `cmd_report` readable and isolates the provider-build failure path.
/// #5454 turned that path fatal: a build error (a key rejected at construction, a
/// bad model id) used to degrade the report to deterministic output, which is the
/// mode the owner decision removed. The investigation (#2357) runs FIRST so its
/// verified findings are injected into the model before synthesis, and synthesis
/// sees the coverage summary; its verified (measured-evidence) prose then wins
/// over any synthesis prose for the same finding.
/// What: resolves the reviewer role's provider/model (the same construction path
/// the review pipeline uses), then runs `run_investigation` (local repos only),
/// `apply_investigation`, `synthesize`, and `merge_investigation_prose`.
///
/// # Errors
///
/// When the provider cannot be built, or when the synthesis pass produces no
/// verified prose ([`trusty_review::report::SynthesisError`]).
///
/// Test: the build path is network-bound; the failure decisions are covered by
/// `report::synthesize::tests` and `tests/report_investigate.rs` with stubs.
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

    // Wave-3 investigation (local checkouts only): select → LLM → verify, then
    // inject verified findings so synthesis and the reporter render them.
    // #6082: only the manifest declares this — there is no CLI flag, because the
    // producer that reached a healthy index is the only party that knows whether
    // the declared list is the whole intended sample.
    // #6166 leg 2: the verdict pass runs on the VERIFIER role, built through the
    // same `build_provider` path as the reviewer. A build failure is not fatal
    // here (unlike the reviewer's): the pass fails closed, recording every traced
    // finding as unverifiable, which is a stated gap rather than a lost render.
    let verifier = build_verdict_verifier(config).await;
    if let Some(inv) = run_investigation(
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
        apply_investigation(model, &inv);
        // #6009: capture the raw response next to the report output on an
        // unparseable-response failure, so a future occurrence is diagnosable
        // without spending another live call to find out what the model sent.
        let mut synthesis = Synthesizer::new(provider, role.model.clone())
            .with_max_tokens(role.max_tokens)
            .with_raw_capture_dir(out_dir)
            .synthesize(model)
            .await?;
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
const INVESTIGATION_SNAPSHOT_FILENAME: &str = "investigation.json";

/// Write the verified investigation next to the report output before synthesis
/// runs (#6093).
///
/// Why: the investigation is the expensive half of a report run — several
/// minutes of selection, LLM calls, and mechanical evidence verification. A
/// synthesis failure used to throw all of it away, so recovering meant paying
/// for the whole investigation again. The snapshot lands before the first
/// synthesis call, so it survives every failure downstream of it.
/// What: serialises [`Investigation`] to
/// `<out_dir>/`[`INVESTIGATION_SNAPSHOT_FILENAME`], creating the directory if
/// needed, and prints the path. Any failure is a warning, never an error: a
/// recovery aid must not itself abort a run that would otherwise succeed.
/// Test: `tests::investigation_snapshot_is_written_and_reloadable`,
/// `tests::investigation_snapshot_failure_is_not_fatal`.
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
mod tests {
    use super::*;

    /// Why: clap parsing of the report flags must be stable.
    /// What: parses a minimal invocation and asserts defaults + overrides.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_defaults() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--template",
            "report-technical-dd-cast",
        ])
        .expect("parse");
        assert_eq!(args.manifest, PathBuf::from("m.toml"));
        assert_eq!(args.template.as_deref(), Some("report-technical-dd-cast"));
        assert_eq!(args.out, PathBuf::from("./reports"));
        // M3 flags default off / unset.
        assert!(!args.benchmark);
        assert!(!args.corpus_add);
        assert!(args.corpus.is_none());
        // Epic #2445: --analyze defaults off.
        assert!(!args.analyze);
    }

    /// Why: the epic #2445 `--analyze` flag must parse.
    /// What: parses `--analyze` and asserts the boolean is set.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_analyze() {
        let args = ReportArgs::try_parse_from(["report", "--manifest", "m.toml", "--analyze"])
            .expect("parse");
        assert!(args.analyze);
    }

    /// Why: the M3 corpus/benchmark flags must parse and resolve a corpus dir.
    /// What: parses `--corpus`, `--corpus-add`, `--benchmark` and asserts the
    /// resolver honours the explicit `--corpus` directory (CLI precedence).
    /// Test: this test itself.
    #[test]
    fn report_args_parse_benchmark() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--corpus",
            "/tmp/corpus",
            "--corpus-add",
            "--benchmark",
        ])
        .expect("parse");
        assert!(args.benchmark);
        assert!(args.corpus_add);
        assert_eq!(args.corpus.as_deref(), Some(Path::new("/tmp/corpus")));

        // The resolver prefers the explicit --corpus directory.
        let manifest = load_manifest_from_str(
            "[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        );
        let dir = resolve_corpus_dir(&args, &manifest).expect("resolve");
        assert_eq!(dir, PathBuf::from("/tmp/corpus"));
    }

    /// Why: the wave-3 investigation budget resolves by CLI > manifest > default.
    /// What: a CLI `--investigate-max-files` overrides the manifest key; the byte
    /// cap falls back to the manifest; an unset dimension uses the default.
    /// Test: this test itself.
    #[test]
    fn report_args_parse_investigate_budget() {
        let args = ReportArgs::try_parse_from([
            "report",
            "--manifest",
            "m.toml",
            "--synthesize",
            "--investigate-max-files",
            "7",
        ])
        .expect("parse");
        assert_eq!(args.investigate_max_files, Some(7));
        assert!(args.investigate_max_bytes.is_none());

        let manifest = load_manifest_from_str(
            "[report]\ntitle = \"T\"\ninvestigate_max_files = 3\ninvestigate_max_bytes = 1024\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        );
        let budget = resolve_budget(&args, &manifest);
        assert_eq!(budget.max_files, 7, "CLI flag wins over the manifest key");
        assert_eq!(
            budget.max_bytes, 1024,
            "manifest key fills the unset CLI flag"
        );
    }

    /// #6082: the environment is the tier below the manifest, and it is what
    /// carries an audit's budget across the two process boundaries in time.
    ///
    /// Why this is the regression: `trusty-audit` records the budget in
    /// `[report]`, but on the sweep path `tga audit` has already run this binary
    /// against that manifest by the time the key is written — so the shipped
    /// manifest declared `investigate_max_files = 240` while the investigation
    /// that produced the report recorded `{"max_files": 40}`, this crate's bare
    /// default. Against the pre-fix `resolve_budget`, which had no environment
    /// tier at all, the first assertion below reads 40 and fails.
    #[test]
    fn the_environment_budget_is_read_below_the_manifest() {
        let args = ReportArgs::try_parse_from(["report", "--manifest", "m.toml", "--synthesize"])
            .expect("parse");
        let bare = load_manifest_from_str(
            "[report]\ntitle = \"T\"\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        );

        let from_env = resolve_budget_from(&args, &bare, Some(240), Some(2_457_600));
        assert_eq!(from_env.max_files, 240, "the audit's budget arrives");
        assert_eq!(from_env.max_bytes, 2_457_600);

        let declared = load_manifest_from_str(
            "[report]\ntitle = \"T\"\ninvestigate_max_files = 3\n\n[[repositories]]\nname = \"A\"\npath = \"/x\"\n",
        );
        let mixed = resolve_budget_from(&args, &declared, Some(240), Some(2_457_600));
        assert_eq!(mixed.max_files, 3, "an operator's manifest key still wins");
        assert_eq!(
            mixed.max_bytes, 2_457_600,
            "and the environment fills only what the manifest left unset"
        );

        let none = resolve_budget_from(&args, &bare, None, None);
        assert_eq!(
            none.max_files,
            Budget::default().max_files,
            "no manifest key and no variable is still the default"
        );
    }

    /// Parse a manifest from an in-memory string for CLI validation tests.
    fn load_manifest_from_str(toml: &str) -> Manifest {
        trusty_review::report::manifest::parse_manifest(toml, Path::new("m.toml"))
            .expect("manifest parses")
    }

    // ── #5454: the credential preflight ──────────────────────────────────────

    /// Why: #5454 — this is the failure that is knowable BEFORE a multi-minute
    /// sweep, and letting it surface at the end (as the provider-build error it
    /// used to be) wastes the whole one-shot run DOC-67 allows.
    /// What: a blank OpenRouter key is rejected, and the message names the
    /// variable and how to set it.
    /// Test: this test itself.
    #[test]
    fn preflight_rejects_a_blank_openrouter_key() {
        for blank in ["", "   ", "\n"] {
            let err = credential_rule(Provider::OpenRouter, blank)
                .expect_err("a blank key must not pass the preflight");
            let msg = format!("{err}");
            assert!(
                msg.contains("OPENROUTER_API_KEY") && msg.contains("export OPENROUTER_API_KEY="),
                "the message must name the variable and how to set it: {msg}"
            );
            // #6135: the provider may have come from the manifest rather than
            // from anything on this machine, so the message names it.
            assert!(
                msg.contains("openrouter"),
                "the message must name the provider that asked for the key: {msg}"
            );
        }
    }

    /// Why: #6135 — the report states which models ran, and that record starts
    /// here. All three roles are resolved, not only the reviewer the synthesis
    /// pass builds, so the manifest's whole declared selection is attributed.
    /// What: resolves against a config whose role models are the built-in
    /// OpenRouter defaults, and asserts one row per role.
    /// Test: this test itself.
    #[test]
    fn attribution_names_every_role() {
        let manifest = trusty_review::config::RoleManifest {
            reviewer_model: Some("anthropic/claude-opus-4.8".to_string()),
            verifier_model: Some("anthropic/claude-haiku-4.5".to_string()),
            summarizer_model: Some("anthropic/claude-haiku-4.5".to_string()),
            provider: Some("openrouter".to_string()),
        };
        let config = ReviewConfig::from_env_and_manifest(None, None, Some(&manifest));
        let record = resolve_attribution(&config, "the manifest's [inference] section")
            .expect("the declared selection resolves");

        assert_eq!(record.provider, "openrouter");
        assert_eq!(
            record
                .roles
                .iter()
                .map(|r| r.role.as_str())
                .collect::<Vec<_>>(),
            vec!["reviewer", "verifier", "summarizer"]
        );
        assert!(
            record.roles.iter().all(|r| r.requested == r.ran),
            "nothing was adjusted here: {:?}",
            record.roles
        );
        assert_eq!(record.roles[0].ran, "anthropic/claude-opus-4.8");
    }

    /// Why: the resolver adjusting an id must never be invisible — that is the
    /// whole anti-silent-wrong-model guarantee once refusal is gone.
    /// What: a manifest whose reviewer id is pinned to Bedrock but spelled for
    /// OpenRouter, resolved and attributed.
    /// Test: this test itself.
    #[test]
    fn attribution_shows_a_translated_id_as_requested_then_ran() {
        let manifest = trusty_review::config::RoleManifest {
            reviewer_model: Some("bedrock/anthropic/claude-sonnet-4.6".to_string()),
            provider: Some("bedrock".to_string()),
            ..Default::default()
        };
        let config = ReviewConfig::from_env_and_manifest(None, None, Some(&manifest));
        let record =
            resolve_attribution(&config, "the manifest's [inference] section").expect("resolves");

        let reviewer = &record.roles[0];
        assert_eq!(reviewer.requested, "bedrock/anthropic/claude-sonnet-4.6");
        assert_eq!(reviewer.ran, "us.anthropic.claude-sonnet-4-6");
        assert!(reviewer.note.is_some(), "the adjustment must be recorded");
        assert!(
            record.line().contains(" → "),
            "the page shows both halves: {}",
            record.line()
        );
    }

    /// Why: the preflight must not stand between an operator with a key and a run.
    /// What: any non-blank key passes.
    /// Test: this test itself.
    #[test]
    fn preflight_accepts_a_present_openrouter_key() {
        credential_rule(Provider::OpenRouter, "sk-or-v1-example")
            .expect("a present key passes the preflight");
    }

    /// Why: OpenRouter is the only path #5454 preflights; Bedrock resolves its
    /// credentials through the AWS chain and Fireworks through its own key, so
    /// neither can be judged from `openrouter_api_key`. They stay the
    /// provider-build site's business — which is fatal now too.
    /// What: a non-OpenRouter provider passes even with no OpenRouter key.
    /// Test: this test itself.
    #[test]
    fn preflight_leaves_non_openrouter_providers_to_the_build_site() {
        credential_rule(Provider::Bedrock, "").expect("Bedrock is not preflighted here");
        credential_rule(Provider::Fireworks, "").expect("Fireworks is not preflighted here");
    }

    /// Why: an operator's terminal, their shell history, and any log scraping it
    /// are all places a key must never appear.
    /// What: a run with a real-looking key produces no error at all; a run with a
    /// blank one produces a message containing no key material.
    /// Test: this test itself.
    #[test]
    fn preflight_message_never_echoes_the_key() {
        let secret = "sk-or-v1-DEADBEEFdeadbeef";
        // The accepting path emits nothing.
        credential_rule(Provider::OpenRouter, secret).expect("present key passes");
        // The rejecting path has no key to echo, and must not invent one.
        let msg = format!(
            "{}",
            credential_rule(Provider::OpenRouter, "").expect_err("blank key fails")
        );
        assert!(!msg.contains(secret), "no key material may appear: {msg}");
        assert!(
            !msg.contains("sk-or"),
            "no key-shaped text may appear: {msg}"
        );
    }

    /// A one-repo investigation with a single verified finding.
    fn snapshot_fixture() -> Investigation {
        use trusty_review::report::investigate::{
            InvestigationStatus, RepoInvestigation, VerifiedFinding,
        };
        Investigation {
            repos: vec![RepoInvestigation {
                verdicts: None,
                slug: "acme-core".to_string(),
                name: "Acme Core".to_string(),
                status: InvestigationStatus::Available,
                findings: vec![VerifiedFinding {
                    trace_verdict: String::new(),
                    title: "Hardcoded credential".to_string(),
                    severity: trusty_review::report::metrics::Severity::Red,
                    dimension: "security".to_string(),
                    file: "src/auth.rs".to_string(),
                    line: Some(12),
                    evidence_quote: "let api_key = \"…\";".to_string(),
                    description: "A credential is committed in source.".to_string(),
                    business_impact: String::new(),
                    remediation: "Move it to the secret store.".to_string(),
                    cost_effort: String::new(),
                }],
                deps: Default::default(),
                traces: None,
                coverage: Default::default(),
            }],
        }
    }

    /// Why: #6093 — a synthesis failure used to discard the whole investigation,
    /// the expensive half of a report run. The snapshot must land before the
    /// first synthesis call and must be readable afterwards.
    /// What: writes a fixture investigation to a temp dir; asserts the file
    /// exists at the documented name and that its JSON still carries the
    /// verified finding's title and file.
    /// Test: this test itself.
    #[test]
    fn investigation_snapshot_is_written_and_reloadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        persist_investigation(dir.path(), &snapshot_fixture());

        let path = dir.path().join(INVESTIGATION_SNAPSHOT_FILENAME);
        let raw = std::fs::read_to_string(&path).expect("the snapshot must exist");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(parsed["repos"][0]["slug"], "acme-core");
        assert_eq!(
            parsed["repos"][0]["findings"][0]["title"],
            "Hardcoded credential"
        );
        assert_eq!(parsed["repos"][0]["findings"][0]["file"], "src/auth.rs");
    }

    /// Why: a recovery aid must never turn a run that would have succeeded into
    /// a failure of its own.
    /// What: points the snapshot at a path that cannot be a directory (an
    /// existing file); asserts the call returns normally rather than panicking
    /// or propagating.
    /// Test: this test itself.
    #[test]
    fn investigation_snapshot_failure_is_not_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocked = dir.path().join("not-a-dir");
        std::fs::write(&blocked, b"x").expect("write blocker");
        persist_investigation(&blocked, &snapshot_fixture());
        assert!(blocked.is_file(), "the blocking file is untouched");
    }
}
