//! Bootstrap the single tm-global CLAUDE_CONFIG_DIR (`~/.trusty-mpm/claude-config/`).
//!
//! Why: DOC-24 SPEC-STANDALONE-MPM-03 requires a single, shared user-level
//! config directory that supplies the global hooks, global skills/slash-commands,
//! and global MCP servers for ALL tm-launched sessions. Claude Code is pointed at
//! it via the required `CLAUDE_CONFIG_DIR` env var.
//! This module creates that dir once and never regenerates it per project.
//! What: [`ensure_global_config_dir`] creates `<managed_root>/claude-config/`,
//! writes a minimal `settings.json` seeded with the `outputStyle`/`statusLine`
//! defaults via [`super::settings_defaults::ensure_settings_defaults`] (issue
//! #2214 defense-in-depth — see that module's doc comment), merges the MPM hook
//! triad, seeds `.credentials.json` from `~/.claude/.credentials.json` if it
//! exists (NOTE: this file holds MCP OAuth tokens, NOT the primary session auth
//! — see WI-10 for the auth model), declares the framework builtin MCP servers
//! in the user-scope `mcpServers` map of `.claude.json` (#4181 / ADR-0042 —
//! see [`seed_builtin_mcp_servers`]), and (WI-2)
//! deploys bundled agents and skills from `<managed_root>/framework/{agents,skills}`
//! into the managed config dir so that every tm-launched session starts with
//! the full agent/skill set.
//!
//! # WI-10 Auth Model
//!
//! Primary session auth (`claude` on a Claude Max/Pro plan) uses the macOS
//! Keychain keyed by the `CLAUDE_CONFIG_DIR` path. Seeding `.credentials.json`
//! does NOT establish that keychain entry — it only carries MCP OAuth tokens.
//! The two supported auth paths are:
//!
//! 1. **Keychain (default):** run `tm login` once. That command launches
//!    `claude auth login` under `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config`
//!    so the OAuth flow creates a keychain entry for that path. All subsequent
//!    `tm run` sessions authenticate on the Max/Pro plan automatically.
//!
//! 2. **API key (`--bare`):** set `ANTHROPIC_API_KEY` in the environment.
//!    `tm run` detects the key and adds `--bare` to the `claude` invocation,
//!    which bypasses keychain/OAuth and uses the API key directly. Useful for
//!    CI and automation.
//!
//! Test: `test_global_config_dir_ensure_idempotent` and
//! `test_deploy_agents_and_skills_*` in this module.

use std::path::{Path, PathBuf};

/// Ensure the single tm-global config dir exists, seeding minimal content.
///
/// Why: `tm run` calls this before launching `claude` so the CLAUDE_CONFIG_DIR
/// always contains a valid `settings.json`, MCP config, and (WI-2) the bundled
/// agents and skills. Idempotent — safe to call on every launch.
/// What: creates `<managed_root>/claude-config/`, seeds `settings.json` with the
/// `outputStyle`/`statusLine` defaults via
/// [`super::settings_defaults::ensure_settings_defaults`] (issue #2214;
/// non-destructive — never overwrites an already-set value), merges the MPM
/// hook triad into `settings.json` via [`super::hooks::ensure_managed_hooks`]
/// (WI-3), copies `~/.claude/.credentials.json` if found (silently skips
/// otherwise), declares the framework builtin MCP servers in `.claude.json`'s
/// user-scope `mcpServers` map (#4181 / ADR-0042 — insert-if-absent, non-fatal;
/// see [`seed_builtin_mcp_servers`]), then deploys bundled agents and skills
/// via [`deploy_agents_and_skills`].
/// Test: `test_global_config_dir_ensure_idempotent`,
/// `test_deploy_agents_and_skills_populates_config_dir`,
/// `test_deploy_agents_and_skills_missing_source_is_ok`,
/// `test_global_config_dir_seeds_output_style_and_status_line`,
/// `test_global_config_dir_seeds_builtin_mcp_servers`.
pub fn ensure_global_config_dir(
    managed_root: &Path,
    claude_config_dir: &Path,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(claude_config_dir)?;

    // Issue #2214: seed `outputStyle`/`statusLine` defaults directly into the
    // tm-owned settings.json (defense-in-depth, independent of the project tier
    // + `--setting-sources`). Non-destructive — see module doc comment.
    super::settings_defaults::ensure_settings_defaults(claude_config_dir)?;

    // WI-3: merge the MPM lifecycle hook triad (PreToolUse/PostToolUse/Stop)
    // into settings.json so managed sessions emit lifecycle events to the daemon.
    super::hooks::ensure_managed_hooks(claude_config_dir)?;

    seed_credentials(claude_config_dir);
    seed_builtin_mcp_servers(claude_config_dir);
    deploy_agents_and_skills(managed_root, claude_config_dir)?;

    Ok(claude_config_dir.to_path_buf())
}

/// Deploy bundled agents and skills into the managed CLAUDE_CONFIG_DIR (WI-2).
///
/// Why: managed `tm run` sessions use `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config/`
/// which starts empty — without deploying the trusty-mpm agents and skills, sessions
/// start with NO agent/skill set, defeating the purpose of the managed driver.
/// This function populates `<claude_config_dir>/agents/` and
/// `<claude_config_dir>/skills/` from the framework sources installed by `tm install`.
/// What: calls [`crate::core::agent_deployer::deploy_agents`] and
/// [`crate::core::skill_deployer::deploy_skills`] (the same deployers used by
/// `tm install` for the real `~/.claude` dirs), each guarded by a source-dir
/// existence check. If a source dir is missing (framework not yet installed),
/// emits a hint to stderr and skips without error so `tm load`/`tm run`/`tm login`
/// remain functional before `tm install` has been run. Errors from the deployers
/// are surfaced as `anyhow::Error`.
/// Test: `test_deploy_agents_and_skills_populates_config_dir` (happy-path),
/// `test_deploy_agents_and_skills_missing_source_is_ok` (pre-install guard).
/// Deploy bundled output styles, agents, and skills into the managed CLAUDE_CONFIG_DIR (WI-2).
///
/// Why: managed `tm run`/`tm load`/`tm login` sessions use
/// `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config/` which starts empty.
/// Without deploying the trusty-mpm agent/skill set AND the output-style
/// definitions, sessions start with neither agent/skill set nor output-style
/// active, defeating the purpose of the managed driver.
/// This function populates `<claude_config_dir>/output-styles/`,
/// `<claude_config_dir>/agents/`, and `<claude_config_dir>/skills/` from the
/// bundled constants (output styles) and the framework sources installed by
/// `tm install` (agents + skills).
/// What: calls [`crate::core::output_style_deployer::deploy_output_styles`]
/// unconditionally (bundled constants are always available), then calls
/// [`crate::core::agent_deployer::deploy_agents`] and
/// [`crate::core::skill_deployer::deploy_skills`] (the same deployers used by
/// `tm install` for the real `~/.claude` dirs), each guarded by a source-dir
/// existence check. If a source dir is missing (framework not yet installed),
/// emits a hint to stderr and skips without error so `tm load`/`tm run`/
/// `tm login` remain functional before `tm install` has been run. Errors from
/// the deployers are surfaced as `anyhow::Error`.
/// Test: `test_deploy_agents_and_skills_populates_config_dir` (happy-path),
/// `test_deploy_agents_and_skills_missing_source_is_ok` (pre-install guard),
/// `test_deploy_output_styles_wired_into_config_dir` (output-style write).
fn deploy_agents_and_skills(managed_root: &Path, claude_config_dir: &Path) -> anyhow::Result<()> {
    let agents_src = managed_root.join("framework").join("agents");
    let skills_src = managed_root.join("framework").join("skills");
    let agents_dest = claude_config_dir.join("agents");
    let skills_dest = claude_config_dir.join("skills");

    // Nit 3 (accepted TOCTOU): `exists()` is checked here before calling the
    // deployers so we can emit a targeted hint. The deployers already handle a
    // missing source dir by returning Ok(empty) internally, so this is a
    // benign, accepted TOCTOU window — the framework directory essentially never
    // disappears mid-run in normal dev use.
    let agents_missing = !agents_src.exists();
    let skills_missing = !skills_src.exists();

    // Nit 1: collapse the two missing-source hints into a single combined message
    // when both source dirs are absent (framework not yet installed), so the user
    // sees one clear actionable line instead of two. When only one is missing,
    // print just that specific hint so it names the exact missing path.
    if agents_missing && skills_missing {
        eprintln!(
            "note: no bundled agents or skills found under {}/framework; \
             run `tm install` to populate them",
            managed_root.display()
        );
    } else {
        if agents_missing {
            eprintln!(
                "note: no bundled agents found at {}; run `tm install` to populate them",
                agents_src.display()
            );
        }
        if skills_missing {
            eprintln!(
                "note: no bundled skills found at {}; run `tm install` to populate them",
                skills_src.display()
            );
        }
    }

    if !agents_missing {
        // Nit 2: layout contract — deploy_agents writes each flat <name>.md source
        // as <agents_dest>/<name>.md plus a .trusty-mpm-manifest.json side-car
        // (see core::agent_deployer). deploy_all_skill_tiers writes each flat
        // <name>.md source as <skills_dest>/<name>/SKILL.md (see
        // core::skill_deployer).
        crate::core::agent_deployer::deploy_agents(&agents_src, &agents_dest)
            .map_err(|e| anyhow::anyhow!("failed to deploy agents into managed config dir: {e}"))?;
    }

    // PR #2818 review (round 3, MEDIUM decision): route through the multi-tier
    // orchestrator rather than raw `deploy_skills`, so a user-custom skill
    // (`~/.trusty-mpm/skills/`, i.e. `<managed_root>/skills`) reaches the
    // tm-global roster too — Bob's stated intent is that user-custom skills
    // apply to every session, and this config dir is load-bearing for the
    // flag-less standalone `tm run` driver (see the module doc: the daemon
    // managed-spawn path reads the roster from the PROJECT layer instead, so
    // this dir is belt-and-suspenders there, but load-bearing here). The
    // project-custom tier is naturally empty at this dir — nothing hand-places
    // a skill directly into `<claude_config_dir>/skills/`, so
    // `deploy_all_skill_tiers`'s project-stem scan simply finds none, exactly
    // matching "N/A" without any special-casing. Unlike the old guard, this
    // call is NOT skipped when `skills_missing` — a user-tier skill must still
    // deploy even if the bundled framework skills haven't been installed yet;
    // the orchestrator already treats a missing source dir as an empty tier.
    crate::core::skill_tiers::deploy_all_skill_tiers(
        &skills_src,
        &managed_root.join("skills"),
        &skills_dest,
        |_| true,
    )
    .map_err(|e| anyhow::anyhow!("failed to deploy skills into managed config dir: {e}"))?;

    // WI-2 follow-up (#1553): deploy bundled output styles from compile-time
    // constants — no framework installation required.  These are always
    // framework-owned (never user-editable), so the deployer overwrites stale
    // copies and skips files whose checksum already matches (idempotent).
    let styles = crate::core::output_style_deployer::deploy_output_styles(claude_config_dir)
        .map_err(|e| {
            anyhow::anyhow!("failed to deploy output styles into managed config dir: {e}")
        })?;
    // #5866: the deployer now records a per-style IO failure and keeps going,
    // so the batch verdict this caller owes its own callers is read from
    // `failed` rather than from the return status.
    if !styles.failed.is_empty() {
        let detail = styles
            .failed
            .iter()
            .map(|(file_name, why)| format!("{file_name}: {why}"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(anyhow::anyhow!(
            "failed to deploy output styles into managed config dir: {detail}"
        ));
    }

    Ok(())
}

/// Core copy logic for credential seeding — testable without `dirs::home_dir`.
///
/// Why: extracting the copy logic into a free function with explicit `src`/`dst`
/// parameters makes real unit tests possible without `dirs::home_dir()` inside
/// the hot path (F3 fix; previously the test replicated the logic inline rather
/// than calling the production code).
/// NOTE: `.credentials.json` carries MCP OAuth tokens (trusty-memory etc.),
/// NOT the primary Claude Max/Pro session auth. Primary session auth requires
/// a macOS Keychain entry keyed by the `CLAUDE_CONFIG_DIR` path — established
/// via `tm login` (keychain path) or bypassed by `ANTHROPIC_API_KEY`+`--bare`
/// (API-key path). See module-level WI-10 doc for details.
///
/// # Credential direction (closes #1550 item 2)
///
/// We seed **only when the managed copy is absent**. We intentionally do NOT
/// overwrite an existing managed `.credentials.json`, even when the real-home
/// source is newer. Rationale: once the managed dir has a credential file the
/// user may have intentionally put different MCP OAuth tokens there (e.g. a
/// different Anthropic account or a customised set of server grants). Silently
/// clobbering it on every `tm run` would destroy that deliberate divergence with
/// no warning. When the managed copy already exists, `tm login` is the correct
/// mechanism to refresh it.
///
/// What: copies `src` → `dst` only when `dst` is absent. When `src` is missing
/// the function returns `Ok(())` and emits an informational note. Credential
/// file contents are never logged.
/// Test: `test_seed_credentials_from_missing_src_is_ok`,
/// `test_seed_credentials_from_copies_when_dst_missing`,
/// `test_seed_credentials_from_skips_when_dst_already_exists`.
pub(crate) fn seed_credentials_from(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> anyhow::Result<()> {
    if !src.exists() {
        eprintln!(
            "note: {} not found; MCP OAuth tokens will not be pre-seeded \
             (primary auth is via `tm login` or ANTHROPIC_API_KEY, not this file)",
            src.display()
        );
        return Ok(());
    }

    // Only seed when the managed copy is absent (first-time bootstrap).
    // If a managed credential file already exists we leave it untouched — the
    // user may have intentionally diverged it from the real-home copy.
    if !dst.exists() {
        std::fs::copy(src, dst).map_err(|e| anyhow::anyhow!("failed to copy credentials: {e}"))?;
    }
    Ok(())
}

/// Copy `~/.claude/.credentials.json` into the tm-global config dir (MCP tokens).
///
/// Why: `.credentials.json` carries MCP OAuth tokens (trusty-memory, etc.) which
/// are useful to pre-seed so managed sessions can reach those MCP servers. NOTE:
/// this does NOT establish primary Claude Max/Pro session auth — that requires a
/// macOS Keychain entry keyed by the `CLAUDE_CONFIG_DIR` path, established via
/// `tm login` (keychain path). See module-level WI-10 auth model.
/// Delegates to `seed_credentials_from` so the mtime-comparison and copy logic
/// is unit-testable with explicit paths. Silently warns when the home dir or src
/// is absent (non-blocking; primary auth is independent of this file).
/// What: resolves `src = ~/.claude/.credentials.json` and
/// `dst = <claude_config_dir>/.credentials.json`, delegates to
/// `seed_credentials_from`. Credential contents are never logged.
/// Test: `test_seed_credentials_skips_when_source_missing` exercises the
/// missing-source path via `ensure_global_config_dir`. Direct logic is covered
/// by the `seed_credentials_from` tests.
fn seed_credentials(claude_config_dir: &Path) {
    let Some(home) = dirs::home_dir() else {
        eprintln!("warning: cannot resolve home directory; skipping credential seed");
        return;
    };
    let src = home.join(".claude").join(".credentials.json");
    let dst = claude_config_dir.join(".credentials.json");
    if let Err(e) = seed_credentials_from(&src, &dst) {
        eprintln!("warning: {e}");
    }
}

/// Declare the framework builtin MCP servers in USER scope, non-fatally.
///
/// Why (#4181, ADR-0042): this replaced `ensure_mcp_config`, which wrote the
/// same four servers into `<claude_config_dir>/.mcp.json`. Claude Code
/// discovers a `.mcp.json` only by walking UP from the session's cwd, and cwd
/// is always the repo (`standalone::run` sets `current_dir(repo_path)`; the
/// daemon spawns in the workspace), so that file was never on any session's
/// search path — a server declared there reached nothing. The top-level
/// `mcpServers` map of `<claude_config_dir>/.claude.json` is the map that does
/// reach a session, and reaches it with no approval prompt.
/// What: calls [`crate::core::mcp_config::seed_builtin_servers`] and swallows
/// the outcome into a log line. **Non-fatal by construction** — this runs
/// between `seed_credentials` and [`deploy_agents_and_skills`], so a `?` here
/// would let an unwritable `.claude.json` skip the agent and skill deploy and
/// leave the session with no roster. A seeding failure costs MCP servers for
/// one launch; it must cost nothing else.
/// Test: `test_global_config_dir_seeds_builtin_mcp_servers`,
/// `test_global_config_dir_never_overwrites_an_operator_mcp_entry`,
/// `test_global_config_dir_survives_a_malformed_claude_json`.
fn seed_builtin_mcp_servers(claude_config_dir: &Path) {
    match crate::core::mcp_config::seed_builtin_servers(claude_config_dir) {
        Ok(seeded) if !seeded.is_empty() => tracing::info!(
            config_dir = %claude_config_dir.display(),
            servers = %seeded.join(", "),
            "declared builtin MCP servers in user scope"
        ),
        Ok(_) => {}
        Err(e) => tracing::warn!(
            config_dir = %claude_config_dir.display(),
            "builtin MCP server seeding failed (non-fatal): {e}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_global_config_dir_ensure_idempotent() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let claude_config = managed_root.join("claude-config");

        ensure_global_config_dir(&managed_root, &claude_config).unwrap();
        assert!(claude_config.join("settings.json").exists());
        // #4181: the MCP declaration moved from `.mcp.json` (never on a
        // session's discovery path) to `.claude.json`'s user-scope map.
        assert!(claude_config.join(".claude.json").exists());

        // Second call must not fail (idempotent).
        ensure_global_config_dir(&managed_root, &claude_config).unwrap();
        assert!(claude_config.join("settings.json").exists());
    }

    // Issue #2214: ensure_global_config_dir must seed outputStyle/statusLine
    // into a FRESH tm-owned settings.json, and must preserve any pre-existing
    // unrelated keys (e.g. a client-persisted `theme`) on a second call.
    #[test]
    fn test_global_config_dir_seeds_output_style_and_status_line() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let claude_config = managed_root.join("claude-config");

        ensure_global_config_dir(&managed_root, &claude_config).unwrap();

        let text = std::fs::read_to_string(claude_config.join("settings.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            val["outputStyle"].as_str(),
            Some(crate::core::session_launch::OUTPUT_STYLE),
            "fresh tm-owned settings.json must have outputStyle seeded"
        );
        assert!(
            val["statusLine"]["command"]
                .as_str()
                .is_some_and(|c| c.ends_with(" statusline")),
            "fresh tm-owned settings.json must have statusLine seeded"
        );

        // Simulate a client-persisted key (e.g. self-persisted theme) landing
        // in the tm-owned settings.json between launches.
        let mut with_theme = val.clone();
        with_theme["theme"] = serde_json::json!("dark");
        std::fs::write(
            claude_config.join("settings.json"),
            serde_json::to_string_pretty(&with_theme).unwrap(),
        )
        .unwrap();

        // Re-running ensure_global_config_dir must not clobber that key.
        ensure_global_config_dir(&managed_root, &claude_config).unwrap();
        let text_after = std::fs::read_to_string(claude_config.join("settings.json")).unwrap();
        let val_after: serde_json::Value = serde_json::from_str(&text_after).unwrap();
        assert_eq!(
            val_after["theme"].as_str(),
            Some("dark"),
            "client-persisted keys must survive re-provisioning"
        );
        assert_eq!(
            val_after["outputStyle"].as_str(),
            Some(crate::core::session_launch::OUTPUT_STYLE)
        );
    }

    // WI-3 + WI-8 + #3918, retargeted by #4181: the four framework builtins
    // must be declared in `.claude.json`'s user-scope `mcpServers` map — the
    // map that reaches a session without an approval prompt. This assertion
    // used to read the `.mcp.json` `ensure_mcp_config` wrote, which no session
    // ever discovered (cwd is always the repo, and discovery walks up from cwd).
    #[test]
    fn test_global_config_dir_seeds_builtin_mcp_servers() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let cfg = managed_root.join("claude-config");

        ensure_global_config_dir(&managed_root, &cfg).unwrap();

        let text = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        let servers = val["mcpServers"].as_object().unwrap();
        for name in crate::core::mcp_config::BUILTIN_MANAGED_MCP_SERVERS {
            assert!(
                servers.contains_key(*name),
                "{name} must be declared in .claude.json's user-scope mcpServers map"
            );
        }
    }

    // WI-8, retargeted: the trusty-review entry keeps its stdio transport and
    // canonical command/args in its new home.
    #[test]
    fn test_seeded_review_server_entry() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        seed_builtin_mcp_servers(&cfg);
        let text = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        let review = &val["mcpServers"]["trusty-review"];
        assert_eq!(
            review["type"].as_str(),
            Some("stdio"),
            "trusty-review transport must be stdio"
        );
        assert_eq!(
            review["command"].as_str(),
            Some("trusty-review"),
            "trusty-review command must be 'trusty-review'"
        );
        let args: Vec<&str> = review["args"]
            .as_array()
            .expect("args must be an array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            args,
            vec!["serve", "--stdio"],
            "trusty-review args must be [\"serve\", \"--stdio\"]"
        );
    }

    // WI-8 ISOLATION, retargeted: seeding must touch nothing outside the given
    // config dir — in particular never the operator's real `$HOME`.
    #[serial_test::serial]
    #[test]
    fn test_seed_builtin_mcp_servers_no_home_write() {
        /// RAII guard restoring $HOME on drop (including panic).
        struct HomeGuard(Option<String>);
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                match self.0 {
                    Some(ref p) => unsafe { std::env::set_var("HOME", p) },
                    None => unsafe { std::env::remove_var("HOME") },
                }
            }
        }

        let root = TempDir::new().unwrap();
        let cfg = root.path().join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();

        let fake_home = TempDir::new().unwrap();
        // SAFETY: test is serial; HomeGuard restores HOME even on panic.
        let _home_guard = {
            let prev = std::env::var("HOME").ok();
            unsafe { std::env::set_var("HOME", fake_home.path()) };
            HomeGuard(prev)
        };

        seed_builtin_mcp_servers(&cfg);

        assert!(
            cfg.join(".claude.json").exists(),
            "seeding must write .claude.json inside the given dir"
        );
        // Nothing must land directly under root (outside cfg).
        assert!(
            !root.path().join(".claude.json").exists(),
            "seeding must NOT write .claude.json outside the given dir (isolation)"
        );
        // $HOME must remain empty — no .claude.json or .claude/ created.
        assert!(
            !fake_home.path().join(".claude.json").exists(),
            "seeding must NOT write .claude.json to $HOME (isolation)"
        );
        assert!(
            !fake_home.path().join(".claude").exists(),
            "seeding must NOT create $HOME/.claude/ (isolation)"
        );
        // _home_guard drops here and restores HOME.
    }

    // #1550 item 3, retargeted by #4181: re-provisioning must not rewrite
    // `.claude.json` when every builtin is already declared. Byte comparison,
    // because mtime is unreliable on coarse-grained CI filesystems.
    #[test]
    fn test_seeding_no_spurious_write_when_unchanged() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        let claude_json = cfg.join(".claude.json");

        seed_builtin_mcp_servers(&cfg);
        let bytes_after_first = std::fs::read(&claude_json).unwrap();

        seed_builtin_mcp_servers(&cfg);
        let bytes_after_second = std::fs::read(&claude_json).unwrap();

        assert_eq!(
            bytes_after_first, bytes_after_second,
            "seeding must not rewrite .claude.json when every builtin is already present"
        );
    }

    // #1550 Finding-1, retargeted by #4181: a trailing newline left by an
    // editor is a formatting difference, not a content difference. Seeding
    // decides by parsed key membership, so such a file must come back
    // byte-identical — a serialize-and-compare implementation would silently
    // reformat the operator's `.claude.json` on every single launch.
    #[test]
    fn test_seeding_trailing_newline_does_not_trigger_rewrite() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        let claude_json = cfg.join(".claude.json");

        seed_builtin_mcp_servers(&cfg);
        let canonical = std::fs::read_to_string(&claude_json).unwrap();
        std::fs::write(&claude_json, format!("{canonical}\n").as_bytes()).unwrap();
        let bytes_before = std::fs::read(&claude_json).unwrap();

        seed_builtin_mcp_servers(&cfg);

        let bytes_after = std::fs::read(&claude_json).unwrap();
        assert_eq!(
            bytes_before, bytes_after,
            "a trailing newline must not trigger a spurious rewrite of .claude.json \
             (refs #1550 Finding-1)"
        );
    }

    // #4181: an entry the operator registered under a builtin name via
    // `tm mcp add` is the supported edit path (ADR-0042 decision item 3) and
    // must survive re-provisioning verbatim. Insert-if-absent, observed through
    // the real `ensure_global_config_dir` entry point.
    #[test]
    fn test_global_config_dir_never_overwrites_an_operator_mcp_entry() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let cfg = managed_root.join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();

        let operator = serde_json::json!({
            "type": "stdio",
            "command": "/opt/operator/trusty-search",
            "args": ["serve", "--index", "pinned"]
        });
        std::fs::write(
            cfg.join(".claude.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "mcpServers": { "trusty-search": operator.clone() }
            }))
            .unwrap(),
        )
        .unwrap();

        ensure_global_config_dir(&managed_root, &cfg).unwrap();

        let text = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            val["mcpServers"]["trusty-search"], operator,
            "seeding must never overwrite an operator-registered entry"
        );
        assert!(
            val["mcpServers"]["trusty-mpm"].is_object(),
            "the remaining builtins must still be seeded alongside it"
        );
    }

    // #4181 FAIL-OPEN: seeding runs on every launch now, so a malformed
    // `.claude.json` must neither fail provisioning nor be quarantined by the
    // seeder — that file also holds OAuth state. `deploy_agents_and_skills`
    // runs AFTER the seed, so the output-styles assertion is what proves the
    // failure did not short-circuit the rest of provisioning.
    #[test]
    fn test_global_config_dir_survives_a_malformed_claude_json() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let cfg = managed_root.join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();

        // Truncated mid-object — what a crash or a full disk leaves behind.
        // Built rather than written as a literal: an unbalanced brace inside a
        // string literal skews `check_line_cap.sh`'s brace matcher and makes it
        // count this whole inline test module against the 500-SLOC prod cap.
        let complete = serde_json::to_string(&serde_json::json!({
            "oauthAccount": { "emailAddress": "op@example.com" }
        }))
        .unwrap();
        let malformed = complete.as_bytes()[..complete.len() - 1].to_vec();
        std::fs::write(cfg.join(".claude.json"), &malformed).unwrap();

        ensure_global_config_dir(&managed_root, &cfg)
            .expect("a malformed .claude.json must not fail provisioning");

        assert_eq!(
            std::fs::read(cfg.join(".claude.json")).unwrap(),
            malformed,
            "the seeder must leave a malformed .claude.json byte-identical — no rename, no write"
        );
        let quarantined = std::fs::read_dir(&cfg)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
        assert!(!quarantined, "the seeder must not quarantine .claude.json");
        assert!(
            cfg.join("output-styles").exists(),
            "provisioning steps after the seed must still have run"
        );
    }

    // #4181 FAIL-OPEN: an unreadable `.claude.json` makes `seed_builtin_servers`
    // return `Err`, and the wrapper must absorb it so the agent/skill deploy
    // below still runs. A directory standing where the file belongs is the
    // read error that reproduces without depending on the test user's uid.
    #[test]
    fn test_global_config_dir_survives_an_unreadable_claude_json() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let cfg = managed_root.join("claude-config");
        std::fs::create_dir_all(cfg.join(".claude.json")).unwrap();

        ensure_global_config_dir(&managed_root, &cfg)
            .expect("an unreadable .claude.json must not fail provisioning");

        assert!(
            cfg.join(".claude.json").is_dir(),
            "the seeder must not have disturbed what it could not read"
        );
        assert!(
            cfg.join("output-styles").exists(),
            "provisioning steps after the seed must still have run"
        );
    }

    // F3: seed_credentials_from — src missing → returns Ok, dst unchanged.
    #[test]
    fn test_seed_credentials_from_missing_src_is_ok() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("nonexistent.json");
        let dst = tmp.path().join("dst.json");

        // src does not exist; dst also does not exist.
        let result = seed_credentials_from(&src, &dst);
        assert!(result.is_ok(), "missing src must return Ok, not an error");
        assert!(!dst.exists(), "dst must not be created when src is missing");
    }

    // F3: seed_credentials_from — dst missing → dst is created with src content.
    #[test]
    fn test_seed_credentials_from_copies_when_dst_missing() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.json");
        let dst = tmp.path().join("dst.json");

        std::fs::write(&src, r#"{"token":"initial"}"#).unwrap();

        seed_credentials_from(&src, &dst).unwrap();

        assert!(dst.exists(), "dst must be created when it was missing");
        let content = std::fs::read_to_string(&dst).unwrap();
        assert_eq!(
            content, r#"{"token":"initial"}"#,
            "dst content must match src content after first-time copy"
        );
    }

    // #1550 item 2: seed_credentials_from — dst already exists → must NOT be
    // overwritten even when src has different (or newer) content.  The managed
    // credential file may have been intentionally diverged (different account).
    #[test]
    fn test_seed_credentials_from_skips_when_dst_already_exists() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.json");
        let dst = tmp.path().join("dst.json");

        std::fs::write(&src, r#"{"token":"real-home"}"#).unwrap();
        // dst already exists with different (managed) content.
        std::fs::write(&dst, r#"{"token":"managed-custom"}"#).unwrap();

        seed_credentials_from(&src, &dst).unwrap();

        let content = std::fs::read_to_string(&dst).unwrap();
        assert_eq!(
            content, r#"{"token":"managed-custom"}"#,
            "existing managed credential must NOT be overwritten (#1550 item 2)"
        );
    }

    // F3: seed_credentials must do nothing (warn only) when source is missing.
    // We test the guard in ensure_global_config_dir indirectly: calling it in a
    // tmp dir where ~/.claude/.credentials.json does not exist must not panic or
    // leave a broken dest file.
    #[test]
    fn test_seed_credentials_skips_when_source_missing() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let claude_config = managed_root.join("claude-config");
        // ensure_global_config_dir calls seed_credentials; in a CI environment
        // where ~/.claude/.credentials.json does not exist it must succeed (no
        // panic, no dest file created).
        ensure_global_config_dir(&managed_root, &claude_config).unwrap();
        // The dest should NOT exist if the source (~/.claude/.credentials.json)
        // doesn't exist. We cannot guarantee the source state in CI/dev, so
        // only assert that the function returns Ok (no panic).
        assert!(claude_config.join("settings.json").exists());
    }

    // WI-2: deploy_agents_and_skills — happy path: agent and skill source dirs
    // exist; both must be deployed into the managed config dir.
    #[test]
    fn test_deploy_agents_and_skills_populates_config_dir() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let claude_config = managed_root.join("claude-config");

        // Write a fake agent source file under <managed_root>/framework/agents/.
        let agents_src = managed_root.join("framework").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::write(
            agents_src.join("my-agent.md"),
            "---\nname: my-agent\nrole: engineer\n---\n\n# My Agent\n\nAgent content.\n",
        )
        .unwrap();

        // Write a fake skill source file under <managed_root>/framework/skills/.
        let skills_src = managed_root.join("framework").join("skills");
        std::fs::create_dir_all(&skills_src).unwrap();
        std::fs::write(
            skills_src.join("my-skill.md"),
            "---\nname: my-skill\n---\n\n# My Skill\n\nSkill content.\n",
        )
        .unwrap();

        ensure_global_config_dir(&managed_root, &claude_config).unwrap();

        // Agent must land in <config_dir>/agents/my-agent.md.
        let agent_path = claude_config.join("agents").join("my-agent.md");
        assert!(
            agent_path.exists(),
            "agent must be deployed to config_dir/agents/my-agent.md"
        );
        let agent_content = std::fs::read_to_string(&agent_path).unwrap();
        assert!(
            agent_content.contains("Agent content."),
            "deployed agent must contain the source content"
        );

        // Skill must land in <config_dir>/skills/my-skill/SKILL.md.
        let skill_path = claude_config
            .join("skills")
            .join("my-skill")
            .join("SKILL.md");
        assert!(
            skill_path.exists(),
            "skill must be deployed to config_dir/skills/my-skill/SKILL.md"
        );
        let skill_content = std::fs::read_to_string(&skill_path).unwrap();
        assert!(
            skill_content.contains("Skill content."),
            "deployed skill must contain the source content"
        );

        // Agent manifest must exist (written by deploy_agents).
        assert!(
            claude_config
                .join("agents")
                .join(".trusty-mpm-manifest.json")
                .exists(),
            "agent manifest must exist after deploy"
        );
    }

    #[test]
    fn test_deploy_agents_and_skills_includes_user_tier_skill() {
        // PR #2818 review (round 3, MEDIUM decision): a user-custom skill
        // (`<managed_root>/skills/`) must reach the tm-global roster deployed
        // by this function, not just per-project deploys.
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let claude_config = managed_root.join("claude-config");

        // No bundled framework/skills source — proves the user tier deploys
        // even when the bundled portfolio hasn't been installed yet.
        let user_skills_src = managed_root.join("skills");
        std::fs::create_dir_all(&user_skills_src).unwrap();
        std::fs::write(
            user_skills_src.join("my-custom-skill.md"),
            "---\nname: my-custom-skill\n---\n\nUSER CUSTOM.\n",
        )
        .unwrap();

        ensure_global_config_dir(&managed_root, &claude_config).unwrap();

        assert!(
            claude_config
                .join("skills/my-custom-skill/SKILL.md")
                .exists(),
            "user-custom skill must be deployed to the tm-global config dir \
             even with no bundled framework skill source present"
        );
    }

    // WI-2: deploy_agents_and_skills — missing-source guard: when the framework
    // source dirs do not exist (tm install not yet run), ensure_global_config_dir
    // must succeed without error. Nothing is deployed — acceptable pre-install.
    #[test]
    fn test_deploy_agents_and_skills_missing_source_is_ok() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let claude_config = managed_root.join("claude-config");

        // Do NOT create framework/ — simulate a pre-install state.
        let result = ensure_global_config_dir(&managed_root, &claude_config);
        assert!(
            result.is_ok(),
            "missing framework source dirs must not cause an error: {result:?}"
        );
        // settings.json must still be seeded.
        assert!(
            claude_config.join("settings.json").exists(),
            "settings.json must exist even when no framework is installed"
        );
        // agents/ and skills/ are simply absent — nothing was deployed.
        assert!(
            !claude_config.join("agents").exists(),
            "agents dir must not be created when source is missing"
        );
        assert!(
            !claude_config.join("skills").exists(),
            "skills dir must not be created when source is missing"
        );
        // output-styles/ must always be populated (bundled constants, no install needed).
        assert!(
            claude_config.join("output-styles").exists(),
            "output-styles dir must be created even when no framework is installed"
        );
    }

    // WI-2 follow-up (#1553): ensure_global_config_dir must always deploy the
    // bundled output styles into <config_dir>/output-styles/ even when the
    // framework source dirs (agents, skills) are absent (pre-install state).
    // Output styles come from bundled constants, not framework root.
    #[test]
    fn test_deploy_output_styles_wired_into_config_dir() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let claude_config = managed_root.join("claude-config");

        ensure_global_config_dir(&managed_root, &claude_config).unwrap();

        let styles_dir = claude_config.join("output-styles");
        assert!(
            styles_dir.exists(),
            "output-styles dir must exist after ensure_global_config_dir"
        );

        // All bundled styles must be present.
        for style in crate::core::bundle::OUTPUT_STYLES {
            let target = styles_dir.join(style.file_name);
            assert!(
                target.exists(),
                "output-styles/{} must be deployed by ensure_global_config_dir",
                style.file_name
            );
            let content = std::fs::read_to_string(&target).unwrap();
            assert_eq!(
                content, style.content,
                "deployed output style {} must match bundled content",
                style.file_name
            );
        }
    }

    // WI-2 follow-up (#1553): output-style deploy must be idempotent — a second
    // call to deploy_output_styles must not rewrite style files whose content
    // is already current.  We assert via the DeployResult fields rather than
    // mtime comparisons (mtime checks are racy on coarse-grained filesystems).
    #[test]
    fn test_deploy_output_styles_idempotent_via_global_config() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let claude_config = managed_root.join("claude-config");

        // First call (via ensure_global_config_dir) seeds all output styles.
        ensure_global_config_dir(&managed_root, &claude_config).unwrap();

        // Second call to the deployer directly must report all files unchanged.
        let result =
            crate::core::output_style_deployer::deploy_output_styles(&claude_config).unwrap();
        assert!(
            result.deployed.is_empty(),
            "second deploy must not overwrite any style file (idempotent); \
             deployed: {:?}",
            result.deployed
        );
        assert_eq!(
            result.unchanged.len(),
            crate::core::bundle::OUTPUT_STYLES.len(),
            "all style files must be reported unchanged on second call; \
             unchanged: {:?}",
            result.unchanged
        );
    }
}
