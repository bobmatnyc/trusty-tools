//! Clone or refresh a managed project workspace for a registered alias.
//!
//! Why: `tm load <alias>` is the idempotent entry point that turns a registered
//! name into a ready-to-drive, isolated project directory (DOC-24
//! SPEC-STANDALONE-MPM-02). Making it idempotent (clone-once, refresh-many)
//! means `run` can call it unconditionally and it also serves as the
//! "bring this project up to date" verb.
//! What: [`load_alias`] resolves the alias from the registry, clones (or
//! fast-forward-pulls) the repo into
//! `<managed_root>/projects/<alias>/repo/`, runs `prepare_session` from the
//! session-launch core, writes `.trusty-mpm/managed.toml`, and returns the
//! absolute path to `repo/`.
//! Test: unit tests for the marker-file write logic; the clone path against a
//! local bare repo in `load_alias_strips_a_stored_token_before_clone_and_marker`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use trusty_common::url_userinfo::{ends_url, strip_url_secret, userinfo_end};

use super::registry::ManagedRegistry;
use crate::core::remote_url_redact::{authority_userinfo_end, is_git_scheme};

/// The marker file written into every managed project.
///
/// Why: the marker lets `path`, `ls`, `rm`, and `update` know a directory is
/// tm-managed and carries the metadata needed to replay a launch deterministically
/// (DOC-24 SPEC-STANDALONE-MPM-03d).
/// What: a TOML struct at `.trusty-mpm/managed.toml`.
/// Test: `test_marker_write_round_trip`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ManagedMarker {
    /// The alias this project was cloned from.
    pub alias: String,
    /// The clone URL.
    pub url: String,
    /// Sentinel for the branch/ref intent (currently always `"default"`, meaning
    /// the remote's default branch was cloned without `-b`; branch selection is
    /// a future enhancement — do not interpret this as a literal Git ref name).
    pub git_ref: String,
    /// Absolute path to the tm-global CLAUDE_CONFIG_DIR.
    pub claude_config_dir: String,
}

/// Load (clone or refresh) the managed workspace for `alias`.
///
/// Why: the idempotent load verb is the load-bearing primitive behind `run`;
/// separating it lets callers (`tm load`, `tm run`) both call it without
/// duplicating the clone/prepare logic.
/// What:
/// 1. Looks up the alias in the registry under `managed_root`.
/// 2. Derives `<managed_root>/projects/<alias>/repo/` as the checkout dir.
/// 3. If `repo/` doesn't exist: clones via `git clone --depth 1 <url> repo/`.
/// 4. If `repo/` exists: `git -C repo/ pull --ff-only` (best-effort, non-fatal).
/// 5. Runs `prepare_session` from `crate::core::session_launch` on `repo/`.
/// 6. Writes `.trusty-mpm/managed.toml`.
/// 7. (WI-3) Pre-seeds project trust + MCP-server approval into
///    `<claude_config_dir>/.claude.json` via [`super::trust_seed::preseed_managed_trust`].
/// 8. Returns the absolute `PathBuf` to `repo/`.
///
/// The trust seed (step 7) writes ONLY into `<claude_config_dir>` — never to
/// `~/.claude.json` or `~/.claude/` (isolation invariant, WI-7).
///
/// #9227: an entry `tm register` stored before #9124 can still embed a token
/// in its URL. The clone, the marker and `prepare_session` get [`clone_url`]'s
/// stripped form, so no new clone records the token; a URL whose secret cannot
/// be stripped is refused before anything is cloned or refreshed, so an
/// existing clone is blocked too until the alias is re-registered.
/// `registry.json` itself is not rewritten, and a clone made before this fix
/// keeps its `.git/config`.
///
/// Test: `load_alias_strips_a_stored_token_before_clone_and_marker`,
/// `load_alias_refuses_a_url_whose_token_cannot_be_stripped`,
/// `test_marker_write_round_trip`.
pub fn load_alias(
    alias: &str,
    managed_root: &Path,
    claude_config_dir: &Path,
) -> anyhow::Result<PathBuf> {
    load_alias_with_git_env(alias, managed_root, claude_config_dir, &[])
}

/// [`load_alias`] with extra environment for the `git clone` child.
///
/// Why: a test points the clone at a sandboxed git config and HOME without
/// mutating the test process's environment.
/// What: the whole of [`load_alias`]; `git_env` is set on the clone `Command`.
/// Test: `load_alias_strips_a_stored_token_before_clone_and_marker`.
fn load_alias_with_git_env(
    alias: &str,
    managed_root: &Path,
    claude_config_dir: &Path,
    git_env: &[(&str, &OsStr)],
) -> anyhow::Result<PathBuf> {
    let registry = ManagedRegistry::load(managed_root)
        .with_context(|| format!("failed to load registry from {}", managed_root.display()))?;
    let entry = registry
        .get(alias)
        .with_context(|| format!("alias '{alias}' is not registered"))?;
    // #9227: strip once, before any use; the raw stored URL is not used again.
    let url = clone_url(alias, &entry.url)?;
    let git_ref = entry.git_ref.clone();

    let project_dir = managed_root.join("projects").join(alias);
    let repo_dir = project_dir.join("repo");

    if !repo_dir.exists() {
        clone_repo(&url, &project_dir, git_env)?;
    } else {
        pull_ff_only(&repo_dir);
    }

    // Issue #1651: thread the registry clone `url` as the authoritative git
    // remote so the per-project `repo/.mcp.json` pins `TRUSTY_MEMORY_PALACE` to
    // the repo's canonical `owner-repo` slug (the same `derive_palace_id`
    // mechanism #1605 uses for the per-project injection). The clone URL is more
    // authoritative than probing the checkout's own origin remote (which a
    // shallow / renamed / origin-less clone might lack); the operator
    // `TRUSTY_MEMORY_PALACE` override still wins over it per `derive_palace_id`
    // precedence.
    run_prepare_session(&repo_dir, Some(&url), managed_root)?;

    write_marker(&repo_dir, alias, &url, &git_ref, claude_config_dir)?;

    // WI-3 sub-part 2: pre-seed project trust into
    // <claude_config_dir>/.claude.json so managed sessions start without the
    // trust dialog. // #4181: no MCP approval is written any more.
    // Writes ONLY to <claude_config_dir>/.claude.json — never to ~/.claude.json.
    // Non-fatal: a seed failure only means the operator may see the trust dialog.
    if let Err(err) = super::trust_seed::preseed_managed_trust(claude_config_dir, &repo_dir) {
        tracing::warn!("failed to pre-seed managed trust for '{alias}': {err}");
    }

    Ok(repo_dir)
}

/// The registry URL with its secret removed; an error when one may remain.
///
/// Why (#9227): a URL stored before #9124 can embed a token, and `git clone`
/// writes the URL it is given into `.git/config`. `strip_url_secret` returns a
/// URL it cannot parse unchanged, so its output is checked again.
/// What: applies [`strip_url_secret`], then refuses the result when the
/// over-reading [`authority_userinfo_end`] or the scp-style boundary still
/// finds userinfo that is on an `http(s)` scheme, holds a `:password`, or
/// holds a quote or whitespace. Over-reading fails closed:
/// `https://host:8080/@scope/pkg` is refused too. The error names only the
/// alias, because `redact_url` misses a password that holds a quote.
/// Test: `clone_url_strips_or_refuses`,
/// `clone_url_refusal_never_echoes_a_quoted_password`,
/// `load_alias_refuses_a_url_whose_token_cannot_be_stripped`.
fn clone_url(alias: &str, stored: &str) -> anyhow::Result<String> {
    let url = strip_url_secret(stored);
    if may_carry_secret(&url) {
        anyhow::bail!(
            "refusing to load '{alias}': its registered URL may carry a \
             credential that cannot be stripped; re-register it with \
             `tm register --force <url-without-credentials> {alias}`"
        );
    }
    Ok(url.into_owned())
}

/// Whether `url` still has userinfo on `http(s)`, or userinfo holding a `:`,
/// a quote or whitespace on any scheme, as the over-reading boundary finds it.
fn may_carry_secret(url: &str) -> bool {
    let (userinfo, http) = match url.find("://").filter(|&at| is_git_scheme(&url[..at])) {
        Some(at) => {
            let tail = &url[at + 3..];
            let cut = authority_userinfo_end(tail);
            debug_assert!(userinfo_end(tail) <= cut, "stored cut ends early");
            let Some(cut) = cut else {
                return false;
            };
            // #9227: a quote or space here is where `userinfo_end` stops early.
            if tail[..cut].contains(ends_url) {
                return true;
            }
            let scheme = url[..at].rsplit('+').next().unwrap_or_default();
            let http = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
            (&tail[..cut], http)
        }
        // #9227: git reads `u:T@h:o/r://x` as scp-style; so does this arm.
        None => {
            let head = &url[..url.find('/').unwrap_or(url.len())];
            match head.rfind('@').filter(|&at| head[at..].contains(':')) {
                Some(at) => (&url[..at], false),
                None => return false,
            }
        }
    };
    http || userinfo.contains(':')
}

/// What an error may print of a URL [`may_carry_secret`] admitted:
/// `scheme://host[:port]`, an scp-style URL's host, or `a local path`.
///
/// Why (#9227): `redact_url` reads its input as free text, so a quote hides a
/// query token from it; no path, query or userinfo is printed here.
/// Test: `clone_origin_prints_scheme_and_host_only`.
fn clone_origin(url: &str) -> String {
    debug_assert!(!may_carry_secret(url), "clone_origin of a refused URL");
    if let Some(at) = url.find("://").filter(|&at| is_git_scheme(&url[..at])) {
        let tail = &url[at + 3..];
        let authority = &tail[..tail.find(['/', '?', '#']).unwrap_or(tail.len())];
        let host = authority.rsplit('@').next().unwrap_or_default();
        return format!("{}://{host}", &url[..at]);
    }
    let head = &url[..url.find('/').unwrap_or(url.len())];
    match head.rsplit('@').next().unwrap_or_default().split_once(':') {
        Some((host, _)) => host.to_owned(),
        None => "a local path".to_owned(),
    }
}

/// Clone the repository into `<project_dir>/repo/`.
///
/// Why: `git clone` is the authoritative way to get a fresh checkout;
/// shelling out avoids a heavy libgit2 dependency.
/// What: runs `git clone --depth 1 <url> repo/` in `project_dir`, creating
/// the directory first; `git_env` is added to the child's environment.
/// Test: `load_alias_strips_a_stored_token_before_clone_and_marker`.
fn clone_repo(url: &str, project_dir: &Path, git_env: &[(&str, &OsStr)]) -> anyhow::Result<()> {
    std::fs::create_dir_all(project_dir).with_context(|| {
        format!(
            "failed to create project directory {}",
            project_dir.display()
        )
    })?;
    let status = Command::new("git")
        .args(["clone", "--depth", "1", url, "repo"])
        .envs(git_env.iter().copied())
        .current_dir(project_dir)
        .status()
        .context("failed to spawn git clone")?;
    if !status.success() {
        // #9124, #9227: print the scheme and host only.
        anyhow::bail!("git clone failed for '{}'", clone_origin(url));
    }
    Ok(())
}

/// Attempt a fast-forward pull; non-fatal on any failure.
///
/// Why: idempotent `load` should refresh an existing checkout but must never
/// destroy user edits — fast-forward only is the safest pull strategy.
/// What: runs `git -C <repo_dir> pull --ff-only`; silently ignores failures
/// (dirty tree, network unavailable) so a load with local modifications still
/// succeeds.
/// Test: integration-only.
fn pull_ff_only(repo_dir: &Path) {
    let result = Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(repo_dir)
        .status();
    if let Ok(status) = result
        && !status.success()
    {
        eprintln!(
            "warning: git pull --ff-only failed in {}; skipping refresh",
            repo_dir.display()
        );
    }
}

/// Run `prepare_session` on the given repo directory, pinning the palace to the
/// cloned-from `repo_url` (issue #1651).
///
/// Why: `prepare_session` deploys composed agents, skills, and CLAUDE.md so the
/// project-local half of the managed configuration is complete. The registry
/// clone URL is threaded through as the authoritative git remote because a
/// managed checkout's own `git remote get-url origin` is not always the repo the
/// session belongs to. Since ADR-0042 deleted the `.mcp.json` injectors, the
/// remote no longer feeds a pin written into the workspace: the palace is
/// exported at spawn by [`crate::core::mcp_session_env::session_mcp_env`], and
/// the remote's remaining job here is the #1939 alias healing
/// ([`crate::core::session_launch::maybe_register_palace_alias`]), which uses it
/// to decide whether the derived `owner-repo` palace should resolve to a
/// pre-existing bare-repo one. `None` falls back to probing the checkout.
///
/// Issue #1927 (DOC-24 SPEC-STANDALONE-MPM-04): this previously resolved
/// `FrameworkPaths::default()`, which always deploys composed agents/skills
/// under the REAL `$HOME/.claude/{agents,skills}` — a direct isolation-invariant
/// violation for the "fully isolated" standalone driver. `load_alias` already
/// carries `managed_root` (the shared framework install, e.g. `~/.trusty-mpm` or
/// an operator override), so it is threaded here and combined with `repo_dir`
/// via `FrameworkPaths::for_managed_project`, which keeps every framework SOURCE
/// path (agent/skill templates, hooks, instructions, catalog root) resolving
/// from `managed_root` exactly as before, while re-targeting ONLY the deploy
/// destination to the project-local `repo_dir/.claude/{agents,skills}` tree
/// (SPEC-STANDALONE-MPM-04 item 1). This is intentionally distinct from — and
/// not a redundant re-deploy of — the tm-global `<managed_root>/claude-config/`
/// deploy that `core::standalone::global_config::ensure_global_config_dir`
/// performs separately (and earlier) in every CLI command handler that calls
/// `load_alias`: the two writes target two different trees that Claude Code
/// merges (tm-global ⊕ project-local), matching the DOC-24 layering model.
/// What: resolves `FrameworkPaths::for_managed_project(managed_root, repo_dir)`
/// and calls `crate::core::session_launch::prepare_session_with_repo_url`,
/// forwarding `repo_url` to the trusty-memory MCP palace-slug derivation.
// #4181: this used to return the four per-run `.mcp.json` pin results so the
/// caller could gate `enabledMcpjsonServers` on them. Both the injectors and the
/// approval are gone (ADR-0042), so it returns nothing but success or failure.
/// Test: `run_prepare_session_never_writes_real_home_claude_dirs`;
/// `prepare_session` itself is covered in session_launch/tests.rs, and the
/// remote-driven alias healing in `session_launch::palace_alias`'s own tests
/// plus `ensure_managed_config_dir_heals_a_bare_repo_palace_alias`.
fn run_prepare_session(
    repo_dir: &Path,
    repo_url: Option<&str>,
    managed_root: &Path,
) -> anyhow::Result<()> {
    let fw = crate::core::paths::FrameworkPaths::for_managed_project(managed_root, repo_dir);
    let report =
        crate::core::session_launch::prepare_session_with_repo_url(&fw, repo_dir, repo_url)
            .map_err(|e| anyhow::anyhow!("prepare_session failed: {e}"))?;
    // Issue #2149: a roster-deploy failure no longer aborts preparation —
    // surface it loudly rather than let it hide behind a silent `Ok(())`.
    // #6649 folded the asset-hygiene lines in beside them, through the one
    // shared reporter.
    crate::core::session_launch::log_prep_findings(
        &report.roster_errors,
        &report.asset_notices,
        crate::core::session_launch::PrepScope {
            kind: "load",
            session: None,
            dir: repo_dir,
        },
    );
    Ok(())
}

/// Write `.trusty-mpm/managed.toml` into the repo directory.
///
/// Why: the marker lets every other lifecycle verb (`path`, `ls`, `rm`) detect
/// a managed directory and read the metadata needed to replay a launch.
/// What: creates `.trusty-mpm/` if absent, serializes [`ManagedMarker`] to
/// TOML, and writes `managed.toml`.
/// Test: `test_marker_write_round_trip`.
fn write_marker(
    repo_dir: &Path,
    alias: &str,
    url: &str,
    git_ref: &str,
    claude_config_dir: &Path,
) -> anyhow::Result<()> {
    let dot_dir = repo_dir.join(".trusty-mpm");
    std::fs::create_dir_all(&dot_dir)
        .with_context(|| format!("failed to create {}", dot_dir.display()))?;
    let marker = ManagedMarker {
        alias: alias.to_string(),
        url: url.to_string(),
        git_ref: git_ref.to_string(),
        claude_config_dir: claude_config_dir.to_string_lossy().to_string(),
    };
    let toml = toml::to_string_pretty(&marker).context("failed to serialize managed.toml")?;
    std::fs::write(dot_dir.join("managed.toml"), toml)
        .context("failed to write .trusty-mpm/managed.toml")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::credentials::test_sandbox::CredentialSandbox;

    /// RAII guard that clears an env var for the duration of a test and restores
    /// the prior value on drop.
    ///
    /// Why: the isolation test below runs the whole `prepare_session` pipeline,
    /// which reaches palace derivation; an ambient `TRUSTY_MEMORY_PALACE`
    /// override would make that step non-hermetic.
    /// What: snapshots the prior value, removes the var, and restores it on drop.
    /// Pairs with `#[serial_test::serial]` so concurrent tests never race on the
    /// shared process environment.
    /// Test: used by `run_prepare_session_never_writes_real_home_claude_dirs`.
    struct EnvClearGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvClearGuard {
        fn clear(key: &'static str) -> Self {
            let prior = std::env::var(key).ok();
            // SAFETY: tests run serially under `#[serial_test::serial]`, so no
            // other thread reads/writes the environment concurrently.
            unsafe { std::env::remove_var(key) };
            Self { key, prior }
        }
    }

    impl Drop for EnvClearGuard {
        fn drop(&mut self) {
            // SAFETY: see `clear`.
            unsafe {
                match self.prior.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    /// RAII guard restoring `$HOME` on drop (including panic) — mirrors the
    /// identical pattern in `standalone::global_config::tests::test_mcp_config_review_no_home_write`.
    ///
    /// Why: the isolation regression test below must point the process at a
    /// throwaway `$HOME` so a bug (`FrameworkPaths::default()`) would write to
    /// that FAKE home, not the developer's real `~/.claude`; the guard makes
    /// this safe under panics and keeps `$HOME` correct for tests that run
    /// after this one.
    /// What: snapshots the prior `$HOME` value and restores it on drop.
    /// Test: used by `run_prepare_session_never_writes_real_home_claude_dirs`.
    struct HomeGuard(Option<String>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: paired with `#[serial_test::serial]` below — no other
            // thread reads/writes the environment concurrently.
            match self.0 {
                Some(ref p) => unsafe { std::env::set_var("HOME", p) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    /// Why (issue #1927, DOC-24 SPEC-STANDALONE-MPM-04): before this fix,
    /// `run_prepare_session` built `FrameworkPaths::default()`, which resolves
    /// `claude_agents`/`claude_skills` under the REAL `$HOME/.claude` — so the
    /// "fully isolated" standalone driver silently deployed agents and skills
    /// into the user's real global `~/.claude/agents` and `~/.claude/skills`,
    /// directly violating the isolation invariant. This test points `$HOME` at
    /// a throwaway tempdir (so a regression would be caught, not accidentally
    /// masked by writing to the real developer home) and asserts the fake
    /// home's `.claude/agents`/`.claude/skills` stay completely absent, while
    /// the project-local `repo/.claude/agents`/`repo/.claude/skills` receive
    /// the deploy instead.
    /// What: seeds a minimal agent source file under
    /// `<fake_home>/.trusty-mpm/framework/agents/` (the SAME tree
    /// `managed_root` resolves to in production, since `~/.trusty-mpm` is the
    /// default managed root — no skill source is seeded because
    /// `core::skill_source::ensure_skill_source_fresh`, part of the
    /// `prepare_session` pipeline, self-heals `framework/skills/` from the
    /// compiled-in bundle unconditionally, overwriting any hand-seeded file),
    /// runs `run_prepare_session` against a `repo/` under a SEPARATE temp dir,
    /// then asserts (a) `$HOME/.claude/agents` and `$HOME/.claude/skills` do
    /// not exist, (b) the agent landed in the tm-managed `CLAUDE_CONFIG_DIR`
    /// tier and NOT in `repo/.claude/agents` (issue #4409 moved the bundled
    /// agent destination off the workspace), and (c) `repo/.claude/skills/`
    /// was populated with at least one deployed skill — skills are still
    /// project-local.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn run_prepare_session_never_writes_real_home_claude_dirs() {
        let _palace_guard = EnvClearGuard::clear("TRUSTY_MEMORY_PALACE");

        let fake_home = crate::test_support::hermetic_temp_dir();
        let _home_guard = {
            let prior = std::env::var("HOME").ok();
            // SAFETY: serialized via `#[serial_test::serial]`.
            unsafe { std::env::set_var("HOME", fake_home.path()) };
            HomeGuard(prior)
        };

        // managed_root mirrors the real default (`~/.trusty-mpm`) under the
        // fake home, so this test exercises the exact production shape.
        let managed_root = fake_home.path().join(".trusty-mpm");
        let agents_src = managed_root.join("framework").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::write(
            agents_src.join("regression-agent.md"),
            "---\nname: regression-agent\nrole: engineer\n---\n\n\
             # Regression Agent\n\nAgent content.\n",
        )
        .unwrap();

        // repo/ lives under a SEPARATE temp dir so it can never coincidentally
        // land inside the fake home tree.
        let project_root = crate::test_support::hermetic_temp_dir();
        let repo = project_root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        run_prepare_session(&repo, None, &managed_root).expect("run_prepare_session succeeds");

        assert!(
            !fake_home.path().join(".claude").join("agents").exists(),
            "run_prepare_session must NOT write to the real $HOME/.claude/agents (issue #1927)"
        );
        assert!(
            !fake_home.path().join(".claude").join("skills").exists(),
            "run_prepare_session must NOT write to the real $HOME/.claude/skills (issue #1927)"
        );

        // Issue #4409: the bundled agent must land in the tm-managed
        // `CLAUDE_CONFIG_DIR` tier, never in the workspace. A copy under
        // `repo/.claude/agents` would shadow the canonical roster.
        let agent_tier =
            crate::core::paths::FrameworkPaths::for_managed_project(&managed_root, &repo)
                .agent_deploy_dir();
        assert!(
            agent_tier.join("regression-agent.md").exists(),
            "run_prepare_session must deploy agents to the tm-managed config tier ({}) — #4409",
            agent_tier.display()
        );
        assert!(
            !repo.join(".claude").join("agents").exists(),
            "run_prepare_session must NOT deploy bundled agents into the workspace (#4409)"
        );
        let deployed_skills_dir = repo.join(".claude").join("skills");
        let has_deployed_skill = deployed_skills_dir
            .read_dir()
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false);
        assert!(
            has_deployed_skill,
            "run_prepare_session must deploy the compiled-in skill bundle to \
             repo/.claude/skills (project-local, DOC-24); dir: {}",
            deployed_skills_dir.display()
        );
    }

    #[test]
    fn test_marker_write_round_trip() {
        let tmp = crate::test_support::hermetic_temp_dir();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let cfg = tmp.path().join("claude-config");

        write_marker(
            &repo,
            "my-alias",
            "https://github.com/org/r",
            "default",
            &cfg,
        )
        .unwrap();

        let toml_path = repo.join(".trusty-mpm").join("managed.toml");
        assert!(toml_path.exists());
        let text = std::fs::read_to_string(&toml_path).unwrap();
        let marker: ManagedMarker = toml::from_str(&text).unwrap();
        assert_eq!(marker.alias, "my-alias");
        assert_eq!(marker.url, "https://github.com/org/r");
        assert_eq!(
            marker.git_ref, "default",
            "git_ref must be the 'default' sentinel, not a literal branch name"
        );
    }

    /// #9227: a stripped URL loads; one with an unstrippable secret is refused.
    #[test]
    fn clone_url_strips_or_refuses() {
        let wrong = [
            ("https://u:T@github.com/o/r", Some("https://github.com/o/r")),
            ("git@github.com:o/r.git", Some("git@github.com:o/r.git")),
            ("ssh://git@h:2222/o/r", Some("ssh://git@h:2222/o/r")),
            ("u:T@host:o/r", Some("u@host:o/r")),
            ("/srv/dir@x/repo", Some("/srv/dir@x/repo")),
            ("https://u:/T@host/o/r", None),
            ("https://u:1234/T@host/o/r", None),
            // Fail closed: the `@` after a port cannot be told from a password.
            ("https://host:8080/@scope/pkg", None),
            // A quote or space must not end a stored URL's authority.
            ("https://u:pa'ss@host/o/r", None),
            ("https://u:pa ss@host/o/r", None),
            ("https://u:pa\"ss@host/o/r", None),
            ("https://u:pa`ss@host/o/r", None),
            ("https://us'er:T@host/o/r", None),
            ("https://u:pa'ss/x@host/o/r", None),
            ("ssh://git@h'x:pw@host/r", None),
            ("ssh://gi t@host/r", None),
            // Not a URL to git: an invalid scheme makes it scp-style `u:T@h:…`.
            ("u:T@h:o/r://x", None),
        ]
        .into_iter()
        .enumerate()
        .filter(|(_, (stored, want))| clone_url("a", stored).ok().as_deref() != *want)
        .map(|(row, _)| row)
        .collect::<Vec<_>>();
        assert!(wrong.is_empty(), "rows {wrong:?} gave the wrong verdict");
    }

    /// #9227: a clone-failure error prints no path, query or userinfo.
    #[test]
    fn clone_origin_prints_scheme_and_host_only() {
        for (url, want) in [
            ("https://h:8443/o/r?token=a'SECRET", "https://h:8443"),
            ("ssh://git@h:22/o/r", "ssh://h:22"),
            ("git@github.com:o/r.git", "github.com"),
            ("/srv/dir@x/repo", "a local path"),
        ] {
            assert_eq!(clone_origin(url), want, "{url:?}");
        }
    }

    /// #9227: the refusal names the alias, never any part of the userinfo.
    #[test]
    fn clone_url_refusal_never_echoes_a_quoted_password() {
        let Err(err) = clone_url("q9227", "https://u:pa'ss@host/o/r") else {
            panic!("a quoted password was admitted");
        };
        let text = format!("{err:#} {err:?}");
        assert!(
            text.contains("q9227"),
            "the refusal does not name the alias"
        );
        for part in ["pa'ss", "'ss", "u:pa", "ss@"] {
            assert!(!text.contains(part), "the refusal echoes {part:?}");
        }
    }

    /// Sentinel token for the #9227 clone tests; never printed by an assert.
    const TOKEN: &str = "tm9227SentinelTok";

    /// A sandbox with a local bare repo that a credentialed `https` URL clones
    /// from offline, through `url.<base>.insteadOf` in a private git config.
    struct GitSandbox {
        _dir: tempfile::TempDir,
        home: PathBuf,
        gitconfig: PathBuf,
        managed_root: PathBuf,
        claude_config_dir: PathBuf,
    }

    impl GitSandbox {
        fn new() -> Self {
            let dir = crate::test_support::hermetic_temp_dir();
            let root = dir.path().to_path_buf();
            let home = root.join("home");
            let remotes = root.join("remotes");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(&remotes).unwrap();
            let gitconfig = root.join("gitconfig");
            let base = format!("file://{}/", remotes.display());
            std::fs::write(
                &gitconfig,
                format!(
                    "[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n\
                     [url \"{base}\"]\n\tinsteadOf = https://example.invalid/\n\
                     \tinsteadOf = https://x-access-token:{TOKEN}@example.invalid/\n\
                     \tinsteadOf = https://x-access-token:/{TOKEN}@example.invalid/\n"
                ),
            )
            .unwrap();
            let sb = Self {
                _dir: dir,
                home,
                gitconfig,
                managed_root: root.join("managed"),
                claude_config_dir: root.join("claude-config"),
            };
            let work = root.join("work");
            sb.git(&root, &["init", "-q", "work"]);
            std::fs::write(work.join("README.md"), "fixture\n").unwrap();
            sb.git(&work, &["add", "README.md"]);
            sb.git(
                &work,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-qm",
                    "i",
                ],
            );
            sb.git(
                &root,
                &["clone", "-q", "--bare", "work", "remotes/o9227/r9227.git"],
            );
            sb
        }

        /// The environment every child git gets: no system, user or
        /// environment-injected config. `GIT_CONFIG_COUNT=0` also covers the
        /// sandbox clearing `GIT_CONFIG_KEY_<n>` but not the count.
        fn env(&self) -> [(&'static str, &OsStr); 5] {
            [
                ("HOME", self.home.as_os_str()),
                ("GIT_CONFIG_GLOBAL", self.gitconfig.as_os_str()),
                ("GIT_CONFIG_NOSYSTEM", OsStr::new("1")),
                ("GIT_CONFIG_COUNT", OsStr::new("0")),
                ("GIT_CONFIG_PARAMETERS", OsStr::new("")),
            ]
        }

        fn git(&self, dir: &Path, args: &[&str]) {
            let status = Command::new("git")
                .args(args)
                .envs(self.env())
                .current_dir(dir)
                .status()
                .expect("spawn git");
            assert!(status.success(), "fixture git {:?} failed", args[0]);
        }

        fn register(&self, alias: &str, url: &str) {
            let mut reg = ManagedRegistry::load(&self.managed_root).unwrap();
            reg.add(alias, url, false).unwrap();
            reg.save().unwrap();
        }

        fn load(&self, alias: &str) -> anyhow::Result<PathBuf> {
            load_alias_with_git_env(
                alias,
                &self.managed_root,
                &self.claude_config_dir,
                &self.env(),
            )
        }
    }

    /// #9227: a registry entry stored with a token clones through the stripped
    /// URL, so neither `.git/config` nor `managed.toml` records the token, and
    /// `registry.json` keeps the entry as stored.
    #[test]
    #[serial_test::serial]
    fn load_alias_strips_a_stored_token_before_clone_and_marker() {
        // `prepare_session` resolves `dirs::home_dir()` and writes there.
        let _creds = CredentialSandbox::enter();
        let _palace_guard = EnvClearGuard::clear("TRUSTY_MEMORY_PALACE");
        let sb = GitSandbox::new();
        let raw = format!("https://x-access-token:{TOKEN}@example.invalid/o9227/r9227.git");
        let clean = "https://example.invalid/o9227/r9227.git";
        sb.register("tok9227", &raw);

        let repo = sb.load("tok9227").expect("load_alias clones the fixture");

        let git_config = std::fs::read_to_string(repo.join(".git").join("config")).unwrap();
        assert!(!git_config.contains(TOKEN), ".git/config holds the token");
        assert!(
            !git_config.contains('@') && git_config.contains(&format!("url = {clean}")),
            "remote.origin.url is not the userinfo-free URL"
        );
        let marker_text =
            std::fs::read_to_string(repo.join(".trusty-mpm").join("managed.toml")).unwrap();
        let marker: ManagedMarker = toml::from_str(&marker_text).unwrap();
        assert!(
            !marker_text.contains(TOKEN) && !marker_text.contains('@'),
            "managed.toml holds userinfo"
        );
        assert!(
            marker.url == clean,
            "managed.toml url is not the stripped URL"
        );
        let reg = ManagedRegistry::load(&sb.managed_root).unwrap();
        assert!(
            reg.get("tok9227").unwrap().url == raw,
            "registry.json entry was rewritten"
        );
    }

    /// #9227: a token `strip_url_secret` cannot remove is refused before any
    /// clone, and the error never echoes it.
    #[test]
    #[serial_test::serial]
    fn load_alias_refuses_a_url_whose_token_cannot_be_stripped() {
        let _creds = CredentialSandbox::enter();
        let _palace_guard = EnvClearGuard::clear("TRUSTY_MEMORY_PALACE");
        let sb = GitSandbox::new();
        // A raw `/` after an empty port reads as `host:` to the stripper.
        sb.register(
            "bad9227",
            &format!("https://x-access-token:/{TOKEN}@example.invalid/o9227/r9227.git"),
        );

        let Err(err) = sb.load("bad9227") else {
            panic!("load_alias cloned a URL whose token it could not strip");
        };

        let text = format!("{err:#} {err:?}");
        assert!(!text.contains(TOKEN), "the refusal echoes the token");
        assert!(
            !sb.managed_root.join("projects").join("bad9227").exists(),
            "a refused load created the project directory"
        );
    }
}
