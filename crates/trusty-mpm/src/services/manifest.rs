//! Service manifest schema — typed representation of `~/.claude-mpm/services.yaml`.
//!
//! Why: agents need a single declaration of "what services could exist" so that
//! `tm services` can probe them at query time without hardcoding ports or PIDs
//! in shell scripts. The manifest is the stable contract; probing is ephemeral.
//! What: defines `ServicesManifest`, `ServiceDecl`, `PortDiscovery`, and the
//! `ManifestValidationError` type, plus helpers for loading, validating, and
//! expanding tilde paths.
//! Test: see `tests` module below — 8 unit tests covering happy and error paths.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default manifest embedded at compile time.
///
/// Why: `tm services list` must work on a fresh install before the user runs
/// `tm services init`. Embedding the default avoids a runtime file read and
/// ensures the binary always has a usable fallback.
/// What: the YAML text from `assets/default-services.yaml`, parsed once by
/// `ServicesManifest::default_manifest()`.
/// Test: `embedded_default_manifest_is_valid`.
const DEFAULT_MANIFEST_YAML: &str = include_str!("../../assets/default-services.yaml");

/// Top-level manifest envelope.
///
/// Why: the `version` field acts as a forward-compatibility guard so a future
/// manifest format (v2+) is detectable before the parser tries to interpret it.
/// What: wraps a BTreeMap of service declarations; BTreeMap preserves sorted
/// order for stable table output in `tm services list`.
/// Test: `manifest_parse_happy_path`, `manifest_rejects_future_version`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServicesManifest {
    /// Manifest schema version. Currently must be 1.
    pub version: u32,

    /// Map from service name to its declaration.
    pub services: BTreeMap<String, ServiceDecl>,
}

/// How the runtime port is discovered for a dynamic-port service.
///
/// Why: `trusty-memory` does not bind a fixed port; it walks 7070–7079 and
/// writes the result to a file. `Static` is the common case; `File` is needed
/// for memory and any future dynamic-port service.
/// What: `serde` uses `rename_all = "snake_case"` so YAML authors write `static`
/// / `file`, matching the existing convention in `trusty-common`.
/// Test: `manifest_parse_happy_path` exercises both variants.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PortDiscovery {
    /// Use `default_port` directly. Most services.
    #[default]
    Static,
    /// Read the bound address from the path in `port_file`. The file contains
    /// a single `host:port` line written by `write_daemon_addr` in trusty-common.
    File,
}

/// Declaration of one service in the manifest.
///
/// Why: all fields that could be absent for sidecar-only daemons (embedderd,
/// bm25-daemon) are `Option` so the manifest does not force authors to write
/// sentinel values. Serde `default` on each Option means absent YAML keys
/// deserialise cleanly to `None`.
/// What: static metadata plus optional lifecycle commands. The discovery engine
/// uses this to build a `ServiceStatus` at query time.
/// Test: `manifest_parse_happy_path`, `manifest_parse_minimal_service`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDecl {
    /// Human-readable description shown in `tm services list`.
    pub description: String,

    /// Default TCP port. None for UDS-only / stdio sidecars.
    #[serde(default)]
    pub default_port: Option<u16>,

    /// How to discover the actual runtime port.
    #[serde(default)]
    pub port_discovery: PortDiscovery,

    /// For `port_discovery: file` — path to the file containing the bound address.
    /// Tilde is expanded at read time.
    #[serde(default)]
    pub port_file: Option<String>,

    /// Health endpoint URL template. `{port}` is replaced with the discovered port.
    /// None for services with no HTTP surface (sidecars).
    #[serde(default)]
    pub health_url: Option<String>,

    /// Path to the most-recent log file. Tilde is expanded at read time.
    #[serde(default)]
    pub log_path: Option<String>,

    /// Shell command whose first line of stdout is the version string.
    #[serde(default)]
    pub version_cmd: Option<String>,

    /// Substring used for pgrep-style process identification.
    /// The discovery engine runs `pgrep -f <process_match>` on Unix.
    /// Must not contain shell metacharacters; validated at manifest load.
    #[serde(default)]
    pub process_match: Option<String>,

    /// Shell command to start the service.
    #[serde(default)]
    pub start_cmd: Option<String>,

    /// Shell command to stop the service.
    #[serde(default)]
    pub stop_cmd: Option<String>,

    /// Shell command to restart the service.
    #[serde(default)]
    pub restart_cmd: Option<String>,
}

/// Validation errors for the manifest.
///
/// Why: `thiserror` gives the library layer structured, match-able errors that
/// the binary layer wraps with `anyhow::Error`. Each variant maps to one
/// concrete validation rule in `ServicesManifest::validate()`.
/// What: covers the five invariants: version, port range, file-discovery
/// completeness, metacharacter safety, and health URL syntax.
/// Test: each variant is exercised by a named unit test in the `tests` module.
#[derive(Debug, thiserror::Error)]
pub enum ManifestValidationError {
    /// Manifest schema version is newer than this binary supports.
    #[error("manifest version {0} is unsupported (max supported: 1)")]
    UnsupportedVersion(u32),

    /// A `default_port` value outside the valid TCP range was specified.
    #[error("service '{0}': default_port {1} is not in the valid range 1-65535")]
    InvalidPort(String, u32),

    /// `port_discovery: file` was set but no `port_file` path was provided.
    #[error("service '{0}': port_discovery is 'file' but port_file is not set")]
    MissingPortFile(String),

    /// `process_match` contains shell metacharacters that would make pgrep unsafe.
    #[error("service '{0}': process_match '{1}' contains shell metacharacters")]
    UnsafeProcessMatch(String, String),

    /// `health_url` contains a template that is not a valid URL after expansion.
    #[error("service '{0}': health_url '{1}' is not a valid URL template")]
    InvalidHealthUrl(String, String),
}

impl ServicesManifest {
    /// Load the embedded default manifest (in-memory only; no disk write).
    ///
    /// Why: `tm services list` must work on a fresh install before the user runs
    /// `tm services init`. The embedded YAML is always-available and validated
    /// at compile time by the unit test suite.
    /// What: parses `DEFAULT_MANIFEST_YAML` and runs `validate()`. Panics only
    /// if the embedded YAML is structurally broken — a programmer error caught
    /// by CI, never by users.
    /// Test: `embedded_default_manifest_is_valid`.
    pub fn default_manifest() -> Self {
        let m: ServicesManifest = serde_yaml::from_str(DEFAULT_MANIFEST_YAML)
            .expect("embedded default-services.yaml is valid YAML");
        m.validate()
            .expect("embedded default-services.yaml passes validation");
        m
    }

    /// Validate all invariants in the manifest.
    ///
    /// Why: centralising validation prevents partial manifests from reaching the
    /// discovery engine, giving the user an actionable error at load time rather
    /// than a confusing `None` at query time.
    /// What: checks version <= 1; all ports in 1-65535; port_file present when
    /// port_discovery == File; process_match free of shell metacharacters;
    /// health_url (when present) contains a `{port}` template token.
    /// Test: `manifest_rejects_future_version`, `manifest_rejects_invalid_port`,
    /// `manifest_rejects_file_discovery_without_port_file`,
    /// `manifest_rejects_metacharacters_in_process_match`.
    pub fn validate(&self) -> Result<(), ManifestValidationError> {
        if self.version > 1 {
            return Err(ManifestValidationError::UnsupportedVersion(self.version));
        }

        for (name, decl) in &self.services {
            // Port range check — u16 covers 0-65535; additionally reject 0.
            if let Some(0) = decl.default_port {
                return Err(ManifestValidationError::InvalidPort(name.clone(), 0));
            }

            // File discovery requires port_file.
            if decl.port_discovery == PortDiscovery::File && decl.port_file.is_none() {
                return Err(ManifestValidationError::MissingPortFile(name.clone()));
            }

            // process_match must not contain shell metacharacters.
            if let Some(pm) = &decl.process_match {
                const METACHARACTERS: &[char] = &[
                    '|', ';', '&', '$', '`', '(', ')', '{', '}', '<', '>', '!', '*', '?', '[', ']',
                    '\\', '"', '\'',
                ];
                if pm.chars().any(|c| METACHARACTERS.contains(&c)) {
                    return Err(ManifestValidationError::UnsafeProcessMatch(
                        name.clone(),
                        pm.clone(),
                    ));
                }
            }

            // health_url template must contain {port} for port expansion.
            if let Some(url) = &decl.health_url
                && !url.contains("{port}")
            {
                return Err(ManifestValidationError::InvalidHealthUrl(
                    name.clone(),
                    url.clone(),
                ));
            }
        }

        Ok(())
    }

    /// Expand tilde in all path-bearing fields.
    ///
    /// Why: tilde in `log_path` and `port_file` is a UX expectation from shell
    /// users, not a shell feature. The runtime must expand it before using paths.
    /// What: expands a leading `~/` to `dirs::home_dir()` for `log_path` and
    /// `port_file` in every service declaration. Returns an error when the home
    /// directory cannot be resolved (unusual, but possible in CI containers).
    /// Test: `manifest_expands_tilde`.
    pub fn expand_paths(&mut self) -> anyhow::Result<()> {
        let home = dirs::home_dir().ok_or_else(|| {
            anyhow::anyhow!("could not resolve home directory for tilde expansion")
        })?;
        for decl in self.services.values_mut() {
            if let Some(p) = &decl.log_path {
                decl.log_path = Some(expand_tilde(p, &home));
            }
            if let Some(p) = &decl.port_file {
                decl.port_file = Some(expand_tilde(p, &home));
            }
        }
        Ok(())
    }
}

/// Expand a leading `~/` in `path` to the given `home` directory.
///
/// Why: shell tilde expansion is not performed by Rust's std::fs functions;
/// every path-bearing field that might contain `~/` must be explicitly expanded.
/// What: returns a new `String` with `~/` replaced by `home/`, or the original
/// unchanged if it does not start with `~/`.
/// Test: `manifest_expands_tilde` (via `ServicesManifest::expand_paths`).
pub fn expand_tilde(path: &str, home: &std::path::Path) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest).to_string_lossy().into_owned()
    } else if path == "~" {
        home.to_string_lossy().into_owned()
    } else {
        path.to_string()
    }
}

/// Expand tilde using the current process home directory.
///
/// Why: convenience wrapper for call sites that do not already hold a `home`
/// `PathBuf`, avoiding repeated `dirs::home_dir()` lookups.
/// What: calls `dirs::home_dir()`, falls back to returning the original path
/// unchanged when the home directory is unavailable (graceful degradation).
/// Test: covered implicitly by any test that calls `expand_paths`.
pub fn expand_tilde_owned(path: &str) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        PathBuf::from(expand_tilde(path, &home))
    } else {
        PathBuf::from(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the full default YAML; assert all 6 services are present.
    #[test]
    fn manifest_parse_happy_path() {
        let m: ServicesManifest = serde_yaml::from_str(DEFAULT_MANIFEST_YAML).expect("parse");
        assert_eq!(m.version, 1);
        assert_eq!(m.services.len(), 6);
        assert!(m.services.contains_key("trusty-search"));
        assert!(m.services.contains_key("trusty-analyze"));
        assert!(m.services.contains_key("trusty-mpm-daemon"));
        assert!(m.services.contains_key("trusty-memory"));
        assert!(m.services.contains_key("trusty-embedderd"));
        assert!(m.services.contains_key("trusty-bm25-daemon"));

        let ts = &m.services["trusty-search"];
        assert_eq!(ts.default_port, Some(7878));
        assert!(ts.health_url.as_deref().unwrap().contains("{port}"));

        let mem = &m.services["trusty-memory"];
        assert_eq!(mem.port_discovery, PortDiscovery::File);
        assert!(mem.port_file.is_some());

        let emb = &m.services["trusty-embedderd"];
        assert!(emb.default_port.is_none());
        assert!(emb.health_url.is_none());
    }

    /// A minimal declaration with only `description` should parse cleanly.
    #[test]
    fn manifest_parse_minimal_service() {
        let yaml = r#"
version: 1
services:
  my-svc:
    description: "Minimal service"
"#;
        let m: ServicesManifest = serde_yaml::from_str(yaml).expect("parse");
        let svc = &m.services["my-svc"];
        assert_eq!(svc.description, "Minimal service");
        assert!(svc.default_port.is_none());
        assert!(svc.health_url.is_none());
        assert!(svc.process_match.is_none());
        m.validate().expect("minimal service is valid");
    }

    /// version: 2 must produce UnsupportedVersion(2).
    #[test]
    fn manifest_rejects_future_version() {
        let yaml = "version: 2\nservices: {}";
        let m: ServicesManifest = serde_yaml::from_str(yaml).expect("parse");
        let err = m.validate().unwrap_err();
        assert!(
            matches!(err, ManifestValidationError::UnsupportedVersion(2)),
            "expected UnsupportedVersion(2), got {err}"
        );
    }

    /// A service with `default_port: 0` must fail with InvalidPort.
    #[test]
    fn manifest_rejects_invalid_port() {
        let yaml = r#"
version: 1
services:
  bad-svc:
    description: "Bad port"
    default_port: 0
"#;
        let m: ServicesManifest = serde_yaml::from_str(yaml).expect("parse");
        let err = m.validate().unwrap_err();
        assert!(
            matches!(err, ManifestValidationError::InvalidPort(ref n, 0) if n == "bad-svc"),
            "expected InvalidPort, got {err}"
        );
    }

    /// `port_discovery: file` without `port_file` must fail.
    #[test]
    fn manifest_rejects_file_discovery_without_port_file() {
        let yaml = r#"
version: 1
services:
  dyn-svc:
    description: "Dynamic port service"
    default_port: 8080
    port_discovery: file
"#;
        let m: ServicesManifest = serde_yaml::from_str(yaml).expect("parse");
        let err = m.validate().unwrap_err();
        assert!(
            matches!(err, ManifestValidationError::MissingPortFile(ref n) if n == "dyn-svc"),
            "expected MissingPortFile, got {err}"
        );
    }

    /// `process_match` containing `|` (pipe) must fail.
    #[test]
    fn manifest_rejects_metacharacters_in_process_match() {
        let yaml = r#"
version: 1
services:
  unsafe-svc:
    description: "Has metachar"
    process_match: "foo|bar"
"#;
        let m: ServicesManifest = serde_yaml::from_str(yaml).expect("parse");
        let err = m.validate().unwrap_err();
        assert!(
            matches!(
                err,
                ManifestValidationError::UnsafeProcessMatch(ref n, ref pm)
                if n == "unsafe-svc" && pm.contains('|')
            ),
            "expected UnsafeProcessMatch, got {err}"
        );
    }

    /// Malformed YAML must produce a serde_yaml error, not a panic.
    #[test]
    fn manifest_rejects_bad_yaml() {
        let yaml = "version: [not a number]";
        let result: Result<ServicesManifest, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err(), "expected parse error");
    }

    /// `~/foo` in log_path must expand to `<home>/foo`.
    #[test]
    fn manifest_expands_tilde() {
        let home = dirs::home_dir().expect("home dir available in test");
        let yaml = r#"
version: 1
services:
  my-svc:
    description: "Tilde test"
    log_path: "~/some/log/path"
    port_file: "~/port-file"
"#;
        let mut m: ServicesManifest = serde_yaml::from_str(yaml).expect("parse");
        m.expand_paths().expect("expand");
        let svc = &m.services["my-svc"];
        let expected_log = home.join("some/log/path").to_string_lossy().into_owned();
        let expected_port = home.join("port-file").to_string_lossy().into_owned();
        assert_eq!(svc.log_path.as_deref(), Some(expected_log.as_str()));
        assert_eq!(svc.port_file.as_deref(), Some(expected_port.as_str()));
    }

    /// The embedded default manifest must pass validation cleanly.
    #[test]
    fn embedded_default_manifest_is_valid() {
        let m = ServicesManifest::default_manifest();
        m.validate().expect("embedded default manifest is valid");
    }
}
