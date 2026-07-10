//! End-to-end investigation test with a mocked provider (wave 3, #2357).
//!
//! Why: the whole point of the investigation pass is that when the repository is
//! readable the tool inspects it and produces evidence-backed findings — and that
//! an unverifiable finding is REJECTED, not softened.  This test drives the real
//! pipeline (select → mock LLM → verify → inject → merge → render) against a
//! fixture repo and asserts exactly that behaviour, with no network.
//! What: a fixture checkout with a hardcoded secret; a mock provider returning one
//! verifiable finding and one fabricated one; assertions that the verifiable one
//! renders with measured evidence and the fabricated one is dropped + noted.

use std::path::Path;
use std::sync::Arc;

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
