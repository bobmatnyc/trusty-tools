//! `tga install` — interactive configuration wizard.
//!
//! Prompts the operator for the minimal set of values required to bootstrap
//! a working `config.yaml`. We deliberately keep the wizard dependency-free
//! (plain stdin) so the CLI stays small and the binary cross-compiles
//! cleanly to musl / Apple Silicon without optional terminal crates.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use clap::Args;
use tga::core::config::Config;

/// Arguments for `tga install`.
#[derive(Args, Debug)]
#[command(
    about = "Interactive configuration wizard for first-time setup.",
    long_about = "Walk through a series of prompts to generate a config.yaml bootstrapped\n\
with the minimal required fields (git repo paths, date range, optional API keys).\n\n\
Press <enter> at each prompt to accept the default shown in brackets.\n\
Optional credentials left blank are omitted from the generated file.\n\n\
The wizard does NOT need an existing database or config — it is designed to\n\
run on a fresh checkout before any other tga command.",
    after_help = "EXAMPLES:\n\
  # First-time setup (writes config.yaml in the current directory)\n\
  tga install\n\n\
  # Write config to a custom path\n\
  tga install --output /etc/tga/config.yaml\n\n\
  # Overwrite an existing config without prompting\n\
  tga install --force\n\n\
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
}

/// Run the interactive install wizard.
///
/// Reads from stdin and writes a YAML config to `args.output`. The wizard
/// is intentionally tolerant: every field has a sensible default and empty
/// optional credentials are simply omitted from the resulting YAML.
///
/// # Errors
///
/// Returns an error if stdin reads fail, if the output path is not
/// writable, or if `args.output` exists and `--force` was not supplied.
pub fn run(_config: Config, args: InstallArgs) -> anyhow::Result<()> {
    if args.output.exists() && !args.force {
        anyhow::bail!(
            "{} already exists. Re-run with --force to overwrite.",
            args.output.display()
        );
    }

    let stdin = io::stdin();
    let mut input = stdin.lock();

    println!("tga install — interactive configuration wizard");
    println!("Press <enter> to accept the default shown in [brackets].\n");

    let repos_raw = prompt(
        &mut input,
        "Path(s) to git repository (comma-separated for multiple)",
        None,
    )?;
    let repo_paths: Vec<PathBuf> = repos_raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();

    if repo_paths.is_empty() {
        anyhow::bail!("at least one repository path is required");
    }
    for p in &repo_paths {
        if !p.exists() {
            eprintln!(
                "  warning: {} does not exist (continuing anyway — fix before running `tga analyze`)",
                p.display()
            );
        }
    }

    let github_token = prompt_optional(&mut input, "GitHub token (optional, leave blank to skip)")?;

    let jira_url = prompt_optional(&mut input, "JIRA URL (optional, leave blank to skip)")?;
    let (jira_user, jira_token) = if jira_url.is_some() {
        let user = prompt_optional(&mut input, "JIRA username/email")?;
        let token = prompt_optional(&mut input, "JIRA API token")?;
        (user, token)
    } else {
        (None, None)
    };

    let output_dir = prompt(&mut input, "Output directory", Some("./tga-output"))?;
    let output_dir_path = PathBuf::from(&output_dir);
    if let Err(e) = std::fs::create_dir_all(&output_dir_path) {
        anyhow::bail!(
            "cannot create output directory {}: {e}",
            output_dir_path.display()
        );
    }

    let llm_provider = prompt(
        &mut input,
        "LLM provider — choose one: none / openai / openrouter",
        Some("none"),
    )?;
    let llm_provider = llm_provider.to_lowercase();
    let llm_api_key = if llm_provider == "openai" || llm_provider == "openrouter" {
        prompt_optional(
            &mut input,
            &format!(
                "{} API key (leave blank to set later via env var)",
                llm_provider
            ),
        )?
    } else {
        None
    };

    let yaml = render_yaml(&RenderYamlConfig {
        repos: &repo_paths,
        github_token: github_token.as_deref(),
        jira_url: jira_url.as_deref(),
        jira_user: jira_user.as_deref(),
        jira_token: jira_token.as_deref(),
        output_dir: &output_dir,
        llm_provider: &llm_provider,
        llm_api_key: llm_api_key.as_deref(),
    });

    std::fs::write(&args.output, yaml)?;
    println!(
        "\nConfig written to {}. Run: tga analyze --config {}",
        args.output.display(),
        args.output.display()
    );
    Ok(())
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

/// Inputs to [`render_yaml`].
///
/// Why: groups the wizard's collected values into a single named struct so
/// the renderer signature stays readable and call sites are self-documenting
/// (no long positional `Option<&str>` argument lists).
struct RenderYamlConfig<'a> {
    /// Repository paths to emit under `repositories:`.
    repos: &'a [PathBuf],
    /// Optional GitHub personal access token.
    github_token: Option<&'a str>,
    /// Optional JIRA base URL.
    jira_url: Option<&'a str>,
    /// Optional JIRA username / email.
    jira_user: Option<&'a str>,
    /// Optional JIRA API token.
    jira_token: Option<&'a str>,
    /// Output directory for generated reports.
    output_dir: &'a str,
    /// LLM provider identifier (`none`, `openai`, or `openrouter`).
    llm_provider: &'a str,
    /// Optional LLM API key (emitted only as a hint comment).
    llm_api_key: Option<&'a str>,
}

/// Render a YAML configuration string. Optional sections are omitted when
/// the corresponding credential set is empty so the generated file is
/// minimal and round-trips through `Config::load` cleanly.
fn render_yaml(cfg: &RenderYamlConfig<'_>) -> String {
    let RenderYamlConfig {
        repos,
        github_token,
        jira_url,
        jira_user,
        jira_token,
        output_dir,
        llm_provider,
        llm_api_key,
    } = *cfg;

    let mut out = String::new();
    out.push_str("# Generated by `tga install`\n");
    out.push_str("version: \"1.0\"\n\n");

    out.push_str("repositories:\n");
    for p in repos {
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("repo")
            .to_string();
        out.push_str(&format!("  - path: \"{}\"\n", p.display()));
        out.push_str(&format!("    name: \"{}\"\n", name));
    }
    out.push('\n');

    out.push_str("output:\n");
    out.push_str(&format!("  directory: \"{}\"\n", output_dir));
    out.push_str("  formats: [csv, json, markdown]\n\n");

    if let Some(token) = github_token {
        out.push_str("github:\n");
        out.push_str(&format!("  token: \"{}\"\n", token));
        out.push_str("  fetch_prs: true\n\n");
    }

    if let (Some(url), Some(user), Some(token)) = (jira_url, jira_user, jira_token) {
        out.push_str("jira:\n");
        out.push_str(&format!("  url: \"{}\"\n", url));
        out.push_str(&format!("  username: \"{}\"\n", user));
        out.push_str(&format!("  token: \"{}\"\n\n", token));
    }

    if llm_provider == "openai" || llm_provider == "openrouter" {
        out.push_str("classification:\n");
        out.push_str("  use_llm: true\n");
        out.push_str(&format!(
            "  llm_model: \"{}\"\n",
            default_model_for(llm_provider)
        ));
        if let Some(key) = llm_api_key {
            out.push_str(&format!(
                "  # API key (also pickable from ${} env var)\n",
                env_var_for(llm_provider)
            ));
            out.push_str(&format!("  # llm_api_key: \"{}\"\n", key));
        }
        out.push('\n');
    }

    out
}

/// Suggest a default model identifier for the chosen provider.
fn default_model_for(provider: &str) -> &'static str {
    match provider {
        "openai" => "gpt-4o-mini",
        "openrouter" => "openrouter/auto",
        _ => "",
    }
}

/// Suggest the conventional environment variable name for the provider's
/// API key. Surfaces in the generated YAML as a hint comment.
fn env_var_for(provider: &str) -> &'static str {
    match provider {
        "openai" => "OPENAI_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        _ => "LLM_API_KEY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn render_yaml_for_tests(repo: &Path, output_dir: &str) -> String {
        render_yaml(&RenderYamlConfig {
            repos: &[repo.to_path_buf()],
            github_token: None,
            jira_url: None,
            jira_user: None,
            jira_token: None,
            output_dir,
            llm_provider: "none",
            llm_api_key: None,
        })
    }

    #[test]
    fn render_yaml_minimal() {
        let yaml = render_yaml_for_tests(Path::new("/tmp/repo"), "./out");
        assert!(yaml.contains("repositories:"));
        assert!(yaml.contains("path: \"/tmp/repo\""));
        assert!(yaml.contains("output:"));
        assert!(yaml.contains("directory: \"./out\""));
        // No optional integrations included.
        assert!(!yaml.contains("github:"));
        assert!(!yaml.contains("jira:"));
        assert!(!yaml.contains("classification:"));
    }

    #[test]
    fn render_yaml_with_github_and_llm() {
        let yaml = render_yaml(&RenderYamlConfig {
            repos: &[PathBuf::from("/tmp/repo")],
            github_token: Some("ghp_xxx"),
            jira_url: None,
            jira_user: None,
            jira_token: None,
            output_dir: "./out",
            llm_provider: "openai",
            llm_api_key: Some("sk-xxx"),
        });
        assert!(yaml.contains("github:"));
        assert!(yaml.contains("ghp_xxx"));
        assert!(yaml.contains("classification:"));
        assert!(yaml.contains("use_llm: true"));
        assert!(yaml.contains("gpt-4o-mini"));
    }

    #[test]
    fn prompt_uses_default_on_empty() {
        let mut input: &[u8] = b"\n";
        let v = prompt(&mut input, "Q", Some("def")).expect("ok");
        assert_eq!(v, "def");
    }

    #[test]
    fn prompt_optional_returns_none_on_empty() {
        let mut input: &[u8] = b"\n";
        let v = prompt_optional(&mut input, "Q").expect("ok");
        assert!(v.is_none());
    }
}
