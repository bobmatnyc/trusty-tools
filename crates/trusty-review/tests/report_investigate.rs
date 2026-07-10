//! End-to-end investigation tests with mocked providers (wave 3, #2357 + #2357
//! follow-up "batch the investigation" hardening).
//!
//! Why: the whole point of the investigation pass is that when the repository is
//! readable the tool inspects it and produces evidence-backed findings — and that
//! an unverifiable finding is REJECTED, not softened. A live-QA incident then
//! showed that a single unbatched request could hit the output-token ceiling and
//! discard EVERY finding for a fully readable repo; these tests drive the real
//! pipeline (select → batch → mock LLM → verify → inject → merge → render)
//! against fixture repos and assert both behaviours, with no network.
//! What: (1) a fixture checkout with a hardcoded secret and a fabricated
//! evidence claim, asserting the verifiable one renders measured and the
//! fabricated one is dropped + noted; (2) a fixture large enough to force
//! multiple investigation batches, with a scripted provider that truncates one
//! batch — asserting the OTHER batch's finding still renders and the failed
//! batch is named in both the report and the synthesis-prompt coverage digest.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use trusty_review::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use trusty_review::report::investigate::{apply_investigation, merge_investigation_prose};
use trusty_review::report::provenance::{INFERRED_TAG, MEASURED_TAG};
use trusty_review::report::synthesize::Synthesis;
use trusty_review::report::{
    Budget, Reporter, TemplateLoader, manifest::parse_manifest, model::ReportModel,
    run_investigation,
};

/// A provider that returns one verifiable + one fabricated finding.
struct MockLlm;

#[async_trait]
impl LlmProvider for MockLlm {
    fn name(&self) -> &str {
        "mock"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = r#"{
          "findings": [
            {"title": "Hardcoded API key", "severity": "red",
             "dimension": "authentication & secrets", "file": "src/auth.rs", "line": 99,
             "evidence_quote": "let api_key = \"sk-hardcoded-secret-123\";",
             "description": "A live credential is committed to source.",
             "business_impact": "Credential compromise.",
             "remediation": "Load from the environment.", "cost_effort": "low"},
            {"title": "Phantom eval sink", "severity": "amber",
             "dimension": "error handling", "file": "src/auth.rs", "line": 3,
             "evidence_quote": "eval(untrusted_input_that_is_not_in_the_file)",
             "description": "x", "business_impact": "y", "remediation": "z", "cost_effort": "low"}
          ]
        }"#;
        Ok(LlmResponse {
            text: body.to_string(),
            model: "mock".to_string(),
            input_tokens: 100,
            output_tokens: 80,
            latency_ms: 5,
            cost_usd: 0.0,
            finish_reason: Some("stop".to_string()),
        })
    }
}

/// Build a fixture checkout with a hardcoded secret + a Cargo manifest.
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    let r = tmp.path();
    std::fs::create_dir_all(r.join("src")).unwrap();
    std::fs::write(
        r.join("src/auth.rs"),
        "pub fn authenticate(user: &str) -> bool {\n    let api_key = \"sk-hardcoded-secret-123\";\n    let _ = api_key;\n    user == \"admin\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        r.join("Cargo.toml"),
        "[package]\nname = \"acme\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1.0\"\n",
    )
    .unwrap();
    tmp
}

#[tokio::test]
async fn investigation_renders_verified_and_rejects_unverifiable() {
    let fx = fixture();
    let repo_path = fx.path().to_string_lossy().replace('\\', "/");
    let toml = format!(
        "[report]\ntitle = \"Acme DD\"\n\n[[repositories]]\nname = \"Acme\"\nslug = \"acme\"\npath = \"{repo_path}\"\n",
    );
    let manifest = parse_manifest(&toml, Path::new("m.toml")).expect("manifest");
    let template_name = "report-technical-dd";
    let template = TemplateLoader::bundled_only()
        .load(template_name)
        .expect("template");
    let mut model =
        ReportModel::build(&manifest, Path::new("m.toml"), template_name, None).expect("model");

    // The repo must have been recognised as a local checkout.
    assert!(model.repositories[0].local_path.is_some());

    let provider: Arc<dyn LlmProvider> = Arc::new(MockLlm);
    let inv = run_investigation(provider, "mock/model", &model, Budget::default())
        .await
        .expect("investigation ran (local repo present)");

    // One finding verified, one rejected.
    let repo_inv = &inv.repos[0];
    assert_eq!(
        repo_inv.findings.len(),
        1,
        "only the verifiable finding survives"
    );
    assert_eq!(repo_inv.findings[0].title, "Hardcoded API key");
    assert_eq!(
        repo_inv.coverage.rejected, 1,
        "the fabricated finding is rejected"
    );
    // Line was corrected from the real match (the secret is on line 2, not 99).
    assert_eq!(repo_inv.findings[0].line, Some(2));
    // Deterministic dependency inventory picked up serde.
    assert!(repo_inv.deps.deps.iter().any(|d| d.name == "serde"));

    apply_investigation(&mut model, &inv);
    let mut synthesis = Synthesis::unavailable("narrative pass skipped in test");
    merge_investigation_prose(&mut synthesis, &inv);
    model.synthesis = Some(synthesis);

    let md = Reporter::new(fx.path()).render(&model, &template);

    // The verified finding renders with its measured evidence quote (⁽ᵐ⁾) and
    // inferred prose (⁽ⁱ⁾).
    assert!(
        md.contains("Hardcoded API key"),
        "verified finding title present"
    );
    assert!(
        md.contains(&format!("sk-hardcoded-secret-123\";{MEASURED_TAG}")),
        "evidence quote must render measured; md:\n{md}"
    );
    assert!(
        md.contains(&format!("committed to source{INFERRED_TAG}")),
        "description must render inferred"
    );
    // The fabricated finding never appears.
    assert!(
        !md.contains("Phantom eval"),
        "rejected finding must not render"
    );
    // Coverage + dependency sections are present and honest.
    assert!(md.contains("## Dependency Inventory"));
    assert!(md.contains("| serde | cargo |"));
    assert!(md.contains("## Investigation Coverage"));
    assert!(md.contains("1 finding(s) rejected (unverifiable evidence)"));
    assert!(md.contains("files examined:"));
}

/// A provider that succeeds on its first call (batch 1) and truncates on every
/// subsequent call (batch 2's initial attempt + its one retry) — the regression
/// scenario for the live-QA batch-collapse incident.
struct BatchTruncatingLlm {
    calls: AtomicUsize,
}

#[async_trait]
impl LlmProvider for BatchTruncatingLlm {
    fn name(&self) -> &str {
        "batch-truncating-mock"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            let body = r#"{"findings": [{"title": "Hardcoded API key", "severity": "red",
             "dimension": "authentication & secrets", "file": "src/blob1.rs", "line": 1,
             "evidence_quote": "let api_key = \"sk-batch-e2e-secret\";",
             "description": "A live credential is committed to source.",
             "business_impact": "Credential compromise.",
             "remediation": "Load from the environment.", "cost_effort": "low"}]}"#;
            Ok(LlmResponse {
                text: body.to_string(),
                model: "mock".to_string(),
                input_tokens: 100,
                output_tokens: 80,
                latency_ms: 5,
                cost_usd: 0.0,
                finish_reason: Some("stop".to_string()),
            })
        } else {
            // Every subsequent batch (and its retry) is truncated.
            Ok(LlmResponse {
                text: "{}".to_string(),
                model: "mock".to_string(),
                input_tokens: 100,
                output_tokens: 8192,
                latency_ms: 5,
                cost_usd: 0.0,
                finish_reason: Some("length".to_string()),
            })
        }
    }
}

/// Build a fixture large enough that selection splits into multiple batches:
/// five files each well over the per-file truncation cap (~24 KiB), so their
/// summed content exceeds the 90 KiB per-batch cap and `partition_batches`
/// produces at least two batches.  The hardcoded secret sits at the very start
/// of the first (alphabetically, so relevance-ranking-tied) file, guaranteeing
/// it survives per-file truncation.
fn multi_batch_fixture() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    let r = tmp.path();
    std::fs::create_dir_all(r.join("src")).unwrap();
    let secret_file = format!(
        "let api_key = \"sk-batch-e2e-secret\";\n{}\n",
        "x".repeat(30_000)
    );
    std::fs::write(r.join("src/blob1.rs"), secret_file).unwrap();
    for n in 2..=5 {
        std::fs::write(
            r.join(format!("src/blob{n}.rs")),
            format!("// filler file {n}\n{}\n", "y".repeat(30_000)),
        )
        .unwrap();
    }
    tmp
}

/// Why: this is the direct regression test for the live-QA incident — the fix
/// must be observable through the FULL pipeline (`run_investigation` →
/// `apply_investigation` → `merge_investigation_prose` → render), not just at
/// the unit level.
/// What: a 5-file fixture forces ≥2 investigation batches; the first batch's
/// finding is verified and merged, the second (and every later) batch truncates
/// and is recorded as a named coverage gap.  Asserts: (1) more than one batch
/// ran, (2) the successful batch's finding renders in the report with measured
/// evidence, (3) the coverage section names the failed batch instead of
/// silently reporting fewer findings, and (4) the synthesis-prompt coverage
/// digest carries the same named gap.
/// Test: this test itself.
#[tokio::test]
async fn investigation_survives_one_truncated_batch_and_names_it() {
    let fx = multi_batch_fixture();
    let repo_path = fx.path().to_string_lossy().replace('\\', "/");
    let toml = format!(
        "[report]\ntitle = \"Batch DD\"\n\n[[repositories]]\nname = \"Acme\"\nslug = \"acme\"\npath = \"{repo_path}\"\n",
    );
    let manifest = parse_manifest(&toml, Path::new("m.toml")).expect("manifest");
    let template_name = "report-technical-dd";
    let template = TemplateLoader::bundled_only()
        .load(template_name)
        .expect("template");
    let mut model =
        ReportModel::build(&manifest, Path::new("m.toml"), template_name, None).expect("model");

    let provider: Arc<dyn LlmProvider> = Arc::new(BatchTruncatingLlm {
        calls: AtomicUsize::new(0),
    });
    let inv = run_investigation(provider, "mock/model", &model, Budget::default())
        .await
        .expect("investigation ran (local repo present)");

    let repo_inv = &inv.repos[0];
    assert!(
        repo_inv.coverage.batches_total >= 2,
        "the 5-file fixture must split into multiple batches, got {}",
        repo_inv.coverage.batches_total
    );
    assert!(
        !repo_inv.coverage.batches_failed.is_empty(),
        "at least one later batch must be recorded as truncated"
    );
    assert_eq!(
        repo_inv.findings.len(),
        1,
        "the successful batch's finding survives despite the other batch's failure"
    );
    assert_eq!(repo_inv.findings[0].title, "Hardcoded API key");

    // The synthesis-prompt digest names the failed batch (never a bare gap claim).
    let coverage_prompt = inv.coverage_prompt_summary();
    assert!(coverage_prompt.contains("truncated/failed"));

    apply_investigation(&mut model, &inv);
    let mut synthesis = Synthesis::unavailable("narrative pass skipped in test");
    merge_investigation_prose(&mut synthesis, &inv);
    model.synthesis = Some(synthesis);

    let md = Reporter::new(fx.path()).render(&model, &template);
    assert!(md.contains("Hardcoded API key"));
    assert!(
        md.contains(&format!("sk-batch-e2e-secret\";{MEASURED_TAG}")),
        "evidence quote must render measured; md:\n{md}"
    );
    assert!(md.contains("## Investigation Coverage"));
    assert!(
        md.contains("truncated/failed"),
        "the failed batch must be named in the report, not silently absorbed; md:\n{md}"
    );
}

#[tokio::test]
async fn investigation_returns_none_for_remote_only() {
    let toml = "[report]\ntitle = \"Remote DD\"\n\n[[repositories]]\nname = \"R\"\nremote = \"acme/repo\"\n";
    let manifest = parse_manifest(toml, Path::new("m.toml")).expect("manifest");
    let model = ReportModel::build(&manifest, Path::new("m.toml"), "report-technical-dd", None)
        .expect("model");
    let provider: Arc<dyn LlmProvider> = Arc::new(MockLlm);
    let inv = run_investigation(provider, "mock/model", &model, Budget::default()).await;
    assert!(inv.is_none(), "remote-only manifests are not investigated");
}
