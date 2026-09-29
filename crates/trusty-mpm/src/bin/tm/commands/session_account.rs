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
) -> anyhow::Result<()> {
    if let Some(login) = account {
        let Some(dir) = spawn_dir(&action)? else {
            anyhow::bail!(
                "--account {login} applies to `tm sessions new` and `tm sessions start`; this \
                 verb spawns no session. A resumed session keeps the account its project pins."
            );
        };
        pin_account_for_dir(client, url, &dir, login).await?;
    }
    super::session::session(client, url, action).await
}

/// Prove `login` for the checkout at `dir`, then pin it on the project (#8914).
///
/// Why: the pin is what every later spawn, resume and relaunch of a session in
/// this checkout reads, and it must name a proven account before any of them
/// runs. A failure at any step refuses the command.
/// What: `dir` must be an existing directory with an `origin` remote. Then
/// `prepare_session_account` (blocking, off the executor), then every
/// registry record whose `repo_url` matches the origin — or one new record
/// named from it — is upserted with `gh_account = login` and
/// `github.config_dir` = the per-account dir. A second record left pinning
/// another account would make the daemon drop both pins, so all are updated.
/// Test: the pure step is `pin_targets_*`; the rest is HTTP and `gh`.
pub(crate) async fn pin_account_for_dir(
    client: &reqwest::Client,
    url: &str,
    dir: &Path,
    login: &str,
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
    let config_dir = tokio::task::spawn_blocking(move || {
        trusty_mpm::core::gh_session_account::prepare_session_account(&who, &from)
    })
    .await?
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let projects = trusty_mpm::client::DaemonClient::with_client(client.clone(), url.to_string())
        .registry_list_projects(None)
        .await?;
    for (name, repo_url) in pin_targets(&projects, &origin)? {
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
    }
    eprintln!("tm: this session runs gh and HTTPS git as '{login}'");
    Ok(())
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
