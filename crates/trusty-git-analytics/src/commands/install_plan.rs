//! The single config-emission path shared by both `tga install` front ends.
//!
//! Why (#5216): `tga install` grew a non-interactive flag path beside the TTY
//! wizard. Two front ends writing their own YAML would drift — one would gain
//! a `linear:` block or a repo `org:` field the other never learned to emit,
//! and a config generated non-interactively would stop matching the one an
//! operator walked through by hand. Both paths therefore build an
//! [`InstallPlan`] and hand it to [`render_yaml`]; nothing else writes config
//! text.
//!
//! What: [`InstallPlan`] is the resolved answer set — repositories, host,
//! credentials, PM system, output directory, LLM provider — with no notion of
//! where those answers came from. [`render_yaml`] turns one into the YAML that
//! `Config::load` reads back.
//!
//! Test: `render_yaml_minimal` and `render_yaml_with_github_and_llm` below;
//! `wizard_and_flag_paths_render_identical_config` in `install.rs` asserts the
//! two front ends reach byte-identical output for the same answers.

use std::path::PathBuf;

/// One entry under `repositories:` in the generated config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    /// Working-tree path the collector will walk.
    pub path: PathBuf,
    /// Display name recorded against the repository's rows.
    pub name: String,
    /// Owning GitHub org / Bitbucket workspace, when it is known.
    pub org: Option<String>,
}

/// A secret plus the text that stands in for it in the generated YAML.
///
/// Why: a token supplied through the environment must not be copied into a
/// file the operator may well commit. tga already expands `${VAR}` references
/// in config values, so an env-sourced credential is written as its reference
/// and re-read from the environment at collect time.
/// What: `value` is the live secret (used for the discovery calls `install`
/// itself makes); `emitted` is what [`render_yaml`] writes.
/// Test: `env_sourced_credentials_emit_a_reference_not_the_secret`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    /// The live secret, used for HTTP calls made during install.
    pub value: String,
    /// What gets written to YAML — `${VAR}` for an env-sourced credential.
    pub emitted: String,
}

impl Credential {
    /// A credential given directly (a flag, or a wizard prompt).
    #[must_use]
    pub fn literal(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            emitted: value.clone(),
            value,
        }
    }

    /// A credential read from environment variable `var`.
    #[must_use]
    pub fn from_env(var: &str, value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            emitted: format!("${{{var}}}"),
        }
    }
}

/// JIRA connection settings for the generated `jira:` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraSettings {
    /// Base URL of the JIRA instance.
    pub url: String,
    /// Account username or email.
    pub username: String,
    /// API token.
    pub token: Credential,
}

/// Linear connection settings for the generated `linear:` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearSettings {
    /// Linear API key.
    pub api_key: Credential,
    /// Team keys to scope issue fetches to; empty means every visible team.
    pub team_keys: Vec<String>,
}

/// The resolved answers a generated config is rendered from.
///
/// Why: the wizard and the flag path collect the same facts by different
/// means. Naming those facts once is what lets [`render_yaml`] stay the only
/// place that knows the YAML schema.
/// What: a plain data struct — every field is already resolved, with no
/// defaults left to apply and no prompting or env lookup remaining.
/// Test: both front ends construct one in `install.rs` / `install_flags.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallPlan {
    /// Repositories to collect from.
    pub repos: Vec<RepoEntry>,
    /// GitHub org the repository set was discovered from, when there was one.
    pub github_org: Option<String>,
    /// GitHub token.
    pub github_token: Option<Credential>,
    /// Bitbucket Cloud workspace.
    pub bitbucket_workspace: Option<String>,
    /// Bitbucket Cloud token.
    pub bitbucket_token: Option<Credential>,
    /// JIRA settings, when JIRA is the chosen PM system.
    pub jira: Option<JiraSettings>,
    /// Linear settings, when Linear is the chosen PM system.
    pub linear: Option<LinearSettings>,
    /// Directory reports are written to.
    pub output_dir: String,
    /// LLM provider identifier (`none`, `openai`, or `openrouter`).
    pub llm_provider: String,
    /// LLM API key, emitted only as a hint comment.
    pub llm_api_key: Option<Credential>,
    /// Operator-facing caveats written into the config as leading comments —
    /// for example that a discovered workspace turned out to be empty.
    pub notes: Vec<String>,
}

/// Render a plan as the YAML `tga` reads back through `Config::load`.
///
/// Why: one renderer means the wizard and the flag path cannot emit different
/// schemas for the same answers.
/// What: writes `repositories:`, `output:`, then only those optional blocks
/// the plan actually populated, so the generated file stays minimal.
/// Test: `render_yaml_minimal`, `render_yaml_with_github_and_llm`,
/// `render_yaml_emits_discovered_org_repos` and
/// `render_yaml_emits_linear_and_bitbucket` below.
#[must_use]
pub fn render_yaml(plan: &InstallPlan) -> String {
    let mut out = String::new();
    out.push_str("# Generated by `tga install`\n");
    for note in &plan.notes {
        out.push_str(&format!("# NOTE: {note}\n"));
    }
    out.push_str("version: \"1.0\"\n\n");

    out.push_str("repositories:\n");
    for r in &plan.repos {
        out.push_str(&format!("  - path: \"{}\"\n", r.path.display()));
        out.push_str(&format!("    name: \"{}\"\n", r.name));
        if let Some(org) = &r.org {
            out.push_str(&format!("    org: \"{org}\"\n"));
        }
    }
    out.push('\n');

    out.push_str("output:\n");
    out.push_str(&format!("  directory: \"{}\"\n", plan.output_dir));
    out.push_str("  formats: [csv, json, markdown]\n\n");

    render_hosts(plan, &mut out);
    render_pm(plan, &mut out);
    render_classification(plan, &mut out);
    out
}

/// Emit the `github:` and `bitbucket:` blocks a plan populated.
fn render_hosts(plan: &InstallPlan, out: &mut String) {
    if plan.github_token.is_some() || plan.github_org.is_some() {
        out.push_str("github:\n");
        if let Some(token) = &plan.github_token {
            out.push_str(&format!("  token: \"{}\"\n", token.emitted));
        }
        if let Some(org) = &plan.github_org {
            out.push_str(&format!("  org: \"{org}\"\n"));
        }
        out.push_str("  fetch_prs: true\n\n");
    }

    if plan.bitbucket_workspace.is_some() || plan.bitbucket_token.is_some() {
        out.push_str("bitbucket:\n");
        // #5220: emitted as the plural discovery list, not as the singular
        // `workspace` — that one is half of a `workspace`/`repo_slug` coordinate
        // and the validator would then demand a `repo_slug` this file has not
        // got. `workspaces` says what actually happened: the set came from the
        // API and is refreshed from it on every collect.
        if let Some(ws) = &plan.bitbucket_workspace {
            out.push_str(&format!("  workspaces: [\"{ws}\"]\n"));
        }
        if let Some(token) = &plan.bitbucket_token {
            out.push_str(&format!("  token: \"{}\"\n", token.emitted));
        }
        out.push_str("  fetch_prs: true\n\n");
    }
}

/// Emit the `jira:` and `linear:` blocks a plan populated.
fn render_pm(plan: &InstallPlan, out: &mut String) {
    if let Some(jira) = &plan.jira {
        out.push_str("jira:\n");
        out.push_str(&format!("  url: \"{}\"\n", jira.url));
        out.push_str(&format!("  username: \"{}\"\n", jira.username));
        out.push_str(&format!("  token: \"{}\"\n\n", jira.token.emitted));
    }

    if let Some(linear) = &plan.linear {
        out.push_str("linear:\n");
        out.push_str(&format!("  api_key: \"{}\"\n", linear.api_key.emitted));
        if !linear.team_keys.is_empty() {
            let keys = linear
                .team_keys
                .iter()
                .map(|k| format!("\"{k}\""))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("  team_keys: [{keys}]\n"));
        }
        out.push('\n');
    }
}

/// Emit the `classification:` block when an LLM provider was chosen.
fn render_classification(plan: &InstallPlan, out: &mut String) {
    if plan.llm_provider != "openai" && plan.llm_provider != "openrouter" {
        return;
    }
    out.push_str("classification:\n");
    out.push_str("  use_llm: true\n");
    out.push_str(&format!(
        "  llm_model: \"{}\"\n",
        default_model_for(&plan.llm_provider)
    ));
    if let Some(key) = &plan.llm_api_key {
        out.push_str(&format!(
            "  # API key (also pickable from ${} env var)\n",
            env_var_for(&plan.llm_provider)
        ));
        out.push_str(&format!("  # llm_api_key: \"{}\"\n", key.emitted));
    }
    out.push('\n');
}

/// Suggest a default model identifier for the chosen provider.
#[must_use]
pub fn default_model_for(provider: &str) -> &'static str {
    match provider {
        "openai" => "gpt-4o-mini",
        "openrouter" => "openrouter/auto",
        _ => "",
    }
}

/// The conventional environment variable holding the provider's API key.
/// Surfaces in the generated YAML as a hint comment.
#[must_use]
pub fn env_var_for(provider: &str) -> &'static str {
    match provider {
        "openai" => "OPENAI_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        _ => "LLM_API_KEY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plan carrying one local repository and nothing optional.
    fn minimal_plan(path: &str, output_dir: &str) -> InstallPlan {
        InstallPlan {
            repos: vec![RepoEntry {
                path: PathBuf::from(path),
                name: "repo".to_string(),
                org: None,
            }],
            output_dir: output_dir.to_string(),
            llm_provider: "none".to_string(),
            ..InstallPlan::default()
        }
    }

    /// Why: the minimal config must round-trip through `Config::load`, so no
    /// optional block may appear when nothing populated it.
    /// What: renders a repo-only plan.
    /// Test: this test itself.
    #[test]
    fn render_yaml_minimal() {
        let yaml = render_yaml(&minimal_plan("/tmp/repo", "./out"));
        assert!(yaml.contains("repositories:"));
        assert!(yaml.contains("path: \"/tmp/repo\""));
        assert!(yaml.contains("output:"));
        assert!(yaml.contains("directory: \"./out\""));
        assert!(!yaml.contains("github:"));
        assert!(!yaml.contains("bitbucket:"));
        assert!(!yaml.contains("jira:"));
        assert!(!yaml.contains("linear:"));
        assert!(!yaml.contains("classification:"));
    }

    /// Why: the GitHub + LLM combination is what the wizard emitted before
    /// #5216; the renderer must keep emitting it unchanged.
    /// What: renders a plan with a literal token and the `openai` provider.
    /// Test: this test itself.
    #[test]
    fn render_yaml_with_github_and_llm() {
        let mut plan = minimal_plan("/tmp/repo", "./out");
        plan.github_token = Some(Credential::literal("ghp_xxx"));
        plan.llm_provider = "openai".to_string();
        plan.llm_api_key = Some(Credential::literal("sk-xxx"));
        let yaml = render_yaml(&plan);
        assert!(yaml.contains("github:"));
        assert!(yaml.contains("ghp_xxx"));
        assert!(yaml.contains("classification:"));
        assert!(yaml.contains("use_llm: true"));
        assert!(yaml.contains("gpt-4o-mini"));
    }

    /// Why (#5216): org discovery is pointless if the discovered repos never
    /// reach the file, so the org and every discovered entry must be written.
    /// What: renders a two-repo plan carrying a `github_org`.
    /// Test: this test itself.
    #[test]
    fn render_yaml_emits_discovered_org_repos() {
        let mut plan = minimal_plan("/cache/acme/widget", "./out");
        plan.repos = vec![
            RepoEntry {
                path: PathBuf::from("/cache/acme/widget"),
                name: "widget".to_string(),
                org: Some("acme".to_string()),
            },
            RepoEntry {
                path: PathBuf::from("/cache/acme/gadget"),
                name: "gadget".to_string(),
                org: Some("acme".to_string()),
            },
        ];
        plan.github_org = Some("acme".to_string());
        let yaml = render_yaml(&plan);
        assert!(yaml.contains("name: \"widget\""), "{yaml}");
        assert!(yaml.contains("name: \"gadget\""), "{yaml}");
        assert!(yaml.contains("    org: \"acme\""), "{yaml}");
        assert!(yaml.contains("  org: \"acme\""), "{yaml}");
    }

    /// Why (#5216): Linear and Bitbucket are the two options the issue adds,
    /// and an option that renders nothing is not an option.
    /// What: renders a plan carrying both.
    /// Test: this test itself.
    #[test]
    fn render_yaml_emits_linear_and_bitbucket() {
        let mut plan = minimal_plan("/tmp/repo", "./out");
        plan.bitbucket_workspace = Some("acme".to_string());
        plan.bitbucket_token = Some(Credential::literal("bb-token"));
        plan.linear = Some(LinearSettings {
            api_key: Credential::literal("lin_xxx"),
            team_keys: vec!["ENG".to_string(), "OPS".to_string()],
        });
        plan.notes = vec!["the workspace looked empty".to_string()];
        let yaml = render_yaml(&plan);
        assert!(yaml.contains("bitbucket:"), "{yaml}");
        // #5220: the plural discovery list, not the singular half-coordinate
        // the validator would then demand a `repo_slug` alongside.
        assert!(yaml.contains("workspaces: [\"acme\"]"), "{yaml}");
        assert!(yaml.contains("linear:"), "{yaml}");
        assert!(yaml.contains("team_keys: [\"ENG\", \"OPS\"]"), "{yaml}");
        assert!(
            yaml.contains("# NOTE: the workspace looked empty"),
            "{yaml}"
        );
    }

    /// Why: an env-sourced token copied into config.yaml is a secret written
    /// to a file the operator is likely to commit.
    /// What: renders a plan whose credentials all came from the environment.
    /// Test: this test itself.
    #[test]
    fn env_sourced_credentials_emit_a_reference_not_the_secret() {
        let mut plan = minimal_plan("/tmp/repo", "./out");
        plan.github_token = Some(Credential::from_env("GITHUB_TOKEN", "ghp_secret"));
        let yaml = render_yaml(&plan);
        assert!(yaml.contains("token: \"${GITHUB_TOKEN}\""), "{yaml}");
        assert!(!yaml.contains("ghp_secret"), "{yaml}");
    }
}
