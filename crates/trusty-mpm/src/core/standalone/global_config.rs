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
//! — see WI-10 for the auth model), writes a minimal `.mcp.json` with
//! trusty-memory, trusty-review, and trusty-search server stubs, and (WI-2)
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
/// otherwise), writes `.mcp.json` with trusty-memory, trusty-review, and
/// trusty-search stubs (idempotent via the inject pattern; WI-8 adds
/// trusty-review), then deploys bundled agents and skills via
/// [`deploy_agents_and_skills`].
/// Test: `test_global_config_dir_ensure_idempotent`,
/// `test_deploy_agents_and_skills_populates_config_dir`,
/// `test_deploy_agents_and_skills_missing_source_is_ok`,
/// `test_global_config_dir_seeds_output_style_and_status_line`.
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
    ensure_mcp_config(claude_config_dir)?;
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
    crate::core::output_style_deployer::deploy_output_styles(claude_config_dir).map_err(|e| {
        anyhow::anyhow!("failed to deploy output styles into managed config dir: {e}")
    })?;

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

/// Write a minimal `.mcp.json` with trusty-memory, trusty-review, and trusty-search stubs.
///
/// Why: the tm-global config dir must carry the global MCP server definitions
/// so every tm-launched session can use memory, review, and search tools without
/// per-project wiring. `trusty-review` was added in WI-8 (refs #1548).
/// What: reads `<claude_config_dir>/.mcp.json` (starts from `{}` when absent),
/// injects `trusty-memory`, `trusty-review`, and `trusty-search` entries under
/// `mcpServers` using the same idempotent merge used by `inject_mcp_server` in
/// settings.rs. To avoid spurious mtime bumps on every `tm run`, the on-disk
/// content is parsed into a `serde_json::Value` and compared structurally
/// (not byte-wise) to the merged value; the file is only written when the
/// parsed Values differ (closes #1550 item 3). Byte-wise comparison would be
/// defeated by any trailing newline or whitespace difference left by editors or
/// prior writes; structural comparison is immune to those formatting variations.
/// Secrets/env: trusty-review reads its config from the environment
/// (OPENROUTER_API_KEY, AWS credentials, TRUSTY_SEARCH_URL etc.) — no secrets
/// are injected here; the managed session inherits the daemon env, matching the
/// pattern used for trusty-memory and trusty-search.
/// Palace pinning (issue #1651): the trusty-memory entry here is DELIBERATELY a
/// bare `serve --stdio` stub with NO `env.TRUSTY_MEMORY_PALACE`. This is the
/// SINGLE tm-global config dir (SPEC-STANDALONE-MPM-08), shared as the
/// user-level MCP layer across EVERY alias, so it cannot carry any one project's
/// palace slug. The per-project palace pin lives in the cloned repo's
/// `repo/.mcp.json` (written by `load_alias` → `run_prepare_session` →
/// `prepare_session_with_repo_url`, threading the registry clone URL), which
/// Claude Code layers OVER this global stub (project-local precedence, A4). So
/// the correct slug is always applied at the project layer; this global stub
/// only declares server availability and must stay bare.
/// Test: `test_global_config_dir_ensure_idempotent`,
/// `test_mcp_config_contains_all_three_servers`,
/// `test_mcp_config_no_spurious_write_when_unchanged`,
/// `test_mcp_config_trailing_newline_does_not_trigger_rewrite`.
fn ensure_mcp_config(claude_config_dir: &Path) -> anyhow::Result<()> {
    let mcp_path = claude_config_dir.join(".mcp.json");

    // Parse the on-disk value for structural comparison.  On parse failure
    // (malformed file) or absence we treat the existing state as "empty object"
    // so the merge proceeds and the file is (re)written correctly.
    let existing_value: Option<serde_json::Value> = std::fs::read_to_string(&mcp_path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter(|v| v.is_object());

    let mut config = existing_value
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));

    let servers = config
        .as_object_mut()
        .expect("config is an object")
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        *servers = serde_json::json!({});
    }
    let servers = servers.as_object_mut().expect("mcpServers is an object");

    // The three built-in framework servers' launch definitions live in a single
    // catalog (`core::mcp_config::builtin_server_entry`) so this managed-config
    // wiring and `tm mcp test`'s probe can never disagree on how a server starts.
    // trusty-review reads its LLM credentials (OPENROUTER_API_KEY, AWS credentials)
    // and TRUSTY_SEARCH_URL from the environment — no secrets are injected here;
    // the managed session inherits the daemon env, matching the memory/search pattern.
    for name in crate::core::mcp_config::BUILTIN_MANAGED_MCP_SERVERS {
        if let Some(entry) = crate::core::mcp_config::builtin_server_entry(name) {
            servers.entry(*name).or_insert(entry);
        }
    }

    // Compare structurally (Value == Value) rather than byte-wise so that
    // trailing newlines, editor-inserted whitespace, or any other formatting
    // difference in the on-disk file does NOT trigger a spurious rewrite.
    // A byte comparison of `existing_text` vs the fresh serialization would
    // fail whenever the on-disk file has a trailing newline, defeating the
    // idempotency guarantee this function is supposed to provide.
    let needs_write = existing_value.as_ref() != Some(&config);

    if needs_write {
        let serialized = serde_json::to_string_pretty(&config)?;
        std::fs::write(&mcp_path, &serialized)?;
    }
    Ok(())
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
        assert!(claude_config.join(".mcp.json").exists());

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

    // WI-3 + WI-8: ensure_mcp_config must define all three managed MCP servers,
    // including trusty-review added in WI-8 (refs #1548).
    #[test]
    fn test_mcp_config_contains_all_three_servers() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        ensure_mcp_config(&cfg).unwrap();
        let text = std::fs::read_to_string(cfg.join(".mcp.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        let servers = val["mcpServers"].as_object().unwrap();
        assert!(
            servers.contains_key("trusty-memory"),
            "trusty-memory must be defined in .mcp.json"
        );
        assert!(
            servers.contains_key("trusty-review"),
            "trusty-review must be defined in .mcp.json (WI-8)"
        );
        assert!(
            servers.contains_key("trusty-search"),
            "trusty-search must be defined in .mcp.json"
        );
    }

    // WI-8: trusty-review server entry must use stdio transport and the correct command/args.
    #[test]
    fn test_mcp_config_review_server_entry() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        ensure_mcp_config(&cfg).unwrap();
        let text = std::fs::read_to_string(cfg.join(".mcp.json")).unwrap();
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

    // WI-8 ISOLATION: ensure_mcp_config must write the review entry without
    // touching any file outside the given claude_config_dir.
    #[serial_test::serial]
    #[test]
    fn test_mcp_config_review_no_home_write() {
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

        ensure_mcp_config(&cfg).unwrap();

        // .mcp.json must exist inside cfg.
        assert!(
            cfg.join(".mcp.json").exists(),
            "ensure_mcp_config must write .mcp.json inside the given dir"
        );
        // Nothing must land directly under root (outside cfg).
        assert!(
            !root.path().join(".mcp.json").exists(),
            "ensure_mcp_config must NOT write .mcp.json outside the given dir (isolation)"
        );
        // $HOME must remain empty — no .claude.json or .claude/ created.
        assert!(
            !fake_home.path().join(".mcp.json").exists(),
            "ensure_mcp_config must NOT write .mcp.json to $HOME (isolation)"
        );
        assert!(
            !fake_home.path().join(".claude").exists(),
            "ensure_mcp_config must NOT create $HOME/.claude/ (isolation)"
        );
        // _home_guard drops here and restores HOME.
    }

    // #1550 item 3: ensure_mcp_config must NOT rewrite .mcp.json when the
    // content is already correct.  We detect a write by comparing the raw bytes
    // before and after — if the guard regresses the bytes will change.
    // (mtime-based assertions are unreliable on coarse-grained CI filesystems.)
    #[test]
    fn test_mcp_config_no_spurious_write_when_unchanged() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        let mcp_path = cfg.join(".mcp.json");

        // First call writes the canonical file.
        ensure_mcp_config(&cfg).unwrap();
        let bytes_after_first = std::fs::read(&mcp_path).unwrap();

        // Second call must leave the file byte-identical (no write occurred).
        ensure_mcp_config(&cfg).unwrap();
        let bytes_after_second = std::fs::read(&mcp_path).unwrap();

        assert_eq!(
            bytes_after_first, bytes_after_second,
            "ensure_mcp_config must not rewrite .mcp.json when content is unchanged (#1550 item 3)"
        );
    }

    // #1550 item 3 regression: a file with a trailing newline (left by an
    // editor or a prior write) is logically identical to the same JSON without
    // the newline.  The structural comparison must treat them as equal and must
    // NOT rewrite the file.  A byte-wise comparison would fail here, defeating
    // idempotency.  This test directly covers the Finding-1 regression scenario.
    #[test]
    fn test_mcp_config_trailing_newline_does_not_trigger_rewrite() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        let mcp_path = cfg.join(".mcp.json");

        // Write the canonical content, then append a trailing newline to
        // simulate what an editor or prior implementation might have left.
        ensure_mcp_config(&cfg).unwrap();
        let canonical = std::fs::read_to_string(&mcp_path).unwrap();
        let with_trailing_newline = format!("{canonical}\n");
        std::fs::write(&mcp_path, with_trailing_newline.as_bytes()).unwrap();

        // Capture the bytes as they are now (with the extra newline).
        let bytes_before = std::fs::read(&mcp_path).unwrap();

        // ensure_mcp_config must recognise the file is structurally up-to-date
        // and must NOT rewrite it.
        ensure_mcp_config(&cfg).unwrap();

        let bytes_after = std::fs::read(&mcp_path).unwrap();
        assert_eq!(
            bytes_before, bytes_after,
            "trailing newline in .mcp.json must not trigger a spurious rewrite \
             (structural comparison must tolerate formatting differences, refs #1550 Finding-1)"
        );
    }

    // #1550 item 3: ensure_mcp_config is idempotent across two calls — content
    // must be byte-identical after both calls.
    #[test]
    fn test_mcp_config_idempotent_content() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        ensure_mcp_config(&cfg).unwrap();
        let text_first = std::fs::read_to_string(cfg.join(".mcp.json")).unwrap();

        ensure_mcp_config(&cfg).unwrap();
        let text_second = std::fs::read_to_string(cfg.join(".mcp.json")).unwrap();

        assert_eq!(
            text_first, text_second,
            "ensure_mcp_config must produce byte-identical output on repeated calls (#1550 item 3)"
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
