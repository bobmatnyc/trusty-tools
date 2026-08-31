//! End-to-end coverage for the configured bundled-template provider policy
//! (#3766).
//!
//! Why: `llm::provider_policy`'s own tests pin the predicate and the config
//! parse. What they cannot show is the property the ticket is actually about —
//! that a bundled template's provider stops depending on the ambient
//! environment, covers EVERY unpinned template rather than a hand-picked few,
//! survives a template reprovision, and changes nothing at all while the
//! policy is unset. Each test below asserts one of those, and each carries its
//! own control: the `policy = None` arm IS the pre-#3766 code path, so a test
//! that shows `None` behaving differently from `Some(provider)` is
//! demonstrating the defect and the fix in one run.
//! What: drives `AgentConfig::apply_provider_pin_with_policy` over the real
//! bundled templates read from this crate's `.trusty-agents/agents/` tree —
//! the same bytes `agents::bundled` embeds — rather than over fixtures, so a
//! template added later is covered automatically.
//! Test: this module IS the test surface.

use std::path::{Path, PathBuf};

use crate::agents::AgentConfig;

/// The policy provider these tests configure.
///
/// Why: Bedrock's `credential_name()` is `None`, so pinning to it needs no
/// API key present. Every other pinnable provider would make these tests pass
/// or fail on whether the developer's shell happens to export a key.
const TEST_POLICY: &str = "bedrock";

/// The bundled templates #3766 names as resolving their provider ambiently.
///
/// Why: acceptance criterion 4 is that the mechanism covers ALL of them, not a
/// subset. Listing them here means dropping one from the covered set fails a
/// test rather than passing quietly.
const CITED_UNPINNED_TEMPLATES: [&str; 7] = [
    "assistant/agent.toml",
    "izzie/agent.toml",
    "izzie.toml",
    "personal-assistant.toml",
    "ctrl.toml",
    "ctrl/agent.toml",
    "cto-assistant/agent.toml",
];

/// This crate's bundled agent-template directory.
fn templates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".trusty-agents")
        .join("agents")
}

/// Every bundled template as `(path relative to the agents dir, TOML body)`.
///
/// Why: reading the tree rather than naming files keeps the sweep tests honest
/// as the roster changes.
/// What: flat `<name>.toml` plus one level of directory packages
/// (`<name>/agent.toml`), skipping `.bak` archives.
fn bundled_templates() -> Vec<(String, String)> {
    fn collect(dir: &Path, prefix: &str, out: &mut Vec<(String, String)>) {
        let entries = std::fs::read_dir(dir).expect("read the bundled agents dir");
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                collect(&path, &format!("{prefix}{name}/"), out);
            } else if name.ends_with(".toml") && !name.contains(".bak") {
                let body = std::fs::read_to_string(&path).expect("read template");
                out.push((format!("{prefix}{name}"), body));
            }
        }
    }

    let mut out = Vec::new();
    collect(&templates_dir(), "", &mut out);
    out.sort();
    assert!(
        out.len() >= CITED_UNPINNED_TEMPLATES.len(),
        "expected the bundled roster to be non-trivial, found {}",
        out.len()
    );
    out
}

/// Parse `body` into an `AgentConfig` without running the loader's ambient
/// steps.
///
/// Why: `AgentConfig::load` resolves `TAGENT_MODEL_*` overrides and reads the
/// operator config from the real `$HOME`, either of which would make these
/// assertions depend on the developer's environment. Parsing directly leaves
/// `[agent].model` exactly as the template declares it, which is what the
/// byte-level comparisons below need.
/// What: parses to a `toml::Value` and fills in `system_prompt.content` only
/// when the template omits it — the directory-package form
/// (`assistant/agent.toml`, `ctrl/agent.toml`) keeps its prompt in a sibling
/// file. `[agent]` and `[llm]` are never touched, so every field under test
/// stays byte-exact.
fn parse_template(rel: &str, body: &str) -> AgentConfig {
    let mut value: toml::Value = body
        .parse()
        .unwrap_or_else(|e| panic!("parse bundled template {rel}: {e}"));
    let table = value
        .as_table_mut()
        .unwrap_or_else(|| panic!("{rel} is not a TOML table"));
    let prompt = table
        .entry("system_prompt")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    if let Some(prompt) = prompt.as_table_mut()
        && !prompt.contains_key("content")
    {
        prompt.insert(
            "content".to_string(),
            toml::Value::String("placeholder".to_string()),
        );
    }
    value
        .try_into()
        .unwrap_or_else(|e| panic!("deserialize bundled template {rel}: {e}"))
}

/// Acceptance criterion 5 — with no policy configured, loading a bundled
/// template resolves exactly what it resolved before #3766.
///
/// Why: the feature is only safe to ship if "unset" is a true no-op. This is
/// the byte-level form of that claim: `apply_provider_pin_with_policy` writes
/// exactly three fields (`agent.model`, `agent.provider_id`,
/// `llm.use_anthropic_direct`), so asserting all three are unchanged over
/// every bundled template IS the assertion that nothing resolved differently.
/// Test: itself.
#[test]
fn an_unset_policy_leaves_every_bundled_template_unchanged() {
    for (rel, body) in bundled_templates() {
        let mut cfg = parse_template(&rel, &body);
        let model_before = cfg.agent.model.clone();
        let provider_before = cfg.agent.provider_id.clone();
        let direct_before = cfg.llm.use_anthropic_direct;

        cfg.apply_provider_pin_with_policy(None)
            .unwrap_or_else(|e| panic!("{rel} must load with no policy configured: {e:#}"));

        assert_eq!(cfg.agent.model, model_before, "{rel}: model");
        assert_eq!(cfg.agent.provider_id, provider_before, "{rel}: provider_id");
        assert_eq!(
            cfg.llm.use_anthropic_direct, direct_before,
            "{rel}: use_anthropic_direct"
        );
    }
}

/// Acceptance criterion 4 — the policy reaches EVERY unpinned template, and
/// only those.
///
/// Why: a mechanism that covered five of the seven cited templates would leave
/// the other two resolving ambiently and look fixed. Sweeping the whole tree
/// and asserting the cited seven are among the templates the policy moved is
/// what makes a partial fix fail.
/// What: for each template, `applies_to` decides the expectation — an unpinned
/// template with a bare slug must come out carrying the policy provider's
/// routing marker; a template that pins a provider or names one in its slug
/// must come out untouched.
/// Test: itself.
#[test]
fn the_policy_covers_every_unpinned_bundled_template() {
    let mut moved = Vec::new();

    for (rel, body) in bundled_templates() {
        let mut cfg = parse_template(&rel, &body);
        let model_before = cfg.agent.model.clone();
        let governed = crate::llm::provider_policy::applies_to(
            cfg.agent.provider_id.as_deref(),
            &cfg.agent.model,
        );

        cfg.apply_provider_pin_with_policy(Some(TEST_POLICY))
            .unwrap_or_else(|e| panic!("{rel} must load under the policy: {e:#}"));

        if governed {
            assert_eq!(
                cfg.agent.model,
                format!("{TEST_POLICY}/{model_before}"),
                "{rel}: an unpinned template must adopt the configured provider"
            );
            assert_eq!(cfg.agent.provider_id.as_deref(), Some(TEST_POLICY), "{rel}");
            moved.push(rel);
        } else {
            assert_eq!(
                cfg.agent.model, model_before,
                "{rel}: a template that names its own provider must be left alone"
            );
        }
    }

    for cited in CITED_UNPINNED_TEMPLATES {
        assert!(
            moved.iter().any(|m| m == cited),
            "#3766 names {cited} as resolving its provider ambiently, but the policy \
             did not reach it; covered: {moved:?}"
        );
    }
}

/// Acceptance criterion 3 — with the policy set, the SAME template resolves
/// the SAME provider under different ambient credentials.
///
/// Why: this is the defect. The `None` arm below is the pre-#3766 path and it
/// FAILS the equality this test demands — an ambient `ANTHROPIC_API_KEY`
/// flips `use_anthropic_direct`, an ambient OpenRouter key rewrites the slug,
/// and the template silently runs somewhere else. The `Some` arm shows the
/// configured policy making both credential environments resolve identically,
/// via the same `provider_id` skip `ctrl::config::apply_credential_routing`
/// already had from #3765.
/// What: routes `ctrl.toml` — one of the seven templates #3766 cites —
/// through `apply_credential_routing` under `AnthropicDirect` and under
/// `OpenRouter`, and compares the resulting `(model, use_anthropic_direct)`.
/// Test: itself.
#[test]
fn a_configured_policy_survives_differing_ambient_credentials() {
    use crate::llm::credentials::LlmCredentials;

    let body = std::fs::read_to_string(templates_dir().join("ctrl.toml")).expect("read ctrl.toml");

    let resolve = |policy: Option<&str>, creds: &LlmCredentials| {
        let mut cfg = parse_template("ctrl.toml", &body);
        cfg.apply_provider_pin_with_policy(policy)
            .expect("ctrl.toml loads");
        crate::ctrl::config::apply_credential_routing(&mut cfg, creds);
        (cfg.agent.model.clone(), cfg.llm.use_anthropic_direct)
    };

    // Control — the pre-#3766 behaviour, still reachable with no policy set:
    // the ambient credential decides, so the two environments disagree.
    let ambient_anthropic = resolve(None, &LlmCredentials::AnthropicDirect);
    let ambient_openrouter = resolve(None, &LlmCredentials::OpenRouter);
    assert_ne!(
        ambient_anthropic, ambient_openrouter,
        "without a policy the ambient credential is expected to decide — if this ever \
         holds, the test below has stopped proving anything"
    );

    let pinned_anthropic = resolve(Some(TEST_POLICY), &LlmCredentials::AnthropicDirect);
    let pinned_openrouter = resolve(Some(TEST_POLICY), &LlmCredentials::OpenRouter);
    assert_eq!(
        pinned_anthropic, pinned_openrouter,
        "the configured policy must decide, not the ambient credential"
    );
    assert_eq!(
        pinned_anthropic.0,
        format!("{TEST_POLICY}/claude-sonnet-4-6")
    );
    assert!(
        !pinned_anthropic.1,
        "use_anthropic_direct must follow the policy"
    );
}

/// Acceptance criterion 1 — a bundled-template refresh cannot regress a
/// policy-resolved provider.
///
/// Why: `agents::bundled` rewrites a deployed template whenever its bytes
/// differ from the embedded bundle, and it never re-applies a live edit. Had
/// #3766 been implemented by writing a provider into the template files, every
/// reprovision would have silently reverted it. Keeping the policy in the
/// operator config and applying it at LOAD is what makes this test pass, so
/// this is the test that would fail an in-template implementation.
/// What: deploys the bundle into a tempdir, records the policy-resolved
/// provider for `ctrl.toml`, forces a genuine stamp-mismatch refresh (an
/// on-disk edit plus a stale `.bundled-stamp`), and re-resolves.
/// Test: itself.
#[test]
fn a_reprovision_cannot_regress_the_policy_resolved_provider() {
    let target = tempfile::tempdir().expect("tempdir");
    let target = target.path();
    crate::agents::bundled::ensure_bundled_agents_deployed_in(target).expect("initial deploy");

    let deployed = target.join("ctrl.toml");
    let resolve = || {
        let body = std::fs::read_to_string(&deployed).expect("read the deployed template");
        let mut cfg = parse_template("ctrl.toml", &body);
        cfg.apply_provider_pin_with_policy(Some(TEST_POLICY))
            .expect("deployed ctrl.toml loads under the policy");
        (cfg.agent.model.clone(), cfg.agent.provider_id.clone())
    };

    let before = resolve();
    assert_eq!(before.1.as_deref(), Some(TEST_POLICY));

    // A hand-edit plus a stale stamp is exactly the state
    // `ensure_bundled_agents_deployed_in` refreshes from.
    let edited = format!(
        "{}\n# local edit\n",
        std::fs::read_to_string(&deployed).expect("read deployed")
    );
    std::fs::write(&deployed, edited).expect("write the edited template");
    std::fs::write(target.join(".bundled-stamp"), "stale-stamp").expect("write a stale stamp");

    let report =
        crate::agents::bundled::ensure_bundled_agents_deployed_in(target).expect("refresh pass");
    assert!(
        report.refreshed >= 1,
        "the stale stamp must have driven a real refresh: {report:?}"
    );

    assert_eq!(
        resolve(),
        before,
        "a template refresh must not change the policy-resolved provider"
    );
}

/// Acceptance criterion 2 — no bundled template defaults to the
/// unauthenticated local endpoint.
///
/// Why: `ollama/…` routes to an unauthenticated localhost server, so a
/// template defaulting to it degrades silently to whatever model that host
/// happens to serve — the failure mode #3556 already recorded for `ctrl`.
/// Nothing about #3766 should reintroduce one, and the policy is the obvious
/// place a `local` default could leak in.
/// What: asserts no bundled template names the local provider, either in its
/// model slug (`ollama/…`, `local/…`) or in an explicit `provider_id`.
/// Test: itself.
#[test]
fn no_bundled_template_defaults_to_the_local_provider() {
    use trusty_common::inference::registry::ProviderId;

    for (rel, body) in bundled_templates() {
        let cfg = parse_template(&rel, &body);
        assert_ne!(
            ProviderId::from_slug_prefix(&cfg.agent.model),
            Some(ProviderId::Local),
            "{rel}: model '{}' defaults to the unauthenticated local endpoint",
            cfg.agent.model
        );
        if let Some(pin) = cfg.agent.provider_id.as_deref() {
            assert_ne!(
                trusty_common::inference::registry::capabilities_for(pin).map(|c| c.id),
                Some(ProviderId::Local),
                "{rel}: provider_id '{pin}' defaults to the unauthenticated local endpoint"
            );
        }
    }
}

/// An explicit `[agent].provider_id` outranks the operator default.
///
/// Why: the policy is a DEFAULT. If it overwrote a pin, #3766 would have
/// broken #3765 — an operator who pinned one agent to a specific provider
/// would find it silently retargeted by a machine-wide setting.
/// Test: itself.
#[test]
fn an_explicit_pin_outranks_the_configured_policy() {
    let mut cfg = parse_template(
        "<inline>",
        r#"
[agent]
name = "pinned"
role = "engineer"
model = "claude-sonnet-4-6"
description = "x"
provider_id = "bedrock"

[llm]
temperature = 0.0
max_tokens = 1024
"#,
    );

    // A policy naming a DIFFERENT provider must not be adopted. `atlascloud`
    // is used only as the policy value here — it is never resolved, because
    // the explicit pin short-circuits the adoption step.
    cfg.apply_provider_pin_with_policy(Some("atlascloud"))
        .expect("an explicitly pinned agent loads");

    assert_eq!(cfg.agent.provider_id.as_deref(), Some("bedrock"));
    assert_eq!(cfg.agent.model, "bedrock/claude-sonnet-4-6");
}

/// A policy naming an unusable provider fails the load closed, naming the
/// config key that set it.
///
/// Why: the whole point of routing the policy through
/// `llm::provider_pin::resolve` is that it inherits #3765's fail-closed
/// contract instead of falling back to an ambient credential. The operator
/// then needs to know WHICH of the two sources to fix — the agent's TOML or
/// their `config.toml` — which the raw pin error cannot tell them.
/// Test: itself.
#[test]
fn an_unusable_policy_fails_closed_and_names_the_config_key() {
    let mut cfg = parse_template(
        "<inline>",
        r#"
[agent]
name = "unpinned"
role = "engineer"
model = "claude-sonnet-4-6"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
"#,
    );

    let err = cfg
        .apply_provider_pin_with_policy(Some("not-a-provider"))
        .expect_err("an unknown provider must fail the load, not fall back");
    let msg = format!("{err:#}");
    assert!(msg.contains("default_provider_id"), "{msg}");
    assert!(msg.contains("not-a-provider"), "{msg}");
}
