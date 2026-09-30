//! `--account` on the session-spawning verbs (#8914).
//!
//! Why: `--account` is a clap `global = true` flag, so `tm sessions new
//! --account X <path>` parsed it and then dropped it; the session ran as the
//! machine's active gh account. A spawn verb must either run as X or refuse.
//! What: [`pin_account_for_dir`] proves X for the checkout's origin
//! ([`trusty_mpm::core::gh_session_account::prepare_session_account`]), then
//! pins X and its per-account gh dir on every registry record for that origin.
//! Every spawn, resume and relaunch reads that pin, and the daemon builds the
//! session env from it. [`session_as_account`] applies it to `tm sessions
//! new`/`start` and refuses `--account` on a verb that spawns nothing.
//! Test: `session_account_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::project::Project;

use crate::cli::SessionAction;
use crate::commands::projects::registry::{RegisterInput, current_default_branch, register};

/// The directory a `tm sessions` verb spawns a session in, or `None` for a
/// verb that spawns nothing.
/// Test: `spawn_dir_names_the_new_and_start_checkouts`,
/// `spawn_dir_is_none_for_a_verb_that_spawns_nothing`.
pub(crate) fn spawn_dir(action: &SessionAction) -> anyhow::Result<Option<PathBuf>> {
    Ok(match action {
        SessionAction::New { repo, .. } => Some(PathBuf::from(repo)),
        SessionAction::Start { dir } => Some(crate::commands::project::resolve_dir(dir.clone())?),
        _ => None,
    })
}

/// `tm sessions <verb>` with the global `--account` applied (#8914).
///
/// What: no account → the plain [`super::session::session`] dispatch. With an
/// account, a spawn verb pins it first ([`pin_account_for_dir`]) and any other
/// verb is refused, so the flag is never silently dropped.
/// Test: `spawn_dir_is_none_for_a_verb_that_spawns_nothing` covers the
/// decision; the pin is [`pin_account_for_dir`]'s.
pub(crate) async fn session_as_account(
    client: &reqwest::Client,
    url: &str,
    action: SessionAction,
    account: Option<&str>,
    account_token: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(login) = account {
        let Some(dir) = spawn_dir(&action)? else {
            anyhow::bail!(
                "--account {login} applies to `tm sessions new` and `tm sessions start`; this \
                 verb spawns no session. A resumed session keeps the account its project pins."
            );
        };
        pin_account_for_dir(client, url, &dir, login, account_token).await?;
    }
    super::session::session(client, url, action).await
}

/// Prove `login` for the checkout at `dir`, then pin it on the project (#8914).
///
/// Why: the pin is what every later spawn, resume and relaunch of a session in
/// this checkout reads, and it must name a proven account before any of them
/// runs. A failure at any step refuses the command.
/// What: `dir` must be an existing directory with an `origin` remote. Then
/// `prepare_session_account` (blocking, off the executor; `account_token` is
/// the `--account-token-stdin` token), then every registry record whose
/// `repo_url` matches the origin — or one new record named from it — is
/// upserted with `gh_account = login` and `github.config_dir` = the
/// per-account dir. A record left pinning another account fails every session
/// for the origin closed, so all are updated. Each change is printed as
/// [`pin_notice`], then [`transport_notice`].
/// Test: the pure steps are `pin_targets_updates_every_matching_record`,
/// `pin_targets_names_a_new_record_from_the_origin`,
/// `pin_notice_names_the_account_it_replaces`,
/// `transport_notice_says_when_git_uses_ssh`; the rest is HTTP and `gh`.
pub(crate) async fn pin_account_for_dir(
    client: &reqwest::Client,
    url: &str,
    dir: &Path,
    login: &str,
    account_token: Option<&str>,
) -> anyhow::Result<()> {
    if !dir.is_dir() {
        anyhow::bail!(
            "--account {login}: '{}' is not an existing local directory",
            dir.display()
        );
    }
    let origin = trusty_mpm::daemon::managed_routes::inproject::get_origin_url(dir)
        .map_err(|e| anyhow::anyhow!("--account {login}: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "--account {login}: '{}' has no git origin remote to pin the account on",
                dir.display()
            )
        })?;
    let (who, from) = (login.to_string(), origin.clone());
    let token = account_token.map(str::to_string);
    let config_dir = tokio::task::spawn_blocking(move || {
        trusty_mpm::core::gh_session_account::prepare_session_account(&who, &from, token.as_deref())
    })
    .await?
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let projects = trusty_mpm::client::DaemonClient::with_client(client.clone(), url.to_string())
        .registry_list_projects(None)
        .await?;
    for (name, repo_url) in pin_targets(&projects, &origin)? {
        let previous = projects
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.gh_account.as_deref());
        let notice = pin_notice(&name, previous, login);
        let default_branch = current_default_branch(client, url, &name).await;
        register(
            client,
            url,
            RegisterInput {
                default_branch,
                gh_account: Some(login.to_string()),
                gh_config_dir: Some(config_dir.clone()),
                ..RegisterInput::new(name, repo_url)
            },
        )
        .await?;
        eprintln!("{notice}");
    }
    eprintln!("{}", transport_notice(&origin, login));
    Ok(())
}

/// The line naming a project-wide pin change (#8914 MEDIUM).
///
/// Why: `--account` re-pins the whole project, so every later session of it
/// runs as `login`, not only this one.
/// What: `tm: every session of project '<name>' now runs as gh account
/// '<login>' (was '<previous>')`, or `(was unpinned)`.
/// Test: `pin_notice_names_the_account_it_replaces`.
pub(crate) fn pin_notice(name: &str, previous: Option<&str>, login: &str) -> String {
    let was = previous
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map_or_else(|| "unpinned".to_string(), |p| format!("'{p}'"));
    format!("tm: every session of project '{name}' now runs as gh account '{login}' (was {was})")
}

/// What `--account` does and does not pin for `origin`'s git transport
/// (#8914 MEDIUM).
///
/// Why: the pin covers gh and HTTPS git. An SSH origin authenticates with the
/// operator's SSH key, which tm does not select.
/// What: an `ssh://` or scp-style (`user@host:path`) origin gets a line naming
/// the SSH key as unpinned; any other origin gets the HTTPS line.
/// Test: `transport_notice_says_when_git_uses_ssh`.
pub(crate) fn transport_notice(origin: &str, login: &str) -> String {
    let scp_style = !origin.contains("://") && origin.contains('@') && origin.contains(':');
    if origin.starts_with("ssh://") || scp_style {
        format!(
            "tm: gh runs as '{login}'. git uses SSH for {origin}, so fetch and push authenticate \
             with your SSH key, which tm does not pin to '{login}'."
        )
    } else {
        format!("tm: this session runs gh and HTTPS git as '{login}'")
    }
}

/// The token `--account-token-stdin` supplies, read from `reader` (#8914).
///
/// What: at most 64 KiB, trimmed; refuses an empty token and one with
/// whitespace inside it (two tokens, or a pasted sentence).
/// Test: `read_stdin_token_trims_one_token`,
/// `read_stdin_token_refuses_empty_or_spaced_input`.
pub(crate) fn read_stdin_token(reader: impl std::io::Read) -> anyhow::Result<String> {
    use std::io::Read as _;
    let mut text = String::new();
    reader
        .take(64 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| anyhow::anyhow!("--account-token-stdin: cannot read stdin ({e})"))?;
    let token = text.trim();
    if token.is_empty() || token.contains(char::is_whitespace) {
        anyhow::bail!("--account-token-stdin: stdin must hold exactly one token");
    }
    Ok(token.to_string())
}

/// The `(name, repo_url)` records to pin for `origin`: every registered record
/// whose `repo_url` matches it, else one new record named from it.
/// Test: `pin_targets_updates_every_matching_record`,
/// `pin_targets_names_a_new_record_from_the_origin`.
pub(crate) fn pin_targets(
    projects: &[Project],
    origin: &str,
) -> anyhow::Result<Vec<(String, String)>> {
    let matching: Vec<(String, String)> = projects
        .iter()
        .filter(|p| trusty_mpm::project::record::repo_url_matches(&p.repo_url, origin))
        .map(|p| (p.name.clone(), p.repo_url.clone()))
        .collect();
    if !matching.is_empty() {
        return Ok(matching);
    }
    let name = trusty_mpm::project::derive_name_from_url(origin)
        .ok_or_else(|| anyhow::anyhow!("cannot name a project for origin '{origin}'"))?;
    Ok(vec![(name, origin.to_string())])
}

#[cfg(test)]
#[path = "session_account_tests.rs"]
mod tests;
