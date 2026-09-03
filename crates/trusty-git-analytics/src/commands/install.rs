//! `tga install` — the configuration wizard and its non-interactive twin.
//!
//! Why (#5216): the wizard could only be driven by a human at a terminal, so a
//! stranger scripting tga, or running it in CI, had nothing to run. This module
//! keeps the prompts and adds a flag path beside them; both collect the same
//! `install_flags::Answers` and both render through
//! [`install_plan::render_yaml`](super::install_plan::render_yaml), so the two
//! cannot drift.
//!
//! What: [`run`] picks a front end — the flag path when non-interactive flags
//! were supplied or stdin is not a terminal, the wizard otherwise — then writes
//! the rendered config. The wizard stays dependency-free (plain stdin) so the
//! CLI keeps cross-compiling to musl / Apple Silicon without terminal crates.
//!
//! Test: `wizard_and_flag_paths_render_identical_config` below pins the two
//! front ends together; `install_flags` covers the flag path's own behaviour.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use clap::{Args, ValueEnum};
use tga::core::config::Config;

use super::install_flags::{plan_from_answers, resolve_plan, Answers};
use super::install_plan::{render_yaml, Credential, InstallPlan, JiraSettings, LinearSettings};

/// Where repositories come from.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum InstallHost {
    /// Repositories are already cloned locally and named by path.
    #[default]
    Local,
    /// A GitHub org (or an explicit `owner/name` list).
    Github,
    /// A Bitbucket Cloud workspace with an explicit `workspace/slug` list.
    Bitbucket,
}

/// Which project-management system supplies work items.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum InstallPm {
    /// No PM integration.
    #[default]
    None,
    /// GitHub Issues — uses the GitHub credential already configured.
    Github,
    /// Atlassian JIRA.
    Jira,
    /// Linear.
    Linear,
}

/// Arguments for `tga install`.
#[derive(Args, Debug, Clone)]
#[command(
    about = "Configuration wizard for first-time setup — interactive or flag-driven.",
    long_about = "Generate a config.yaml bootstrapped with the minimal required fields.\n\n\
On a terminal with no flags this walks through a series of prompts; press\n\
<enter> at each to accept the default shown in brackets.\n\n\
Given --host and --pm (or with stdin not a terminal) it runs non-interactively\n\
from flags and environment variables instead, and never prompts. With --host\n\
github and --org it discovers the org's repositories over the GitHub API and\n\
writes them into the generated config.\n\n\
Flag values win over environment variables. A credential taken from the\n\
environment is written to the config as a ${VAR} reference, not as the secret.",
    after_help = "EXAMPLES:\n\
  # First-time setup on a terminal (writes config.yaml in the current directory)\n\
  tga install\n\n\
  # Zero-config org audit, no terminal required\n\
  GITHUB_TOKEN=ghp_… tga install --host github --org acme --pm github\n\n\
  # Bitbucket Cloud — workspace discovery is not available yet (#5220), so name the repos\n\
  tga install --host bitbucket --workspace acme --repo acme/widget --pm jira \\\n\
    --jira-url https://acme.atlassian.net --jira-user me@acme.com\n\n\
  # Write config to a custom path, overwriting any existing file\n\
  tga install --output /etc/tga/config.yaml --force\n\n\
TIPS:\n\
  - After running install, validate the config with `tga collect --validate-only`.\n\
  - Re-run at any time to regenerate the config from scratch."
)]
pub struct InstallArgs {
    /// Path to write the generated config to.
    ///
    /// Defaults to `./config.yaml` in the current working directory.
    #[arg(short, long, default_value = "config.yaml")]
    pub output: PathBuf,

    /// Overwrite an existing config file without prompting.
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Run from flags only, never prompting. Implied when stdin is not a terminal.
    #[arg(long, default_value_t = false)]
    pub non_interactive: bool,

    /// Where repositories come from.
    #[arg(long, value_enum)]
    pub host: Option<InstallHost>,

    /// GitHub org to discover repositories from (`--host github`).
    #[arg(long, value_name = "ORG")]
    pub org: Option<String>,

    /// Bitbucket Cloud workspace (`--host bitbucket`).
    #[arg(long, value_name = "WORKSPACE")]
    pub workspace: Option<String>,

    /// Explicit remote repository, `owner/name` or `workspace/slug`. Repeatable.
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Vec<String>,

    /// Already-cloned repository to collect from. Repeatable.
    #[arg(long, value_name = "PATH")]
    pub repo_path: Vec<PathBuf>,

    /// Directory remote repositories are expected under in the generated config.
    #[arg(long, value_name = "DIR", default_value = "./repos")]
    pub repo_cache: PathBuf,

    /// Host API token. Falls back to `$GITHUB_TOKEN` / `$BITBUCKET_TOKEN`.
    #[arg(long, value_name = "TOKEN")]
    pub host_token: Option<String>,

    /// Project-management system supplying work items.
    #[arg(long, value_enum)]
    pub pm: Option<InstallPm>,

    /// JIRA base URL. Falls back to `$JIRA_URL`.
    #[arg(long, value_name = "URL")]
    pub jira_url: Option<String>,

    /// JIRA username or email. Falls back to `$JIRA_EMAIL`.
    #[arg(long, value_name = "EMAIL")]
    pub jira_user: Option<String>,

    /// JIRA API token. Falls back to `$JIRA_API_TOKEN`.
    #[arg(long, value_name = "TOKEN")]
    pub jira_token: Option<String>,

    /// Linear API key. Falls back to `$LINEAR_API_KEY`.
    #[arg(long, value_name = "KEY")]
    pub linear_api_key: Option<String>,

    /// Linear team key to scope issue fetches to. Repeatable.
    #[arg(long, value_name = "TEAM")]
    pub linear_team: Vec<String>,

    /// Directory reports are written to.
    #[arg(long, value_name = "DIR", default_value = "./tga-output")]
    pub output_dir: String,

    /// LLM provider for Tier-4 classification: `none`, `openai` or `openrouter`.
    #[arg(long, value_name = "PROVIDER", default_value = "none")]
    pub llm_provider: String,

    /// LLM API key. Falls back to the provider's conventional env var.
    #[arg(long, value_name = "KEY")]
    pub llm_api_key: Option<String>,
}

impl InstallArgs {
    /// Whether the caller asked for the flag path.
    ///
    /// Why: a flag the wizard would silently ignore is worse than no flag at
    /// all, so supplying any non-interactive flag selects the flag path even on
    /// a terminal.
    /// What: true for `--non-interactive` or any answer-bearing flag.
    /// Test: `answer_flags_select_the_flag_path` below.
    #[must_use]
    pub fn wants_non_interactive(&self) -> bool {
        self.non_interactive
            || self.host.is_some()
            || self.pm.is_some()
            || self.org.is_some()
            || self.workspace.is_some()
            || !self.repo.is_empty()
            || !self.repo_path.is_empty()
            || self.host_token.is_some()
            || self.jira_url.is_some()
            || self.jira_user.is_some()
            || self.jira_token.is_some()
            || self.linear_api_key.is_some()
            || !self.linear_team.is_empty()
            || self.llm_api_key.is_some()
    }
}

/// Run `tga install`.
///
/// Why: one entry point picks the front end so the overwrite guard, the output
/// directory creation and the config write happen exactly once each.
/// What: refuses to clobber an existing config without `--force`, resolves an
/// [`InstallPlan`] from either the flag path or the wizard, then renders and
/// writes it.
/// Test: `install_flags::flag_path_discovers_org_repos_and_writes_them` and
/// `wizard_and_flag_paths_render_identical_config` below.
///
/// # Errors
///
/// Returns an error if `args.output` exists and `--force` was not supplied, if
/// required flags are missing on the non-interactive path, if stdin reads fail,
/// if org discovery fails, or if the output path is not writable.
pub async fn run(_config: Config, args: InstallArgs) -> anyhow::Result<()> {
    if args.output.exists() && !args.force {
        anyhow::bail!(
            "{} already exists. Re-run with --force to overwrite.",
            args.output.display()
        );
    }

    // #5216: non-interactive install path — flags win, then the environment.
    let plan = if args.wants_non_interactive() || !io::stdin().is_terminal() {
        resolve_plan(&args).await?
    } else {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let answers = collect_answers(&mut input, &args)?;
        plan_from_answers(answers).await?
    };

    write_plan(&plan, &args.output)
}

/// Render `plan` and write it to `output`, creating the report directory.
///
/// # Errors
///
/// Returns an error if the report directory cannot be created or the config
/// cannot be written.
fn write_plan(plan: &InstallPlan, output: &std::path::Path) -> anyhow::Result<()> {
    let output_dir_path = PathBuf::from(&plan.output_dir);
    if let Err(e) = std::fs::create_dir_all(&output_dir_path) {
        anyhow::bail!(
            "cannot create output directory {}: {e}",
            output_dir_path.display()
        );
    }
    for note in &plan.notes {
        eprintln!("  note: {note}");
    }
    std::fs::write(output, render_yaml(plan))?;
    println!(
        "\nConfig written to {}. Run: tga analyze --config {}",
        output.display(),
        output.display()
    );
    Ok(())
}

/// Walk the operator through the prompts and return their answers.
///
/// Why: returning [`Answers`] rather than YAML is what lets the wizard and the
/// flag path share one renderer and one org-discovery call.
/// What: prompts for host, repositories, credentials, PM system, output
/// directory and LLM provider, defaulting from `args` where a flag was given.
/// Test: `wizard_and_flag_paths_render_identical_config` drives this with a
/// scripted stdin.
///
/// # Errors
///
/// Returns an error on a stdin read failure, on EOF where a value is required,
/// or when a required answer is left blank.
fn collect_answers<R: BufRead>(reader: &mut R, args: &InstallArgs) -> anyhow::Result<Answers> {
    println!("tga install — interactive configuration wizard");
    println!("Press <enter> to accept the default shown in [brackets].\n");

    let mut answers = Answers {
        repo_cache: args.repo_cache.clone(),
        ..Answers::default()
    };

    let host = prompt(
        reader,
        "Code host — choose one: local / github / bitbucket",
        Some("local"),
    )?;
    answers.host = match host.to_lowercase().as_str() {
        "github" => InstallHost::Github,
        "bitbucket" => InstallHost::Bitbucket,
        _ => InstallHost::Local,
    };
    prompt_host(reader, &mut answers)?;
    prompt_pm(reader, &mut answers)?;

    answers.output_dir = prompt(reader, "Output directory", Some("./tga-output"))?;

    let llm_provider = prompt(
        reader,
        "LLM provider — choose one: none / openai / openrouter",
        Some("none"),
    )?
    .to_lowercase();
    answers.llm_api_key = if llm_provider == "openai" || llm_provider == "openrouter" {
        prompt_optional(
            reader,
            &format!("{llm_provider} API key (leave blank to set later via env var)"),
        )?
        .map(Credential::literal)
    } else {
        None
    };
    answers.llm_provider = llm_provider;

    Ok(answers)
}

/// Prompt for the host-specific answers: repositories and the host credential.
fn prompt_host<R: BufRead>(reader: &mut R, answers: &mut Answers) -> anyhow::Result<()> {
    match answers.host {
        InstallHost::Local => {
            answers.repo_paths = prompt_paths(reader)?;
        }
        InstallHost::Github => {
            answers.org = prompt_optional(
                reader,
                "GitHub org to discover repositories from (leave blank to list paths instead)",
            )?;
            if answers.org.is_none() {
                answers.repo_paths = prompt_paths(reader)?;
            }
            answers.host_token =
                prompt_optional(reader, "GitHub token (optional, leave blank to skip)")?
                    .map(Credential::literal);
        }
        InstallHost::Bitbucket => {
            answers.workspace = Some(prompt(reader, "Bitbucket Cloud workspace", None)?);
            println!(
                "  Bitbucket workspace discovery is not available yet (#5220) — name the repositories."
            );
            answers.repo_slugs = split_list(&prompt(
                reader,
                "Bitbucket repositories (comma-separated <workspace>/<slug>)",
                None,
            )?);
            answers.host_token =
                prompt_optional(reader, "Bitbucket token (optional, leave blank to skip)")?
                    .map(Credential::literal);
        }
    }
    Ok(())
}

/// Prompt for the PM system and whatever credentials it needs.
fn prompt_pm<R: BufRead>(reader: &mut R, answers: &mut Answers) -> anyhow::Result<()> {
    let pm = prompt(
        reader,
        "Project-management system — choose one: none / github / jira / linear",
        Some("none"),
    )?;
    answers.pm = match pm.to_lowercase().as_str() {
        "github" => InstallPm::Github,
        "jira" => InstallPm::Jira,
        "linear" => InstallPm::Linear,
        _ => InstallPm::None,
    };
    match answers.pm {
        InstallPm::Jira => {
            let url = prompt(reader, "JIRA URL", None)?;
            let username = prompt(reader, "JIRA username/email", None)?;
            let token = prompt(reader, "JIRA API token", None)?;
            answers.jira = Some(JiraSettings {
                url,
                username,
                token: Credential::literal(token),
            });
        }
        InstallPm::Linear => {
            let api_key = prompt(reader, "Linear API key", None)?;
            let teams = prompt_optional(
                reader,
                "Linear team keys (comma-separated, blank for every visible team)",
            )?;
            answers.linear = Some(LinearSettings {
                api_key: Credential::literal(api_key),
                team_keys: teams.as_deref().map(split_list).unwrap_or_default(),
            });
        }
        InstallPm::None | InstallPm::Github => {}
    }
    Ok(())
}

/// Prompt for one or more local repository paths, warning about missing ones.
fn prompt_paths<R: BufRead>(reader: &mut R) -> anyhow::Result<Vec<PathBuf>> {
    let raw = prompt(
        reader,
        "Path(s) to git repository (comma-separated for multiple)",
        None,
    )?;
    let paths: Vec<PathBuf> = split_list(&raw).into_iter().map(PathBuf::from).collect();
    if paths.is_empty() {
        anyhow::bail!("at least one repository path is required");
    }
    for p in &paths {
        if !p.exists() {
            eprintln!(
                "  warning: {} does not exist (continuing anyway — fix before running `tga analyze`)",
                p.display()
            );
        }
    }
    Ok(paths)
}

/// Split a comma-separated answer into trimmed, non-empty entries.
pub(super) fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Print `prompt` (with optional default), read a line, return it trimmed.
fn prompt<R: BufRead>(
    reader: &mut R,
    prompt: &str,
    default: Option<&str>,
) -> anyhow::Result<String> {
    let label = match default {
        Some(d) => format!("{prompt} [{d}]: "),
        None => format!("{prompt}: "),
    };
    let mut stdout = io::stdout();
    stdout.write_all(label.as_bytes())?;
    stdout.flush()?;
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        // EOF.
        if let Some(d) = default {
            return Ok(d.to_string());
        }
        anyhow::bail!("unexpected EOF while reading input for: {prompt}");
    }
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        if let Some(d) = default {
            return Ok(d.to_string());
        }
        anyhow::bail!("a value is required for: {prompt}");
    }
    Ok(trimmed)
}

/// Like [`prompt`], but returns `None` when the user submits an empty line.
fn prompt_optional<R: BufRead>(reader: &mut R, prompt: &str) -> anyhow::Result<Option<String>> {
    let label = format!("{prompt}: ");
    let mut stdout = io::stdout();
    stdout.write_all(label.as_bytes())?;
    stdout.flush()?;
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::install_flags::args_for_tests;

    /// Why: a flag the wizard would ignore must not silently select the wizard.
    /// What: an otherwise-bare `InstallArgs` plus one answer-bearing flag.
    /// Test: this test itself.
    #[test]
    fn answer_flags_select_the_flag_path() {
        assert!(!args_for_tests(&["tga"]).wants_non_interactive());
        assert!(args_for_tests(&["tga", "--non-interactive"]).wants_non_interactive());
        assert!(args_for_tests(&["tga", "--org", "acme"]).wants_non_interactive());
        assert!(args_for_tests(&["tga", "--repo-path", "/tmp/r"]).wants_non_interactive());
        assert!(args_for_tests(&["tga", "--linear-team", "ENG"]).wants_non_interactive());
        // Flags with clap defaults are not answers and must not flip the switch.
        assert!(!args_for_tests(&["tga", "--output", "x.yaml", "--force"]).wants_non_interactive());
    }

    /// Why (#5216): the wizard and the flag path must be two doors onto the
    /// same room. If they can render different config for the same answers,
    /// scripting `tga install` stops being equivalent to running it by hand.
    /// What: drives the wizard with a scripted stdin, drives the flag path with
    /// the equivalent flags, and compares the rendered YAML byte for byte.
    /// Test: this test itself.
    #[tokio::test]
    async fn wizard_and_flag_paths_render_identical_config() {
        // local host, one repo path, JIRA, ./out, no LLM.
        let script = "local\n\
                      /tmp/widget\n\
                      jira\n\
                      https://acme.atlassian.net\n\
                      me@acme.com\n\
                      jira-token\n\
                      ./out\n\
                      none\n";
        let mut input = script.as_bytes();
        let wizard_args = args_for_tests(&["tga"]);
        let wizard_answers = collect_answers(&mut input, &wizard_args).expect("wizard completes");
        let wizard_plan = plan_from_answers(wizard_answers).await.expect("plan");

        let flag_args = args_for_tests(&[
            "tga",
            "--host",
            "local",
            "--repo-path",
            "/tmp/widget",
            "--pm",
            "jira",
            "--jira-url",
            "https://acme.atlassian.net",
            "--jira-user",
            "me@acme.com",
            "--jira-token",
            "jira-token",
            "--output-dir",
            "./out",
        ]);
        let flag_plan = resolve_plan(&flag_args).await.expect("flag path resolves");

        assert_eq!(wizard_plan, flag_plan, "plans diverged");
        assert_eq!(
            render_yaml(&wizard_plan),
            render_yaml(&flag_plan),
            "rendered config diverged"
        );
    }

    /// Why: the wizard's Linear branch is new in #5216 and must reach the
    /// renderer, not just the prompt.
    /// What: drives the wizard through the Linear answers.
    /// Test: this test itself.
    #[tokio::test]
    async fn wizard_collects_linear_answers() {
        let script = "local\n/tmp/widget\nlinear\nlin_key\nENG, OPS\n./out\nnone\n";
        let mut input = script.as_bytes();
        let answers =
            collect_answers(&mut input, &args_for_tests(&["tga"])).expect("wizard completes");
        let yaml = render_yaml(&plan_from_answers(answers).await.expect("plan"));
        assert!(yaml.contains("linear:"), "{yaml}");
        assert!(yaml.contains("api_key: \"lin_key\""), "{yaml}");
        assert!(yaml.contains("team_keys: [\"ENG\", \"OPS\"]"), "{yaml}");
    }

    /// Why: the Bitbucket wizard branch must record the workspace and the
    /// explicit repository list rather than producing an empty config.
    /// What: drives the wizard through the Bitbucket answers.
    /// Test: this test itself.
    #[tokio::test]
    async fn wizard_collects_bitbucket_workspace_and_repos() {
        let script = "bitbucket\nacme\nacme/widget, acme/gadget\nbb-token\nnone\n./out\nnone\n";
        let mut input = script.as_bytes();
        let answers =
            collect_answers(&mut input, &args_for_tests(&["tga"])).expect("wizard completes");
        let plan = plan_from_answers(answers).await.expect("plan");
        let yaml = render_yaml(&plan);
        assert!(yaml.contains("workspace: \"acme\""), "{yaml}");
        assert!(yaml.contains("name: \"widget\""), "{yaml}");
        assert!(yaml.contains("name: \"gadget\""), "{yaml}");
        assert!(
            plan.notes.iter().any(|n| n.contains("#5220")),
            "{:?}",
            plan.notes
        );
    }

    /// Why: the two prompt helpers carry the default/EOF contract the rest of
    /// the wizard relies on.
    /// What: empty line on each.
    /// Test: this test itself.
    #[test]
    fn prompt_uses_default_on_empty() {
        let mut input: &[u8] = b"\n";
        let v = prompt(&mut input, "Q", Some("def")).expect("ok");
        assert_eq!(v, "def");

        let mut input: &[u8] = b"\n";
        let v = prompt_optional(&mut input, "Q").expect("ok");
        assert!(v.is_none());
    }
}
