//! Per-profile OAuth client credential storage (issue #3518).
//!
//! Why: A Google refresh token is bound to the OAuth client (client_id) that
//! issued it — refreshing it with a DIFFERENT client fails with
//! `invalid_client`/`invalid_grant`. Multi-account setups (#3502/#3503) often
//! want per-domain "Internal" Google Workspace apps rather than one shared
//! client, so each profile must be able to record — and every subsequent
//! refresh must reuse — the exact client it was authorized with.
//! What: Persists an optional per-profile client credentials file under
//! `~/.gworkspace-mcp/clients/<profile>.json` (same 0600 hardening as
//! `tokens.json`). [`resolve_client_creds_for_profile`] prefers this file,
//! falling back to the existing global [`resolve_client_creds`] (env vars,
//! then `oauth_client.json`) when the profile has none — so an
//! already-authorized profile with no per-profile client keeps working
//! completely unchanged (backward compatible, no migration).
//! Test: `resolve_prefers_per_profile_file_over_global`,
//! `resolve_falls_back_to_global_when_absent`,
//! `resolve_propagates_malformed_per_profile_file`,
//! `persist_and_resolve_roundtrip`, `persist_rejects_malformed_source`,
//! `persist_restricts_permissions_on_unix` (cfg(unix)),
//! `profile_client_source_reports_global_and_per_profile`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::flow::{ClientCreds, parse_client_creds_json, resolve_client_creds};
use crate::api::auth::perms::restrict_to_owner;

/// Directory holding per-profile OAuth client credential files.
///
/// Why: A `clients/` subdirectory (not flat files at the `.gworkspace-mcp/`
/// root) so a profile named e.g. `tokens` or `oauth_client` can never
/// collide with the existing fixed filenames there.
/// What: `~/.gworkspace-mcp/clients/`.
fn clients_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gworkspace-mcp")
        .join("clients")
}

/// The on-disk path for `profile`'s per-profile OAuth client file, if any.
///
/// What: `~/.gworkspace-mcp/clients/<profile>.json`. Existence is the only
/// signal of whether a per-profile client is configured — no separate index.
pub fn profile_client_path(profile: &str) -> PathBuf {
    clients_dir().join(format!("{profile}.json"))
}

/// Which OAuth client a profile actually uses — for diagnostics
/// (`list_accounts`, `doctor`), not for the refresh decision itself (that is
/// [`resolve_client_creds_for_profile`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientSource {
    /// No per-profile client file; falls back to the shared global client.
    Global,
    /// A per-profile client file exists at this path.
    PerProfile(PathBuf),
}

impl ClientSource {
    /// Human-readable label for MCP/CLI surfaces (`"global"` or
    /// `"per-profile (<path>)"`).
    pub fn label(&self) -> String {
        match self {
            ClientSource::Global => "global".to_string(),
            ClientSource::PerProfile(path) => format!("per-profile ({})", path.display()),
        }
    }
}

/// Cheap existence check — which client `profile` is configured to use.
///
/// Why: `list_accounts` / `doctor` need to *label* every profile without
/// paying for a full parse of every client file.
/// What: `PerProfile` iff the file exists; does not validate its contents
/// (a malformed per-profile file still shows as `PerProfile` here — the
/// content problem surfaces via [`resolve_client_creds_for_profile`] instead).
pub fn profile_client_source(profile: &str) -> ClientSource {
    let path = profile_client_path(profile);
    if path.exists() {
        ClientSource::PerProfile(path)
    } else {
        ClientSource::Global
    }
}

/// Resolve the OAuth client credentials to use for `profile`.
///
/// Why: THE correctness crux (issue #3518) — a refresh token is bound to the
/// client that issued it, so every refresh (and the initial authorization,
/// see `oauth::flow::run_consent_with`) for a profile with its own client
/// MUST use that exact client, never the global one.
/// What: Reads and parses `~/.gworkspace-mcp/clients/<profile>.json` if
/// present — any read/parse error here is returned, NOT swallowed:
/// misconfiguration must surface loudly rather than silently falling back to
/// a global client that would produce a confusing `invalid_client` error
/// instead. Falls back to [`resolve_client_creds`] (env vars, then the global
/// `oauth_client.json`) only when the profile has no per-profile file at all
/// — this is the backward-compat path for every profile authorized before
/// this feature existed.
/// Test: `resolve_prefers_per_profile_file_over_global`,
/// `resolve_falls_back_to_global_when_absent`,
/// `resolve_propagates_malformed_per_profile_file`.
pub fn resolve_client_creds_for_profile(profile: &str) -> Result<ClientCreds> {
    let path = profile_client_path(profile);
    if path.exists() {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("read per-profile OAuth client {}", path.display()))?;
        return parse_client_creds_json(&data)
            .with_context(|| format!("parse per-profile OAuth client {}", path.display()));
    }
    resolve_client_creds()
}

/// Validate and persist `source` as `profile`'s own OAuth client.
///
/// Why: Backs both the `setup --oauth-client <path>` CLI flag and the
/// `add_account` MCP tool's `oauth_client_path` argument — neither ever
/// accepts a client_id/secret inline (the MCP arg is a file path so raw
/// secrets never appear in a tool call or its logs).
/// What: Reads `source`, validates it parses as [`ClientCreds`] (flat or
/// Google's `installed`/`web` download shape) BEFORE writing anything, then
/// copies the raw bytes to [`profile_client_path`] and restricts the target
/// to owner-only (0600 on Unix), matching `tokens.json` hardening. Returns
/// the persisted path. Never logs the parsed secret values.
/// Test: `persist_and_resolve_roundtrip`, `persist_rejects_malformed_source`,
/// `persist_restricts_permissions_on_unix` (cfg(unix)).
pub fn persist_profile_client(profile: &str, source: &Path) -> Result<PathBuf> {
    let data = std::fs::read_to_string(source)
        .with_context(|| format!("read OAuth client file {}", source.display()))?;
    parse_client_creds_json(&data)
        .with_context(|| format!("invalid OAuth client JSON in {}", source.display()))?;

    let target = profile_client_path(profile);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    std::fs::write(&target, &data).with_context(|| format!("write {}", target.display()))?;
    restrict_to_owner(&target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::auth::test_support::{EnvGuard, fresh_temp_home};
    use serial_test::serial;

    const MUTATED_ENV_VARS: &[&str] = &[
        "HOME",
        "GOOGLE_OAUTH_CLIENT_ID",
        "GOOGLE_OAUTH_CLIENT_SECRET",
    ];

    /// Point `dirs::home_dir()` at a fresh temp dir with no env-var
    /// credentials set, so only file-based resolution is in play.
    fn isolated_home(label: &str) -> PathBuf {
        let home = fresh_temp_home(label);
        // SAFETY: caller runs under #[serial]; EnvGuard (held by the caller)
        // restores every var on drop.
        unsafe {
            std::env::remove_var("GOOGLE_OAUTH_CLIENT_ID");
            std::env::remove_var("GOOGLE_OAUTH_CLIENT_SECRET");
            std::env::set_var("HOME", &home);
        }
        home
    }

    fn write_client_json(path: &Path, client_id: &str, client_secret: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(
            path,
            format!(r#"{{"client_id":"{client_id}","client_secret":"{client_secret}"}}"#),
        )
        .unwrap();
    }

    #[test]
    #[serial]
    fn resolve_prefers_per_profile_file_over_global() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        let home = isolated_home("prefer-per-profile");
        write_client_json(
            &home.join(".gworkspace-mcp").join("oauth_client.json"),
            "global-id",
            "global-secret",
        );
        write_client_json(&profile_client_path("work"), "profile-id", "profile-secret");

        let creds = resolve_client_creds_for_profile("work").expect("resolve");
        assert_eq!(creds.client_id, "profile-id");
        assert_eq!(creds.client_secret, "profile-secret");
    }

    #[test]
    #[serial]
    fn resolve_falls_back_to_global_when_absent() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        let home = isolated_home("fallback-global");
        write_client_json(
            &home.join(".gworkspace-mcp").join("oauth_client.json"),
            "global-id",
            "global-secret",
        );

        let creds = resolve_client_creds_for_profile("legacy").expect("resolve");
        assert_eq!(
            creds.client_id, "global-id",
            "a profile with no per-profile client file must keep using the global one"
        );
    }

    #[test]
    #[serial]
    fn resolve_errors_when_neither_source_exists() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        isolated_home("no-creds-anywhere");

        let err = resolve_client_creds_for_profile("ghost").unwrap_err();
        assert!(
            err.to_string().contains("no OAuth client credentials"),
            "{err}"
        );
    }

    #[test]
    #[serial]
    fn resolve_propagates_malformed_per_profile_file() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        let home = isolated_home("malformed-per-profile");
        write_client_json(
            &home.join(".gworkspace-mcp").join("oauth_client.json"),
            "global-id",
            "global-secret",
        );
        let broken = profile_client_path("broken");
        std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
        std::fs::write(&broken, "not json").unwrap();

        let err = resolve_client_creds_for_profile("broken").unwrap_err();
        assert!(
            !err.to_string().contains("global-id"),
            "must not silently substitute the global client on a broken per-profile file: {err}"
        );
    }

    #[test]
    #[serial]
    fn persist_and_resolve_roundtrip() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        isolated_home("persist-roundtrip");
        let source_dir = std::env::temp_dir().join(format!("gw-src-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("client.json");
        write_client_json(&source, "new-id", "new-secret");

        let target = persist_profile_client("work", &source).expect("persist");
        assert_eq!(target, profile_client_path("work"));

        let creds = resolve_client_creds_for_profile("work").expect("resolve after persist");
        assert_eq!(creds.client_id, "new-id");
        assert_eq!(creds.client_secret, "new-secret");
        assert_eq!(
            profile_client_source("work"),
            ClientSource::PerProfile(target)
        );
    }

    #[test]
    #[serial]
    fn persist_rejects_malformed_source() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        isolated_home("persist-rejects");
        let source_dir = std::env::temp_dir().join(format!("gw-src-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("client.json");
        std::fs::write(&source, "not json").unwrap();

        let err = persist_profile_client("work", &source).unwrap_err();
        assert!(err.to_string().contains("invalid OAuth client"), "{err}");
        assert!(
            !profile_client_path("work").exists(),
            "a malformed source must never be written to the profile client path"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn persist_restricts_permissions_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        isolated_home("persist-perms");
        let source_dir = std::env::temp_dir().join(format!("gw-src-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("client.json");
        write_client_json(&source, "id", "secret");

        let target = persist_profile_client("work", &source).expect("persist");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    #[test]
    #[serial]
    fn profile_client_source_reports_global_and_per_profile() {
        let _guard = EnvGuard::capture(MUTATED_ENV_VARS);
        isolated_home("source-labels");

        assert_eq!(profile_client_source("no-file"), ClientSource::Global);
        assert_eq!(profile_client_source("no-file").label(), "global");

        write_client_json(&profile_client_path("work"), "id", "secret");
        match profile_client_source("work") {
            ClientSource::PerProfile(p) => assert_eq!(p, profile_client_path("work")),
            ClientSource::Global => panic!("expected PerProfile"),
        }
        assert!(
            profile_client_source("work")
                .label()
                .starts_with("per-profile (")
        );
    }
}
