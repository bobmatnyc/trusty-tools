//! The non-interactive front end for `tga install` (#5216).
//!
//! Why: the wizard needed a terminal, so a stranger could not script tga, run
//! it from CI, or reach a populated config without answering prompts by hand.
//! This module turns flags and environment variables into the same [`Answers`]
//! the wizard collects, and — for a GitHub org — derives the repository set
//! from the API instead of asking for paths that already exist locally.
//!
//! What: [`resolve_plan`] is the entry point. [`answers_from_flags`] applies
//! the precedence (flag value first, then the environment) and refuses with a
//! list of exactly the missing flags rather than blocking on stdin.
//! [`plan_from_answers`] runs GitHub org discovery and produces the
//! [`InstallPlan`] both front ends render from.
//!
//! Test: `missing_flags_are_named_not_prompted_for`,
//! `flag_path_discovers_org_repos_and_writes_them` and the credential
//! precedence tests below.

use std::path::PathBuf;

use crate::collect::github::{build_http_client, discover_org_repos_at};
use crate::commands::install::{split_list, InstallArgs, InstallHost, InstallPm};
use crate::commands::install_plan::{
    env_var_for, Credential, InstallPlan, JiraSettings, LinearSettings, RepoEntry,
};
use crate::core::config::GithubConfig;

/// Environment variable holding the Bitbucket Cloud token.
const ENV_BITBUCKET_TOKEN: &str = "BITBUCKET_TOKEN";
/// Environment variable holding the JIRA base URL.
const ENV_JIRA_URL: &str = "JIRA_URL";
/// Environment variable holding the JIRA account email.
const ENV_JIRA_EMAIL: &str = "JIRA_EMAIL";
/// Environment variable holding the JIRA API token.
const ENV_JIRA_API_TOKEN: &str = "JIRA_API_TOKEN";
/// Environment variable holding the Linear API key.
const ENV_LINEAR_API_KEY: &str = "LINEAR_API_KEY";

/// The answers both `tga install` front ends collect, before discovery runs.
///
/// Why: the wizard asks and the flag path reads; naming the result once is
/// what lets a scripted install and a hand-walked one reach the same config.
/// What: a plain data struct. Nothing here prompts, reads the environment, or
/// makes a network call — [`plan_from_answers`] does the last of those.
/// Test: `wizard_and_flag_paths_render_identical_config` in `install.rs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Answers {
    /// Where repositories come from.
    pub host: InstallHost,
    /// GitHub org to discover repositories from.
    pub org: Option<String>,
    /// Bitbucket Cloud workspace.
    pub workspace: Option<String>,
    /// Explicit remote repositories, `owner/name` or `workspace/slug`.
    pub repo_slugs: Vec<String>,
    /// Already-cloned repositories, named by path.
    pub repo_paths: Vec<PathBuf>,
    /// Directory remote repositories are expected under.
    pub repo_cache: PathBuf,
    /// Host API token.
    pub host_token: Option<Credential>,
    /// Chosen project-management system.
    pub pm: InstallPm,
    /// JIRA settings, when JIRA was chosen.
    pub jira: Option<JiraSettings>,
    /// Linear settings, when Linear was chosen.
    pub linear: Option<LinearSettings>,
    /// Directory reports are written to.
    pub output_dir: String,
    /// LLM provider identifier.
    pub llm_provider: String,
    /// LLM API key.
    pub llm_api_key: Option<Credential>,
    /// GitHub REST root override. `None` means api.github.com; tests point it
    /// at a local mock server.
    pub github_api_base: Option<String>,
}

/// Resolve an [`InstallPlan`] from flags and the process environment.
///
/// Why: this is what `tga install` calls when there is no terminal to prompt.
/// What: [`answers_from_flags`] against `std::env::var`, then
/// [`plan_from_answers`].
/// Test: `flag_path_discovers_org_repos_and_writes_them`.
///
/// # Errors
///
/// Returns an error naming every missing required flag, or propagates a GitHub
/// org-discovery failure.
pub(crate) async fn resolve_plan(args: &InstallArgs) -> anyhow::Result<InstallPlan> {
    let answers = answers_from_flags(args, &|k| std::env::var(k).ok())?;
    plan_from_answers(answers).await
}

/// Apply flag-then-environment precedence and validate the required set.
///
/// Why: a token supplied through the environment must not be copied into the
/// generated config, and a missing flag must be named rather than prompted
/// for — a prompt with no terminal behind it hangs.
/// What: resolves every credential through [`credential`], collects the
/// missing required flags, and refuses with all of them at once. The `env`
/// lookup is injected so no test touches the real process environment (#1653).
/// Test: `missing_flags_are_named_not_prompted_for`,
/// `flag_value_beats_the_environment` and `env_token_is_emitted_as_a_reference`.
///
/// # Errors
///
/// Returns an error listing exactly the flags that are missing.
pub(crate) fn answers_from_flags(
    args: &InstallArgs,
    env: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<Answers> {
    let token_var = host_token_env(args.host);
    let host_token = token_var.and_then(|var| credential(args.host_token.as_deref(), var, env));

    let jira_url = value(args.jira_url.as_deref(), ENV_JIRA_URL, env);
    let jira_user = value(args.jira_user.as_deref(), ENV_JIRA_EMAIL, env);
    let jira_token = credential(args.jira_token.as_deref(), ENV_JIRA_API_TOKEN, env);
    let linear_key = credential(args.linear_api_key.as_deref(), ENV_LINEAR_API_KEY, env);

    let missing = missing_flags(
        args,
        host_token.is_some(),
        &[
            ("--jira-url <URL>", jira_url.is_some()),
            ("--jira-user <EMAIL>", jira_user.is_some()),
            ("--jira-token <TOKEN>", jira_token.is_some()),
            ("--linear-api-key <KEY>", linear_key.is_some()),
        ],
    );
    if !missing.is_empty() {
        anyhow::bail!(
            "`tga install` has no terminal to prompt on and these required flags are missing:\n  {}\n\n\
             Supply them, or run `tga install` on a terminal for the interactive wizard.",
            missing.join("\n  ")
        );
    }

    let pm = args.pm.unwrap_or(InstallPm::None);
    let llm_provider = args.llm_provider.to_lowercase();
    Ok(Answers {
        host: args.host.unwrap_or(InstallHost::Local),
        org: args.org.clone(),
        workspace: args.workspace.clone(),
        repo_slugs: args.repo.clone(),
        repo_paths: args.repo_path.clone(),
        repo_cache: args.repo_cache.clone(),
        host_token,
        pm,
        jira: match pm {
            InstallPm::Jira => Some(JiraSettings {
                url: jira_url.unwrap_or_default(),
                username: jira_user.unwrap_or_default(),
                token: jira_token.unwrap_or_else(|| Credential::literal("")),
            }),
            _ => None,
        },
        linear: match pm {
            InstallPm::Linear => Some(LinearSettings {
                api_key: linear_key.unwrap_or_else(|| Credential::literal("")),
                team_keys: args.linear_team.clone(),
            }),
            _ => None,
        },
        llm_api_key: credential(args.llm_api_key.as_deref(), env_var_for(&llm_provider), env),
        llm_provider,
        output_dir: args.output_dir.clone(),
        github_api_base: None,
    })
}

/// The required flags that are absent, in the order the error lists them.
///
/// Why: "listing exactly which flags are missing" is the whole contract — an
/// error naming one flag at a time costs a round trip per flag.
/// What: checks the two always-required flags first; host- and PM-specific
/// requirements are only judged once `--host` / `--pm` are known.
/// Test: `missing_flags_are_named_not_prompted_for`.
fn missing_flags(args: &InstallArgs, has_token: bool, pm_creds: &[(&str, bool)]) -> Vec<String> {
    let mut missing = Vec::new();
    if args.host.is_none() {
        missing.push("--host <local|github|bitbucket>".to_string());
    }
    if args.pm.is_none() {
        missing.push("--pm <none|github|jira|linear>".to_string());
    }

    match args.host {
        None => return missing,
        Some(InstallHost::Local) => {
            if args.repo_path.is_empty() {
                missing.push("--repo-path <PATH> (repeatable)".to_string());
            }
        }
        Some(InstallHost::Github) => {
            if args.org.is_none() && args.repo.is_empty() && args.repo_path.is_empty() {
                missing.push("--org <ORG> (or --repo <OWNER/NAME>, repeatable)".to_string());
            }
            if !has_token {
                missing.push("--host-token <TOKEN> (or set $GITHUB_TOKEN)".to_string());
            }
        }
        Some(InstallHost::Bitbucket) => {
            if args.workspace.is_none() {
                missing.push("--workspace <WORKSPACE>".to_string());
            }
            if args.repo.is_empty() {
                missing.push(
                    "--repo <WORKSPACE/SLUG> (repeatable — Bitbucket workspace discovery \
                     is not available yet, see #5220)"
                        .to_string(),
                );
            }
            if !has_token {
                missing.push("--host-token <TOKEN> (or set $BITBUCKET_TOKEN)".to_string());
            }
        }
    }

    let needed: &[usize] = match args.pm {
        Some(InstallPm::Jira) => &[0, 1, 2],
        Some(InstallPm::Linear) => &[3],
        _ => &[],
    };
    for i in needed {
        let (flag, present) = pm_creds[*i];
        if !present {
            missing.push(flag.to_string());
        }
    }
    missing
}

/// Turn collected answers into a renderable plan, discovering org repos.
///
/// Why (#5216): "given an org and a token, tga derives the repo set itself" is
/// the issue's closure condition, and [`discover_org_repos_at`] already knows
/// how to page it — this is the call site that was missing.
/// What: local paths pass through unchanged; a GitHub org is paged over the
/// API and unioned with any explicit `--repo` slugs; a Bitbucket workspace
/// takes the explicit list only, and records why.
/// Test: `flag_path_discovers_org_repos_and_writes_them` and
/// `bitbucket_records_that_discovery_is_unavailable`.
///
/// # Errors
///
/// Propagates a GitHub client-build or org-discovery failure.
pub(crate) async fn plan_from_answers(answers: Answers) -> anyhow::Result<InstallPlan> {
    let Answers {
        host,
        org,
        workspace,
        repo_slugs,
        repo_paths,
        repo_cache,
        host_token,
        pm: _,
        jira,
        linear,
        output_dir,
        llm_provider,
        llm_api_key,
        github_api_base,
    } = answers;

    let mut plan = InstallPlan {
        output_dir,
        llm_provider,
        llm_api_key,
        jira,
        linear,
        ..InstallPlan::default()
    };

    for p in &repo_paths {
        plan.repos.push(RepoEntry {
            name: leaf_name(p),
            path: p.clone(),
            org: None,
        });
    }

    match host {
        InstallHost::Local => {}
        InstallHost::Github => {
            let mut pairs = parse_slugs(&repo_slugs, None);
            if let Some(org) = &org {
                pairs.extend(discover(host_token.as_ref(), github_api_base.as_deref(), org).await?);
                if pairs.is_empty() {
                    plan.notes.push(format!(
                        "GitHub org `{org}` returned no repositories the supplied token can see"
                    ));
                }
            }
            plan.github_token = host_token;
            plan.github_org = org;
            push_remote(&mut plan, &repo_cache, pairs);
        }
        InstallHost::Bitbucket => {
            // #5216: non-interactive install path — Bitbucket workspace-to-repo
            // discovery is #5220, so the explicit list is the repository set.
            plan.notes.push(
                "Bitbucket Cloud workspace discovery is not available yet (#5220); the \
                 repositories listed here are the ones you named explicitly."
                    .to_string(),
            );
            let pairs = parse_slugs(&repo_slugs, workspace.as_deref());
            plan.bitbucket_workspace = workspace;
            plan.bitbucket_token = host_token;
            push_remote(&mut plan, &repo_cache, pairs);
        }
    }

    Ok(plan)
}

/// Page `org`'s repositories over the GitHub API.
///
/// `api_base` of `None` means api.github.com; tests point it at a mock server.
async fn discover(
    token: Option<&Credential>,
    api_base: Option<&str>,
    org: &str,
) -> anyhow::Result<Vec<(String, String)>> {
    let cfg = GithubConfig {
        token: token.map(|c| c.value.clone()),
        ..GithubConfig::default()
    };
    let http = build_http_client(&cfg)?;
    let base = api_base.unwrap_or("https://api.github.com");
    Ok(discover_org_repos_at(&http, base, org).await?)
}

/// Append `pairs` as `repositories[]` entries under `cache`, deduplicating.
fn push_remote(plan: &mut InstallPlan, cache: &std::path::Path, pairs: Vec<(String, String)>) {
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for (owner, name) in pairs {
        if !seen.insert((owner.clone(), name.clone())) {
            continue;
        }
        plan.repos.push(RepoEntry {
            path: cache.join(&owner).join(&name),
            name,
            org: Some(owner),
        });
    }
}

/// Split `owner/name` slugs, falling back to `default_owner` for a bare name.
fn parse_slugs(slugs: &[String], default_owner: Option<&str>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in slugs.iter().flat_map(|s| split_list(s)) {
        match raw.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() => {
                out.push((owner.to_string(), name.to_string()));
            }
            _ => {
                if let Some(owner) = default_owner {
                    out.push((owner.to_string(), raw));
                }
            }
        }
    }
    out
}

/// The final path component, or `repo` when there is none.
fn leaf_name(p: &std::path::Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .to_string()
}

/// The environment variable that stands in for `--host-token`.
fn host_token_env(host: Option<InstallHost>) -> Option<&'static str> {
    match host {
        Some(InstallHost::Github) => Some(trusty_common::env_vars::ENV_GITHUB_TOKEN),
        Some(InstallHost::Bitbucket) => Some(ENV_BITBUCKET_TOKEN),
        _ => None,
    }
}

/// Flag value, else `var` from `env`; blank counts as absent.
fn value(flag: Option<&str>, var: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    flag.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env(var)
                .map(|v| v.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

/// [`value`], tagged with where it came from so an env-sourced secret is
/// written to the config as `${VAR}` rather than in the clear.
fn credential(
    flag: Option<&str>,
    var: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<Credential> {
    if let Some(v) = flag.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(Credential::literal(v));
    }
    env(var)
        .map(|v| v.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|v| Credential::from_env(var, v))
}

/// Parse `argv` into [`InstallArgs`] the way clap does at the real call site.
///
/// Why: tests must exercise the same defaults and value parsing the CLI
/// applies; a hand-built struct would silently diverge from the derive.
/// What: flattens `InstallArgs` behind a throwaway parser.
/// Test: used by every test in this file and in `install.rs`.
#[cfg(test)]
pub(crate) fn args_for_tests(argv: &[&str]) -> InstallArgs {
    use clap::Parser;

    #[derive(Parser)]
    struct Wrapper {
        /// The flags under test.
        #[command(flatten)]
        args: InstallArgs,
    }
    Wrapper::parse_from(argv).args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::install_plan::render_yaml;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// An env lookup over an in-memory map — no process environment touched.
    fn env_map<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| (*v).to_string())
        }
    }

    /// No environment at all.
    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// Why (#5216): with no terminal the wizard would block on the first
    /// prompt forever. The flag path must name every missing flag instead.
    /// What: resolves a bare `tga install`, then a half-specified GitHub one.
    /// Test: this test itself.
    #[test]
    fn missing_flags_are_named_not_prompted_for() {
        let err = answers_from_flags(&args_for_tests(&["tga"]), &no_env)
            .expect_err("bare install must be refused");
        let msg = err.to_string();
        assert!(msg.contains("--host <local|github|bitbucket>"), "{msg}");
        assert!(msg.contains("--pm <none|github|jira|linear>"), "{msg}");
        // Host-specific flags cannot be judged before --host is known.
        assert!(!msg.contains("--org"), "{msg}");

        let err = answers_from_flags(
            &args_for_tests(&["tga", "--host", "github", "--pm", "none"]),
            &no_env,
        )
        .expect_err("github install without org or token must be refused");
        let msg = err.to_string();
        assert!(msg.contains("--org <ORG>"), "{msg}");
        assert!(msg.contains("$GITHUB_TOKEN"), "{msg}");
        assert!(!msg.contains("--host <"), "{msg}");
    }

    /// Why: Bitbucket has no discovery yet, so a workspace alone would produce
    /// an empty repository list. The refusal must say so rather than write it.
    /// What: `--host bitbucket --workspace acme` with no `--repo`.
    /// Test: this test itself.
    #[test]
    fn bitbucket_without_repos_is_refused_and_says_why() {
        let err = answers_from_flags(
            &args_for_tests(&[
                "tga",
                "--host",
                "bitbucket",
                "--workspace",
                "acme",
                "--pm",
                "none",
                "--host-token",
                "bb",
            ]),
            &no_env,
        )
        .expect_err("bitbucket without --repo must be refused");
        let msg = err.to_string();
        assert!(msg.contains("--repo <WORKSPACE/SLUG>"), "{msg}");
        assert!(msg.contains("#5220"), "{msg}");
    }

    /// Why: a JIRA or Linear choice with no credentials writes a config that
    /// cannot collect; the refusal must name the credential flags too.
    /// What: `--pm jira` and `--pm linear` with nothing else.
    /// Test: this test itself.
    #[test]
    fn pm_choice_pulls_in_its_own_credential_flags() {
        let args = args_for_tests(&[
            "tga",
            "--host",
            "local",
            "--repo-path",
            "/r",
            "--pm",
            "jira",
        ]);
        let msg = answers_from_flags(&args, &no_env)
            .expect_err("jira without creds")
            .to_string();
        assert!(msg.contains("--jira-url <URL>"), "{msg}");
        assert!(msg.contains("--jira-user <EMAIL>"), "{msg}");
        assert!(msg.contains("--jira-token <TOKEN>"), "{msg}");

        let args = args_for_tests(&[
            "tga",
            "--host",
            "local",
            "--repo-path",
            "/r",
            "--pm",
            "linear",
        ]);
        let msg = answers_from_flags(&args, &no_env)
            .expect_err("linear without a key")
            .to_string();
        assert!(msg.contains("--linear-api-key <KEY>"), "{msg}");
    }

    /// Why: the crate resolves credentials config-first then env (see
    /// `bitbucket::client::resolve_auth`); the flag path mirrors that with the
    /// flag standing in for the config value.
    /// What: token supplied both ways, then only through the environment.
    /// Test: this test itself.
    #[test]
    fn flag_value_beats_the_environment() {
        let args = args_for_tests(&[
            "tga",
            "--host",
            "github",
            "--org",
            "acme",
            "--pm",
            "none",
            "--host-token",
            "from-flag",
        ]);
        let answers =
            answers_from_flags(&args, &env_map(&[("GITHUB_TOKEN", "from-env")])).expect("resolves");
        let token = answers.host_token.expect("token resolved");
        assert_eq!(token.value, "from-flag");
        assert_eq!(token.emitted, "from-flag");
    }

    /// Why: an env-sourced token copied into config.yaml is a secret written
    /// into a file the operator is likely to commit.
    /// What: token only in the environment.
    /// Test: this test itself.
    #[test]
    fn env_token_is_emitted_as_a_reference() {
        let args = args_for_tests(&["tga", "--host", "github", "--org", "acme", "--pm", "none"]);
        let answers = answers_from_flags(&args, &env_map(&[("GITHUB_TOKEN", "ghp_secret")]))
            .expect("resolves");
        let token = answers.host_token.expect("token resolved");
        assert_eq!(token.value, "ghp_secret");
        assert_eq!(token.emitted, "${GITHUB_TOKEN}");
    }

    /// Why (#5216): the closure condition is that an org plus a token yields a
    /// populated config with no local paths supplied. This is that end to end,
    /// against a local mock — no test here can reach github.com.
    /// What: serves two pages of `GET /orgs/acme/repos`, resolves the flag
    /// path against them, and asserts every discovered repo reaches the YAML.
    /// Test: this test itself.
    #[tokio::test]
    async fn flag_path_discovers_org_repos_and_writes_them() {
        let server = MockServer::start().await;
        let page1: Vec<serde_json::Value> = (0..100)
            .map(|i| json!({"full_name": format!("acme/repo{i}")}))
            .collect();
        Mock::given(method("GET"))
            .and(path("/orgs/acme/repos"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page1))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/orgs/acme/repos"))
            .and(query_param("page", "2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"full_name": "acme/widget"}])),
            )
            .mount(&server)
            .await;

        let args = args_for_tests(&[
            "tga",
            "--host",
            "github",
            "--org",
            "acme",
            "--pm",
            "github",
            "--host-token",
            "ghp_test",
            "--repo-cache",
            "/cache",
        ]);
        let mut answers = answers_from_flags(&args, &no_env).expect("resolves");
        answers.github_api_base = Some(server.uri());
        let plan = plan_from_answers(answers)
            .await
            .expect("discovery succeeds");

        assert_eq!(plan.repos.len(), 101, "both pages must be collected");
        assert_eq!(plan.github_org.as_deref(), Some("acme"));
        let yaml = render_yaml(&plan);
        assert!(yaml.contains("path: \"/cache/acme/widget\""), "{yaml}");
        assert!(yaml.contains("name: \"widget\""), "{yaml}");
        assert!(yaml.contains("name: \"repo0\""), "{yaml}");
        assert!(yaml.contains("  org: \"acme\""), "{yaml}");
        assert!(yaml.contains("token: \"ghp_test\""), "{yaml}");
    }

    /// Why (#5216): a generated config that `Config::load` cannot read back is
    /// worse than no config — install would report success and every later
    /// command would fail. The discovered-org shape is the new one, so it is
    /// the one that has to be proven loadable.
    /// What: renders the discovered plan, writes it to a temp file, and loads
    /// it through the real deserializer.
    /// Test: this test itself.
    #[tokio::test]
    async fn generated_config_loads_back_through_config_load() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/orgs/acme/repos"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"full_name": "acme/widget"}])),
            )
            .mount(&server)
            .await;

        let args = args_for_tests(&[
            "tga",
            "--host",
            "github",
            "--org",
            "acme",
            "--pm",
            "linear",
            "--host-token",
            "ghp_test",
            "--linear-api-key",
            "lin_key",
            "--linear-team",
            "ENG",
            "--repo-cache",
            "/cache",
        ]);
        let mut answers = answers_from_flags(&args, &no_env).expect("resolves");
        answers.github_api_base = Some(server.uri());
        let plan = plan_from_answers(answers).await.expect("resolves");

        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = dir.path().join("config.yaml");
        std::fs::write(&cfg_path, render_yaml(&plan)).expect("write");
        let loaded = crate::core::config::Config::load(&cfg_path).expect("config loads back");

        assert_eq!(loaded.repositories.len(), 1);
        assert_eq!(
            loaded.repositories[0].path,
            std::path::PathBuf::from("/cache/acme/widget")
        );
        assert_eq!(loaded.repositories[0].org.as_deref(), Some("acme"));
        let github = loaded.github.expect("github block");
        assert_eq!(github.org.as_deref(), Some("acme"));
        let linear = loaded.linear.expect("linear block");
        assert_eq!(linear.team_keys, vec!["ENG".to_string()]);
    }

    /// Why: an org that the token cannot see returns an empty list, and a
    /// config with no repositories must say why rather than look complete.
    /// What: serves an empty first page.
    /// Test: this test itself.
    #[tokio::test]
    async fn empty_org_listing_is_recorded_as_a_note() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/orgs/ghost/repos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        let args = args_for_tests(&[
            "tga",
            "--host",
            "github",
            "--org",
            "ghost",
            "--pm",
            "none",
            "--host-token",
            "ghp_test",
        ]);
        let mut answers = answers_from_flags(&args, &no_env).expect("resolves");
        answers.github_api_base = Some(server.uri());
        let plan = plan_from_answers(answers).await.expect("resolves");
        assert!(plan.repos.is_empty());
        assert!(
            plan.notes.iter().any(|n| n.contains("no repositories")),
            "{:?}",
            plan.notes
        );
    }

    /// Why: the Bitbucket option must accept an explicit repo list and say
    /// discovery is not available, not silently emit an empty config.
    /// What: resolves a Bitbucket install with two explicit repos.
    /// Test: this test itself.
    #[tokio::test]
    async fn bitbucket_records_that_discovery_is_unavailable() {
        let args = args_for_tests(&[
            "tga",
            "--host",
            "bitbucket",
            "--workspace",
            "acme",
            "--repo",
            "acme/widget",
            "--repo",
            "gadget",
            "--pm",
            "none",
            "--host-token",
            "bb-token",
            "--repo-cache",
            "/cache",
        ]);
        let answers = answers_from_flags(&args, &no_env).expect("resolves");
        let plan = plan_from_answers(answers).await.expect("resolves");
        let yaml = render_yaml(&plan);
        assert!(yaml.contains("workspace: \"acme\""), "{yaml}");
        assert!(yaml.contains("path: \"/cache/acme/widget\""), "{yaml}");
        // A bare slug inherits the workspace.
        assert!(yaml.contains("path: \"/cache/acme/gadget\""), "{yaml}");
        assert!(yaml.contains("#5220"), "{yaml}");
    }
}
