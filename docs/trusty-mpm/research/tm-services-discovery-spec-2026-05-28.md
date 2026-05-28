# `tm services` — Canonical Service-Discovery CLI: Engineering Spec

**Date**: 2026-05-28
**Author**: Research agent
**Status**: DRAFT — ready for engineer implementation
**Tracking issue**: [#339](https://github.com/bobmatnyc/trusty-tools/issues/339)
**Binary home**: `crates/trusty-mpm/src/bin/tm.rs` (the `tm` / `trusty-mpm` unified binary)

---

## 1. Background and Goals

### Problem

Agents (Claude Code subagents — `local-ops`, `qa`, `rust-engineer`) currently answer
"is this service running?" and "what port is it on?" through four ad-hoc patterns:

- `lsof -i :PORT` — requires knowing the port ahead of time
- `curl localhost:PORT/health` — requires knowing the health URL
- `pgrep -f <daemon>` — fragile against binary renames
- `ps aux | grep <name>` — noisy; ambiguous on shared machines

Each agent re-invents discovery; results are inconsistent; port numbers are
hardcoded in prompts and agent instructions that quickly drift out of date.

The MPM PM instructions already prohibit `curl/lsof/ps` for the PM agent
(`.claude-mpm/PM_INSTRUCTIONS.md` line 253: `curl/lsof/ps/make -> CB#7`) but
ops and QA agents have no canonical alternative — they use the exact commands
the PM is forbidden from running.

### Goals

1. Provide a single canonical command (`tm services`) that any agent can call
   to answer all service questions: is it running, what port, what URL, what version.
2. Maintain a YAML manifest (`~/.claude-mpm/services.yaml`) as the declaration of
   "what could exist" and probe at query time for "what is running right now."
3. Deliver scriptable, machine-parseable output (`--json`, `tm services port <name>`)
   so agents can substitute `tm services port trusty-search` for hardcoded port numbers.
4. After implementation: redirect all agents via `.claude-mpm/` prompt updates
   so `lsof`/`curl`/`ps` for service questions disappear from the agent surface.

---

## 2. Default Service Manifest

The manifest lives at `~/.claude-mpm/services.yaml`. It is auto-installed on
first `tm services` invocation if absent. Engineers copy this block verbatim as
the embedded default template.

### Daemon port and transport inventory (verified against source)

| Daemon | Default port | Transport | Health URL | Port source |
|--------|-------------|-----------|------------|-------------|
| `trusty-search` | 7878 | HTTP | `GET /health` | `crates/trusty-search/src/main.rs:368,422` |
| `trusty-analyze` | 7879 | HTTP | `GET /health` | `crates/trusty-analyze/src/main.rs:73,159,188,190` |
| `trusty-mpm-daemon` | 7880 | HTTP | `GET /health` | `crates/trusty-mpm/src/bin/tm.rs:21` (`DEFAULT_URL = "http://127.0.0.1:7880"`) |
| `trusty-memory` | dynamic (7070–7079) | HTTP + UDS | `GET /health` | `crates/trusty-memory/src/lib.rs:1208,1248` (port discovered via `~/.trusty-memory/http_addr`) |
| `trusty-embedderd` | none (stdio/UDS sidecar) | UDS or stdio | none | `crates/trusty-embedderd/src/lib.rs:214,299,304` — spawned by trusty-search, no HTTP |
| `trusty-bm25-daemon` | none (UDS sidecar) | UDS | none | `crates/trusty-bm25-daemon/src/socket.rs:9,26,64` — `$TMPDIR/trusty-bm25-<palace>.sock` |

**Critical complications for discovery** (addressed in §9 Open Questions):
- `trusty-memory` uses dynamic port selection from range 7070–7079 and writes the
  bound address to `~/.trusty-memory/http_addr` (or macOS-standard
  `~/Library/Application Support/trusty-memory/http_addr`). Discovery must read
  this file, not probe a fixed port.
- `trusty-embedderd` and `trusty-bm25-daemon` are internal sidecars with no HTTP
  surface. They can only be probed via process matching; no health URL exists.
- `trusty-mpm-daemon` additionally writes a lock file; see `resolve_daemon_url`
  in `crates/trusty-mpm/src/core/` for the lock-file-based port discovery pattern.

### Manifest YAML (verbatim default)

```yaml
# ~/.claude-mpm/services.yaml
# Auto-installed by `tm services` on first run. Edit freely to add custom services.
version: 1

services:
  trusty-search:
    description: "Hybrid BM25 + vector + KG code search daemon"
    default_port: 7878
    health_url: "http://localhost:{port}/health"
    log_path: "~/Library/Logs/trusty-search/stderr.log"
    version_cmd: "trusty-search --version"
    process_match: "trusty-search"
    start_cmd: "trusty-search start"
    stop_cmd: "trusty-search stop"
    restart_cmd: "trusty-search stop && trusty-search start"

  trusty-analyze:
    description: "Code analysis sidecar daemon (complexity, smells, quality)"
    default_port: 7879
    health_url: "http://localhost:{port}/health"
    log_path: "~/.trusty-analyze/logs/stderr.log"
    version_cmd: "trusty-analyze --version"
    process_match: "trusty-analyze"
    start_cmd: "trusty-analyze start"
    stop_cmd: "trusty-analyze stop"
    restart_cmd: "trusty-analyze stop && trusty-analyze start"

  trusty-mpm-daemon:
    description: "MPM background orchestrator (sessions, hooks, circuit breaker)"
    default_port: 7880
    health_url: "http://localhost:{port}/health"
    log_path: "~/.trusty-mpm/logs/trusty-mpm.log"
    version_cmd: "tm --version"
    process_match: "trusty-mpmd"
    start_cmd: "tm daemon"
    stop_cmd: "tm stop"
    restart_cmd: "tm restart"

  trusty-memory:
    description: "Memory palace MCP server (dynamic port 7070-7079)"
    default_port: 7070
    port_discovery: "file"
    port_file: "~/.trusty-memory/http_addr"
    health_url: "http://localhost:{port}/health"
    log_path: null
    version_cmd: "trusty-memory --version"
    process_match: "trusty-memory"
    start_cmd: "trusty-memory serve"
    stop_cmd: null
    restart_cmd: null

  trusty-embedderd:
    description: "ONNX embedding sidecar (spawned by trusty-search, no HTTP)"
    default_port: null
    health_url: null
    log_path: null
    version_cmd: "trusty-embedderd --version"
    process_match: "trusty-embedderd"
    start_cmd: null
    stop_cmd: null
    restart_cmd: null

  trusty-bm25-daemon:
    description: "BM25 index sidecar (UDS at $TMPDIR/trusty-bm25-<palace>.sock)"
    default_port: null
    health_url: null
    log_path: null
    version_cmd: null
    process_match: "trusty-bm25-"
    start_cmd: null
    stop_cmd: null
    restart_cmd: null
```

**Notes on log paths**:
- `trusty-search` uses macOS launchd log dir: `~/Library/Logs/trusty-search/stderr.log`
  (source: `crates/trusty-search/src/commands/service.rs:58-62`)
- `trusty-analyze` uses `~/.trusty-analyze/logs/` (source: `crates/trusty-analyze/src/commands/service.rs:75-79`)
- `trusty-mpm-daemon` uses rolling daily logs at `~/.trusty-mpm/logs/trusty-mpm.log`
  (source: `crates/trusty-mpm/src/bin/trusty-mpmd.rs:47-53`)
- `trusty-memory` does not currently write a dedicated log file (UDS-based; logs to stderr only)
- Sidecar daemons (`trusty-embedderd`, `trusty-bm25-daemon`) pipe through their parent's stderr

---

## 3. Manifest Schema (Strict)

```rust
// crates/trusty-mpm/src/services/manifest.rs

use std::collections::BTreeMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// Why: serde_yaml is already a workspace dep (Cargo.toml:47). Using it keeps
/// manifest loading zero-new-deps and consistent with the rest of trusty-mpm's
/// YAML handling (config, help.yaml, etc.).
/// What: top-level manifest envelope. The `version` field is a forward-compat
/// guard — parsers must reject manifests with version > 1.
/// Test: `manifest_parse_happy_path`, `manifest_rejects_future_version`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServicesManifest {
    /// Manifest schema version. Currently must be 1.
    pub version: u32,

    /// Map from service name to its declaration.
    /// BTreeMap preserves insertion order for stable table output.
    pub services: BTreeMap<String, ServiceDecl>,
}

/// How the runtime port is discovered for a dynamic-port service.
///
/// Why: trusty-memory does not bind a fixed port; it walks 7070–7079 and
/// writes the result to a file. Static port binding is the common case;
/// file-based discovery is needed for memory and any future dynamic-port service.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PortDiscovery {
    /// Use `default_port` directly. Most services.
    #[default]
    Static,
    /// Read the bound address from the path in `port_file`. The file contains
    /// a single `host:port` line (matching the `write_daemon_addr` convention
    /// in `crates/trusty-common/src/lib.rs:375-415`).
    File,
}

/// Declaration of one service in the manifest.
///
/// Why: all fields that could be absent for sidecar-only daemons (embedderd,
/// bm25-daemon) are `Option` so the manifest does not force authors to write
/// sentinel values. Serde `default` on each Option means absent YAML keys
/// deserialise cleanly to `None`.
/// What: static metadata plus optional lifecycle commands. The discovery engine
/// (§4) uses this to build a `ServiceStatus` at query time.
/// Test: `manifest_parse_happy_path`, `manifest_parse_minimal_service`,
/// `manifest_rejects_missing_required_field`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDecl {
    /// Human-readable description shown in `tm services list`.
    pub description: String,

    /// Default TCP port. None for UDS-only / stdio sidecars.
    /// Validation: when Some, must be in range 1..=65535.
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
    /// None when logging goes through the parent process or launchd.
    #[serde(default)]
    pub log_path: Option<String>,

    /// Shell command whose first line of stdout is the version string.
    /// None for sidecars that do not have a standalone `--version` flag.
    #[serde(default)]
    pub version_cmd: Option<String>,

    /// Substring or regex (see validation note) used for pgrep-style process
    /// identification. The discovery engine runs `pgrep -f <process_match>` on
    /// Unix. Must not contain shell metacharacters; validated at manifest load.
    /// None means "skip process probing" (useful when liveness is port/HTTP-only).
    #[serde(default)]
    pub process_match: Option<String>,

    // --- Optional lifecycle commands ---

    /// Shell command to start the service. None = not managed by tm services.
    #[serde(default)]
    pub start_cmd: Option<String>,

    /// Shell command to stop the service. None = not managed.
    #[serde(default)]
    pub stop_cmd: Option<String>,

    /// Shell command to restart the service. None = not managed.
    #[serde(default)]
    pub restart_cmd: Option<String>,
}

impl ServicesManifest {
    /// Why: centralising validation prevents partial manifests from reaching
    /// the discovery engine, and gives the user an actionable error at load time
    /// rather than a confusing None at query time.
    /// What: checks version <= 1, all ports in valid range, port_file set when
    /// port_discovery == File, process_match free of shell metacharacters.
    /// Test: `manifest_rejects_future_version`, `manifest_rejects_invalid_port`,
    /// `manifest_rejects_file_discovery_without_port_file`,
    /// `manifest_rejects_metacharacters_in_process_match`.
    pub fn validate(&self) -> Result<(), ManifestValidationError> { ... }

    /// Why: `tm services` must work even when the user has never run `tm install`.
    /// What: returns the embedded YAML string (include_str! of the default manifest
    /// resource, or a const string). Parsed and validated before returning.
    /// Test: `embedded_default_manifest_is_valid`.
    pub fn default_manifest() -> Self { ... }

    /// Why: tilde in log_path and port_file is a UX expectation, not a shell
    /// feature; the runtime must expand it before using the path.
    /// What: expands leading `~/` to `dirs::home_dir()` for all path-bearing fields.
    /// Test: `manifest_expands_tilde`.
    pub fn expand_paths(&mut self) -> anyhow::Result<()> { ... }
}

/// Validation errors for the manifest.
///
/// Why: thiserror gives the engineer structured, match-able errors from the
/// library layer; anyhow::Error wraps them in the binary layer.
/// Test: each variant exercised by a named unit test.
#[derive(Debug, thiserror::Error)]
pub enum ManifestValidationError {
    #[error("manifest version {0} is unsupported (max: 1)")]
    UnsupportedVersion(u32),

    #[error("service '{0}': default_port {1} is not in the valid range 1-65535")]
    InvalidPort(String, u32),

    #[error("service '{0}': port_discovery is 'file' but port_file is not set")]
    MissingPortFile(String),

    #[error("service '{0}': process_match '{1}' contains shell metacharacters")]
    UnsafeProcessMatch(String, String),

    #[error("service '{0}': health_url '{1}' is not a valid URL template")]
    InvalidHealthUrl(String, String),
}
```

**File location in the crate**: `crates/trusty-mpm/src/services/manifest.rs`

**Module registration**: add `pub mod services;` to `crates/trusty-mpm/src/lib.rs`
alongside the existing `pub mod core;` and `pub mod client;`.

---

## 4. Discovery Engine API

### Module layout

```
crates/trusty-mpm/src/services/
├── mod.rs          -- pub use manifest::*, discoverer::*
├── manifest.rs     -- ServicesManifest, ServiceDecl (§3 above)
└── discoverer.rs   -- Discoverer, ServiceStatus, HealthState
```

### Types

```rust
// crates/trusty-mpm/src/services/discoverer.rs

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

/// Aggregate status of one service resolved from the manifest + live probes.
///
/// Why: callers need a single struct that answers every `tm services` subcommand
/// without issuing multiple probes. Discoverer populates all fields in one pass
/// and caches the result (see TTL below).
/// What: all fields are optional because any probe can fail (daemon absent,
/// port file missing, health endpoint returns 500). The CLI renders None as "—"
/// in human output and null in JSON.
/// Test: `service_status_serialises_to_json`, `service_status_all_none_is_unknown`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    /// Service name (key in the manifest).
    pub name: String,

    /// True when the service has a declaration in the manifest.
    pub declared: bool,

    /// True when a matching process was found via `pgrep -f <process_match>`.
    pub running: bool,

    /// PID of the matching process (first hit from pgrep; None if not running).
    pub pid: Option<u32>,

    /// Actual bound port. May differ from `default_port` if the service used
    /// dynamic port selection (e.g. trusty-memory). Discovered via port_file
    /// when `port_discovery == File`, or defaults to `default_port` otherwise.
    pub port: Option<u16>,

    /// Full base URL derived from the discovered port (e.g. "http://localhost:7878").
    /// None when the service has no HTTP surface.
    pub url: Option<String>,

    /// Version string from `version_cmd` stdout, first line, trimmed.
    /// None when version_cmd is absent or the command fails.
    pub version: Option<String>,

    /// Aggregated health state from the health endpoint probe.
    pub health: HealthState,

    /// Resolved, tilde-expanded path to the most-recent log file.
    /// None when log_path is absent or the path does not exist.
    pub log_path: Option<PathBuf>,

    /// Approximate uptime derived from the process start time.
    /// None when PID is unknown or sysinfo probe fails.
    pub uptime: Option<Duration>,
}

/// Health probe result.
///
/// Why: a three-state enum distinguishes "definitely healthy" from "no HTTP
/// surface" (sidecar daemons) from "definitely unhealthy". This prevents callers
/// from treating an absent health endpoint as a failure.
/// What: Unknown is for services with no health_url (sidecars); Ok and Fail
/// carry the HTTP-level determination. FailWithDetail carries the HTTP status
/// code or the connection error for display in `tm services health`.
/// Test: `health_state_serialises_correctly`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    /// Health URL returned 2xx. Service is healthy.
    Ok,
    /// Service has no health URL (UDS sidecar). Liveness from process probe only.
    Unknown,
    /// Health URL returned non-2xx or connection was refused.
    Fail { detail: String },
}

/// TTL for cached discovery results within a process lifetime.
///
/// Why: agents frequently call `tm services port X` multiple times in one
/// session (e.g. inside a loop). Without a cache each call issues pgrep + HTTP
/// probes, adding 200-500ms latency. 5s TTL keeps results fresh enough for
/// operational decisions while eliminating duplicate probes during a single
/// agent task.
/// What: Instant::now() compared against the cache timestamp in Discoverer.
const CACHE_TTL: Duration = Duration::from_secs(5);

/// Service discovery engine.
///
/// Why: centralises all probe logic (process, port, HTTP, version) so the CLI
/// handlers never issue subprocesses or HTTP calls directly — a seam that makes
/// unit testing straightforward by injecting mock implementors.
/// What: holds the parsed manifest and a BTreeMap TTL cache. Probes are issued
/// lazily on `status()` / `list()` calls. Cache is per-process (no cross-process
/// IPC for v1; see §9 Open Questions).
/// Test: unit tests use `MockProcessProber`, `MockPortProber`, `MockHttpProber`.
pub struct Discoverer {
    manifest: ServicesManifest,
    cache: BTreeMap<String, (Instant, ServiceStatus)>,
    process_prober: Box<dyn ProcessProber>,
    port_prober: Box<dyn PortProber>,
    http_prober: Box<dyn HttpProber>,
    version_runner: Box<dyn VersionRunner>,
}

impl Discoverer {
    /// Why: production ctor uses the real system probers; tests inject mocks.
    /// What: creates a Discoverer with real subprocess and HTTP probers.
    pub fn new(manifest: ServicesManifest) -> Self { ... }

    /// List status of every declared service.
    ///
    /// Why: `tm services list` needs all services in a single call; individual
    /// cache entries are returned from cache if fresh.
    /// What: iterates manifest.services, calls `status_inner` for each,
    /// returns sorted Vec<ServiceStatus>.
    /// Test: `list_returns_all_manifest_services`.
    pub async fn list(&mut self) -> Vec<ServiceStatus> { ... }

    /// Status of one named service.
    ///
    /// Why: `tm services status <name>`, `tm services port <name>`, and
    /// `tm services url <name>` all need a single ServiceStatus.
    /// What: returns cached value if within TTL; otherwise calls `probe` and
    /// stores result. Returns None when `name` is not in the manifest.
    /// Test: `status_returns_none_for_unknown_service`,
    /// `status_uses_cache_within_ttl`.
    pub async fn status(&mut self, name: &str) -> Option<ServiceStatus> { ... }

    /// Issue a health probe for one service.
    ///
    /// Why: `tm services health <name>` is a common agent one-liner; it should
    /// always probe liveness freshly (bypasses cache).
    /// What: always re-issues the HTTP probe regardless of cache state; returns
    /// HealthResult with the updated HealthState and writes back to cache.
    /// Test: `health_bypasses_cache`, `health_returns_fail_when_http_down`.
    pub async fn health(&mut self, name: &str) -> Option<HealthResult> { ... }

    // --- Internal probing ---

    /// Probe all discovery dimensions for one ServiceDecl, update cache.
    async fn probe(&mut self, name: &str, decl: &ServiceDecl) -> ServiceStatus {
        let pid = self.probe_process(decl).await;
        let port = self.probe_port(decl).await;
        let url = port.map(|p| format!("http://localhost:{p}"));
        let health = match &url {
            Some(u) if decl.health_url.is_some() => self.probe_health(u, decl).await,
            _ if pid.is_some() => HealthState::Unknown,
            _ => HealthState::Fail { detail: "not running".into() },
        };
        let version = if pid.is_some() { self.probe_version(decl).await } else { None };
        let uptime = pid.and_then(|p| self.probe_uptime(p));
        let log_path = decl.log_path.as_ref().map(|p| expand_path(p)).and_then(|p| {
            p.exists().then_some(p)
        });
        ServiceStatus {
            name: name.to_string(),
            declared: true,
            running: pid.is_some(),
            pid,
            port,
            url,
            version,
            health,
            log_path,
            uptime,
        }
    }

    /// Run `pgrep -f <process_match>`, parse first PID.
    ///
    /// Why: pgrep is the simplest cross-platform process lookup; `-f` matches
    /// against the full command line so we don't need to know the exact binary
    /// name. The discovery engine uses the ProcessProber trait so tests inject
    /// a mock without spawning real processes.
    /// What: spawns `pgrep -f <pattern>`, reads stdout, parses first whitespace-
    /// delimited token as u32. Returns None on no match or parse failure.
    /// Test: `probe_process_returns_pid_when_running`,
    /// `probe_process_returns_none_when_not_running`.
    async fn probe_process(&self, decl: &ServiceDecl) -> Option<u32> { ... }

    /// Discover the runtime port via port_file or default_port.
    ///
    /// Why: trusty-memory uses dynamic port selection; reading port_file (written
    /// by write_daemon_addr in trusty-common) is the only reliable way to find
    /// it. All other services use their default_port.
    /// What: for File discovery: reads and parses `host:port` from port_file;
    /// for Static: returns default_port directly.
    /// Test: `probe_port_reads_port_file`, `probe_port_returns_default_port`.
    async fn probe_port(&self, decl: &ServiceDecl) -> Option<u16> { ... }

    /// HTTP GET <health_url> with 1.5s timeout, return HealthState.
    ///
    /// Why: 1.5s is short enough not to block a quick agent invocation but long
    /// enough to survive a loaded localhost under mild memory pressure.
    /// What: uses the reqwest::Client already available as a workspace dep.
    /// 2xx → HealthState::Ok; non-2xx → HealthState::Fail with status code;
    /// connection error → HealthState::Fail with the error message.
    /// Test: `probe_health_ok_on_2xx`, `probe_health_fail_on_503`,
    /// `probe_health_fail_on_connection_refused`.
    async fn probe_health(&self, url: &str, decl: &ServiceDecl) -> HealthState { ... }

    /// Run version_cmd, return trimmed first line of stdout.
    ///
    /// Why: version strings are useful in `tm services status` and `tm services list`
    /// for diagnosing stale installs. Version probing is best-effort.
    /// What: spawns `sh -c <version_cmd>`, waits 2s, returns first stdout line
    /// trimmed. Returns None on timeout, error, or empty output.
    /// Test: `probe_version_returns_first_line`.
    async fn probe_version(&self, decl: &ServiceDecl) -> Option<String> { ... }

    /// Use sysinfo to look up process start time, compute Duration since then.
    ///
    /// Why: sysinfo is already an always-on dep of trusty-mpm (Cargo.toml:93).
    /// What: `sysinfo::System::new()`, refresh processes, look up pid,
    /// compute `SystemTime::now() - start_time`.
    /// Test: `probe_uptime_returns_none_for_unknown_pid`.
    fn probe_uptime(&self, pid: u32) -> Option<Duration> { ... }
}

/// Thin result type for the `health` method (separates status from state).
pub struct HealthResult {
    pub name: String,
    pub state: HealthState,
    pub message: String,     // human-readable; "healthy" / "unhealthy: <detail>" / "no health endpoint"
}
```

### Trait interfaces (for testability)

```rust
/// Why: allows tests to inject a mock that returns predetermined PID or None
/// without running pgrep. The production impl is a thin wrapper around
/// `std::process::Command::new("pgrep")`.
pub trait ProcessProber: Send + Sync {
    fn pgrep(&self, pattern: &str) -> Option<u32>;
}

/// Why: allows tests to return a predetermined port or None without reading
/// a file or probing the OS.
pub trait PortProber: Send + Sync {
    fn read_port_file(&self, path: &std::path::Path) -> Option<u16>;
}

/// Why: allows tests to return a predetermined health state without making
/// real HTTP calls. Use mockito (already implied by workspace test deps) or
/// a simple struct implementing the trait.
pub trait HttpProber: Send + Sync {
    async fn get_health(&self, url: &str, timeout: Duration) -> HealthState;
}

/// Why: allows tests to return a predetermined version string.
pub trait VersionRunner: Send + Sync {
    fn run(&self, cmd: &str) -> Option<String>;
}
```

---

## 5. CLI Subcommand Structure

### Clap argument structure

The `Services` subcommand variant is added to the existing top-level `Command`
enum in `crates/trusty-mpm/src/bin/tm.rs` at the point where `Command::Coordinator`
is defined (currently line 173). It slots in alphabetically after `Session`.

```rust
/// Inspect and probe workspace service daemons.
///
/// Why: agents need a single canonical interface to answer "is trusty-search
/// running?", "what port is it on?", "is it healthy?" without resorting to
/// lsof/curl/ps. `tm services` reads the manifest at ~/.claude-mpm/services.yaml
/// and probes each declared service on demand.
/// What: five subcommands (list, status, port, url, health, log) with --json
/// where applicable. Exit codes per the spec: 0=running/healthy, 1=down/unhealthy,
/// 2=unknown service.
/// Test: `cli_parses_services_list`, `cli_parses_services_status`,
/// `cli_parses_services_port`, `cli_parses_services_url`,
/// `cli_parses_services_health`, `cli_parses_services_log`.
Services {
    /// Services action to perform.
    #[command(subcommand)]
    action: ServicesAction,
},
```

```rust
/// Subcommands for `tm services`.
#[derive(Debug, Subcommand)]
enum ServicesAction {
    /// List all declared services with their current status.
    ///
    /// Exit code: always 0 (list never fails; individual services may be down).
    List {
        /// Output as JSON array instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },

    /// Show detailed status for one service.
    ///
    /// Exit code: 0 if running, 1 if down, 2 if service name not in manifest.
    Status {
        /// Service name (e.g. trusty-search).
        name: String,
        /// Output as JSON object.
        #[arg(long)]
        json: bool,
    },

    /// Print the port number for a service (scriptable: PORT=$(tm services port X)).
    ///
    /// Prints just the port number on stdout (no trailing newline for clean
    /// subshell capture). Exit code: 0 if running, 1 if down, 2 if unknown.
    Port {
        /// Service name.
        name: String,
    },

    /// Print the full base URL for a service.
    ///
    /// Example: http://localhost:7878
    /// Exit code: 0 if running, 1 if down, 2 if unknown.
    Url {
        /// Service name.
        name: String,
    },

    /// Probe the health endpoint and report OK or FAIL.
    ///
    /// Prints "OK" on stdout when healthy; diagnostic detail on stderr when
    /// unhealthy. Exit code: 0 if healthy, 1 if unhealthy or down.
    Health {
        /// Service name.
        name: String,
    },

    /// Print the path to the most-recent log file (scriptable: tail $(tm services log X)).
    ///
    /// Exit code: 0 if log path known and file exists, 1 if down or log path
    /// absent, 2 if unknown service.
    Log {
        /// Service name.
        name: String,
    },

    /// Write the default manifest to ~/.claude-mpm/services.yaml (non-destructive).
    ///
    /// Errors if the file already exists; use --force to overwrite.
    Init {
        /// Overwrite an existing manifest.
        #[arg(long)]
        force: bool,
    },
}
```

### Handler function signature

```rust
async fn services(action: ServicesAction) -> anyhow::Result<()> { ... }
```

Registered in `main()` at `Command::Services { action } => services(action).await`.

### Output: `tm services list` (human table)

Use the `colored` crate (already a workspace dep, `Cargo.toml:65`) to color
the status column. No new table-formatting dep is needed — format columns with
`format!("{:<N}", value)` using column widths computed from the data. Keep it
simple: no external table crate.

Column layout (tab-separated with fixed-width padding):

```
NAME                    STATUS     PORT    VERSION   HEALTH
trusty-search           running    7878    0.13.2    ok
trusty-analyze          down       —       —         —
trusty-mpm-daemon       running    7880    0.2.12    ok
trusty-memory           running    7073    —         ok
trusty-embedderd        running    —       0.3.0     —
trusty-bm25-daemon      down       —       —         —
```

Column widths: NAME=24, STATUS=10, PORT=8, VERSION=10, HEALTH=8.

Status colors (using `colored`):
- `running` → green
- `down` → red
- `unknown` → yellow

### Output: `tm services status <name>` (detail block)

```
trusty-search
  Status:   running
  PID:      12345
  Port:     7878
  URL:      http://localhost:7878
  Version:  trusty-search 0.13.2
  Health:   ok
  Log:      ~/Library/Logs/trusty-search/stderr.log
  Uptime:   4h 22m
```

### Output: `tm services list --json`

```json
[
  {
    "name": "trusty-search",
    "declared": true,
    "running": true,
    "pid": 12345,
    "port": 7878,
    "url": "http://localhost:7878",
    "version": "trusty-search 0.13.2",
    "health": "ok",
    "log_path": "/Users/bob/Library/Logs/trusty-search/stderr.log",
    "uptime_secs": 15720
  }
]
```

`health` field serialises as `"ok"`, `"unknown"`, or `{"fail": {"detail": "<msg>"}}`.
`uptime_secs` is a u64 count of seconds; null when unknown.

### Output: `tm services status <name> --json`

Single JSON object matching the same schema as one element of the `--json list` array.

### Exit codes

| Exit code | Meaning |
|-----------|---------|
| 0 | Service is running and healthy (or list completed successfully) |
| 1 | Service is declared but not running, or health probe failed |
| 2 | Service name not found in manifest |

`tm services list` always exits 0 regardless of individual service states.

---

## 6. Implementation Phases

Each phase ends with `cargo check && cargo test -p trusty-mpm` passing cleanly.

### Phase 1: Manifest schema + parser + default YAML (S)

**Commit message**: `feat(trusty-mpm): services manifest schema + default YAML (issue #339 phase 1)`

Files created/modified:
- `crates/trusty-mpm/src/services/mod.rs` (new)
- `crates/trusty-mpm/src/services/manifest.rs` (new)
- `crates/trusty-mpm/src/lib.rs` — add `pub mod services;`
- `crates/trusty-mpm/assets/default-services.yaml` (new — embedded via `include_str!`)

Work:
1. Define `ServicesManifest`, `ServiceDecl`, `PortDiscovery`, `ManifestValidationError`
   (§3 types verbatim).
2. Implement `ServicesManifest::validate()`, `::default_manifest()`, `::expand_paths()`.
3. Write the default YAML (§2) into `assets/default-services.yaml`.
4. Embed via `include_str!("../../assets/default-services.yaml")` in `default_manifest()`.
5. Unit tests (in `manifest.rs::tests`):
   - `manifest_parse_happy_path` — parse the full default YAML, assert all 6 services present
   - `manifest_parse_minimal_service` — minimal valid declaration (name + description only)
   - `manifest_rejects_future_version` — version = 2 → ManifestValidationError::UnsupportedVersion
   - `manifest_rejects_invalid_port` — port = 99999 → InvalidPort error
   - `manifest_rejects_file_discovery_without_port_file` — missing port_file → error
   - `manifest_rejects_metacharacters_in_process_match` — `foo|bar` → error
   - `manifest_expands_tilde` — `~/foo` → `/Users/<actual_home>/foo`
   - `embedded_default_manifest_is_valid` — `ServicesManifest::default_manifest().validate()` succeeds

**Exit criteria**: 8 unit tests pass, `cargo check` clean.

### Phase 2: Discovery engine with mocked probers (M)

**Commit message**: `feat(trusty-mpm): service discovery engine with caching + prober traits (issue #339 phase 2)`

Files created/modified:
- `crates/trusty-mpm/src/services/discoverer.rs` (new)
- `crates/trusty-mpm/src/services/mod.rs` — add pub uses

Work:
1. Define prober traits (`ProcessProber`, `PortProber`, `HttpProber`, `VersionRunner`).
2. Implement real probers as `RealProcessProber`, `RealPortProber`, `RealHttpProber`,
   `RealVersionRunner` (thin wrappers around `std::process::Command` and `reqwest`).
3. Implement `Discoverer::new()`, `list()`, `status()`, `health()`, `probe()` and
   all `probe_*` internals.
4. Implement in-process TTL cache (BTreeMap<String, (Instant, ServiceStatus)>).
5. Unit tests (in `discoverer.rs::tests`) using in-process mock probers:
   - `status_returns_none_for_unknown_service`
   - `status_uses_cache_within_ttl` — second call within 5s returns cached result
   - `status_probes_fresh_after_ttl` — elapsed > 5s forces re-probe
   - `probe_process_returns_pid_when_running` — mock pgrep returns Some(1234)
   - `probe_process_returns_none_when_not_running`
   - `probe_port_reads_port_file` — mock file returns "127.0.0.1:7073", port = 7073
   - `probe_port_returns_default_port` — Static discovery returns default_port
   - `probe_health_ok_on_2xx` — mock HTTP returns 200
   - `probe_health_fail_on_503` — mock HTTP returns 503
   - `probe_health_fail_on_connection_refused` — mock HTTP returns Err
   - `health_bypasses_cache` — health() re-probes even when cache is fresh
   - `list_returns_all_manifest_services`
   - `service_status_serialises_to_json`

**Exit criteria**: 13 unit tests pass, `cargo check` clean.

### Phase 3: CLI subcommands + output formatters (M)

**Commit message**: `feat(trusty-mpm): tm services subcommands (list, status, port, url, health, log) (issue #339 phase 3)`

Files modified:
- `crates/trusty-mpm/src/bin/tm.rs` — add `Command::Services { action }`, `ServicesAction`
  enum, `services()` handler, register in `main()` match

Work:
1. Add `ServicesAction` enum with all six subcommand variants (§5).
2. Implement `services(action: ServicesAction)` async function.
3. Wire exit codes: use `std::process::exit(code)` for non-zero exits from the
   `port`, `url`, `health`, `status`, `log` subcommands. Do not use
   `anyhow::bail!` for these cases — the exit code must be exact.
4. Implement human table formatter for `list` (no new deps; use `colored` + format strings).
5. Implement detail block formatter for `status`.
6. Implement `init` subcommand: write `ServicesManifest::default_manifest()` serialised
   as YAML to `~/.claude-mpm/services.yaml`. Error if file exists and `--force` not set.
7. Load manifest in each handler:
   ```rust
   let manifest = if services_yaml_path.exists() {
       serde_yaml::from_str(&fs::read_to_string(&services_yaml_path)?)?.validate()?
   } else {
       ServicesManifest::default_manifest()
   };
   ```
8. CLI parse tests (add to existing `tests/` block in `tm.rs`):
   - `cli_parses_services_list` — `tm services list` parses without error
   - `cli_parses_services_list_json` — `tm services list --json` parses
   - `cli_parses_services_status` — `tm services status trusty-search`
   - `cli_parses_services_port` — `tm services port trusty-search`
   - `cli_parses_services_url` — `tm services url trusty-search`
   - `cli_parses_services_health` — `tm services health trusty-search`
   - `cli_parses_services_log` — `tm services log trusty-search`
   - `cli_parses_services_init` — `tm services init`
   - `cli_parses_services_init_force` — `tm services init --force`

**Exit criteria**: 9 CLI parse tests pass, human output renders correctly (manual
check), `cargo check` clean.

### Phase 4: Integration test + help.yaml update (S)

**Commit message**: `test(trusty-mpm): integration smoke test for tm services against live trusty-search (issue #339 phase 4)`

Files modified:
- `crates/trusty-mpm/tests/services_integration.rs` (new)
- `crates/trusty-mpm/help.yaml` — add `services` and subcommand entries

Work:
1. Integration test (gated `#[ignore]`):
   ```rust
   #[tokio::test]
   #[ignore = "requires live trusty-search daemon on :7878"]
   async fn smoke_test_services_list_against_live_trusty_search() { ... }
   ```
   Test: creates a manifest with only `trusty-search` declared, calls
   `Discoverer::list()`, asserts `trusty-search` appears with `running=true`
   and `port=Some(7878)`.

2. Add `services` and all subcommands to `help.yaml` so `tm servces` (typo)
   suggests `services` via the existing `trusty_common::help::suggest` mechanism.
   Format matches existing entries in `help.yaml`.

**Exit criteria**: integration test is present (passes when daemon running,
skipped in CI), `help.yaml` updated, `cargo check` clean.

### Phase 5: Docs + version bump (S)

**Commit message**: `docs(trusty-mpm): tm services discovery CLI docs + CHANGELOG + version bump (issue #339)`

Files modified:
- `crates/trusty-mpm/CHANGELOG.md` — add entry for new `services` subcommand
- `crates/trusty-mpm/Cargo.toml` — bump workspace version (minor bump per ticket)
- `docs/trusty-mpm/research/tm-services-discovery-spec-2026-05-28.md` — this file
  (already exists in worktree, move to main on merge)

Work:
1. Write CHANGELOG entry summarising subcommands, manifest location, exit codes.
2. Confirm the workspace version bump in `Cargo.toml` (check `[workspace.package]`
   version in the root `Cargo.toml`; bump appropriately per semver).
3. No README update needed for this feature (CHANGELOG is sufficient for a
   subcommand addition; users discover via `tm services --help`).

**Exit criteria**: `cargo test -p trusty-mpm` clean, CHANGELOG updated.

---

## 7. Test Plan

### Unit tests (automated, no daemon required)

**Manifest parsing** (`crates/trusty-mpm/src/services/manifest.rs::tests`):

| Test | Input | Expected |
|------|-------|----------|
| `manifest_parse_happy_path` | Full default YAML | 6 services parsed, all required fields present |
| `manifest_parse_minimal_service` | YAML with description only | Parsed; all Option fields are None |
| `manifest_rejects_future_version` | `version: 2` | `ManifestValidationError::UnsupportedVersion(2)` |
| `manifest_rejects_invalid_port` | `default_port: 99999` | `ManifestValidationError::InvalidPort` |
| `manifest_rejects_bad_yaml` | Malformed YAML | `serde_yaml::Error` before validation |
| `manifest_rejects_file_discovery_without_port_file` | `port_discovery: file`, no `port_file` | `ManifestValidationError::MissingPortFile` |
| `manifest_rejects_metacharacters_in_process_match` | `process_match: "foo|bar"` | `ManifestValidationError::UnsafeProcessMatch` |
| `embedded_default_manifest_is_valid` | `ServicesManifest::default_manifest()` | `validate()` returns Ok(()) |

**Discovery engine** (`crates/trusty-mpm/src/services/discoverer.rs::tests` using mock probers):

| Test | Scenario | Expected |
|------|----------|----------|
| `status_returns_none_for_unknown_service` | name not in manifest | Returns `None` |
| `status_uses_cache_within_ttl` | Two calls within 5s | Second call skips probe |
| `status_probes_fresh_after_ttl` | Advance mock clock > 5s | Second call re-probes |
| `probe_process_returns_pid_when_running` | Mock pgrep returns 1234 | `pid: Some(1234)`, `running: true` |
| `probe_process_returns_none_when_not_running` | Mock pgrep returns None | `pid: None`, `running: false` |
| `probe_port_reads_port_file` | Port file contains "127.0.0.1:7073" | `port: Some(7073)` |
| `probe_port_returns_default_port` | Static discovery, default_port=7878 | `port: Some(7878)` |
| `probe_health_ok_on_2xx` | Mock HTTP 200 | `health: HealthState::Ok` |
| `probe_health_fail_on_503` | Mock HTTP 503 | `health: HealthState::Fail { detail: "503..." }` |
| `probe_health_fail_on_connection_refused` | Mock HTTP Err | `health: HealthState::Fail { detail: "..." }` |
| `health_bypasses_cache` | health() called twice; assert 2 probe invocations | Cache not used for health() |
| `list_returns_all_manifest_services` | 6-service manifest | 6 ServiceStatus entries |
| `service_status_serialises_to_json` | ServiceStatus with all fields | Roundtrips via serde_json |

**CLI argument parsing** (`tm.rs::tests` block, no I/O):

9 tests as listed in Phase 3 above.

### Integration test (gated `#[ignore]`)

`crates/trusty-mpm/tests/services_integration.rs`:
- `smoke_test_services_list_against_live_trusty_search` — requires `trusty-search` on :7878
- `smoke_test_services_health_against_live_trusty_search` — health probe returns Ok

Run locally with:
```bash
cargo test -p trusty-mpm --test services_integration -- --include-ignored --nocapture
```

### Manual test plan (engineer runs before merging)

1. **Init**: `tm services init` → `~/.claude-mpm/services.yaml` created; run again → error "already exists"; run with `--force` → file overwritten.
2. **List (no daemons)**: all 6 services show `down` status with `—` for port, version, health.
3. **List (trusty-search running)**: start `trusty-search start`; `tm services list` shows `trusty-search` as `running / 7878 / ok`.
4. **Port (scriptable)**: `PORT=$(tm services port trusty-search); curl http://localhost:$PORT/health` → `{"status":"ok",...}`.
5. **URL (scriptable)**: `URL=$(tm services url trusty-search); curl $URL/health` → works.
6. **Health (healthy)**: `tm services health trusty-search` → stdout `OK`, exit 0.
7. **Health (down)**: stop trusty-search; `tm services health trusty-search` → exit 1, detail on stderr.
8. **Unknown service**: `tm services port no-such-daemon` → exit 2.
9. **trusty-memory (dynamic port)**: start `trusty-memory serve`; `tm services port trusty-memory` → returns actual port (may be 7070–7079), not always 7070.
10. **JSON output**: `tm services list --json | jq '.[0].name'` → `"trusty-search"`.

---

## 8. Agent-Prompt Update Plan

The following files need updating after `tm services` is implemented and merged.
The PM agent will draft the exact text at that time; this section identifies the
locations and intent only.

| File | Location in file | Intent |
|------|-----------------|--------|
| `.claude-mpm/PM_INSTRUCTIONS.md` | P3 tool routing table (line 19); "Verification Commands" CB#7 section (line 233) | Clarify that `tm services` is the allowed alternative to `lsof/curl/ps` for service questions; agents should ask the PM to delegate to `tm services` or call it directly if they have tool access |
| `.claude-mpm/AGENT_DELEGATION.md` | Local backend routing row (near line 10) | Add note that local-ops and qa agents must use `tm services` for service discovery, not raw `lsof`/`curl` |
| `.claude/agents/cargo-ops.md` | Any section that mentions probing daemon ports | Replace raw port probes with `tm services port <name>` |
| `.claude/agents/rust-engineer.md` | Any section that mentions checking service status | Note `tm services status <name>` as the canonical check |
| Project `CLAUDE.md` (workspace root) | "Running Individual MCP Servers Locally" section | Add a note that `tm services list` shows all running daemons and their ports |

The PM should verify none of the agent files were auto-generated (they may be
overwritten on `tm install`). If auto-generated, update the source template in
`crates/trusty-mpm/src/core/bundle.rs` or the embedded asset directory instead.

---

## 9. Open Questions for User Review

1. **Auto-start on declaration**: Should `tm services status <name>` auto-start a
   declared-but-not-running service, or only report status? The ticket says v1
   is status-only. Confirmed: v1 reports only. The manifest `start_cmd` field is
   present for v2 (`tm services restart`), but auto-start is deferred.

2. **Embedded default vs `tm services init`**: When `~/.claude-mpm/services.yaml`
   is absent, should `tm services list` silently use the embedded default manifest
   (convenient, always works) or error and instruct the user to run
   `tm services init` (explicit, surfaceable)? The ticket leans toward silent
   auto-install on first run, but this has a UX trade-off: the user's
   `~/.claude-mpm/` directory is silently mutated. Recommendation: use the
   embedded default in-memory only (no disk write) unless `--init` is explicitly
   run or the user runs `tm services init`. This avoids surprise file creation.
   **Needs user confirmation before Phase 3 implementation.**

3. **`tm services restart` in v1**: The ticket marks restart as "optional, stretch".
   The `ServicesAction` enum in this spec omits it. Should it be added in v1 (it
   is a trivial shell-command executor given `restart_cmd` in the manifest) or
   explicitly deferred? Decision affects Phase 3 scope.

4. **Cross-process cache**: This spec implements per-process TTL cache (5s, in-memory).
   An agent session may issue many `tm services port X` calls across multiple
   `tm` process invocations. A file-based cache (e.g. `~/.claude-mpm/service-cache.json`
   with a `probed_at` timestamp) would avoid repeated pgrep+HTTP probes across
   invocations. Per-process is simpler and correct; cross-process adds atomicity
   complexity. Recommend per-process for v1 with cross-process as a follow-up.

5. **`trusty-mpm-daemon` health endpoint**: The daemon listens on port 7880
   (`DEFAULT_URL = "http://127.0.0.1:7880"` in `tm.rs:21`) but it is unclear
   whether `GET /health` is implemented on the daemon's axum router. The existing
   `tm status` command calls `GET /health` (inferred from `status()` function body
   and the `DaemonClient` pattern), but the actual route registration was not
   confirmed in the source audit. The engineer must verify that `GET /health`
   exists on the MPM daemon's router before adding it to the manifest `health_url`.
   If it does not exist, the health_url for `trusty-mpm-daemon` should be set to
   null and the manifest doc updated. This is the highest-risk item in the spec.

6. **`trusty-memory` log path**: The memory daemon does not currently write to a
   named log file (it uses UDS and launchd stderr, no tracing-appender). The
   manifest sets `log_path: null`. If the user wants `tm services log trusty-memory`
   to work, the memory daemon needs a `--log-file` flag or tracing-appender
   integration. Defer to a follow-up on the memory crate.

---

## 10. Glossary and Reference

### Environment variables

| Variable | Set by | Purpose |
|----------|--------|---------|
| `TRUSTY_MPM_URL` | operator / agent | Override daemon base URL (default `http://127.0.0.1:7880`) |
| `TRUSTY_SEARCH_URL` | operator | Override trusty-search URL (default `http://127.0.0.1:7878`) |
| `TRUSTY_ANALYZER_PORT` | operator | Override trusty-analyze port (default 7879) |
| `TRUSTY_DATA_DIR_OVERRIDE` | test harness | Redirect `resolve_data_dir` for test isolation (trusty-common) |

### File paths

| Path | Purpose |
|------|---------|
| `~/.claude-mpm/services.yaml` | Default manifest location (created by `tm services init`) |
| `~/.trusty-memory/http_addr` | trusty-memory bound address (written by daemon on start) |
| `~/Library/Application Support/trusty-memory/http_addr` | macOS-standard location for same |
| `~/.trusty-mpm/logs/trusty-mpm.log` | MPM daemon rolling daily log |
| `~/Library/Logs/trusty-search/stderr.log` | trusty-search launchd stderr |
| `~/.trusty-analyze/logs/stderr.log` | trusty-analyze launchd stderr |
| `~/.trusty-analyze/daemon.pid` | trusty-analyze PID file (`crates/trusty-analyze/src/commands/daemon.rs:41`) |

### Exit codes

| Code | Meaning | Commands that return it |
|------|---------|------------------------|
| 0 | Running/healthy/success | All commands on success; `list` always |
| 1 | Down/unhealthy/log absent | `status`, `port`, `url`, `health`, `log` |
| 2 | Unknown service (not in manifest) | `status`, `port`, `url`, `health`, `log` |

### Key source references

| File | Line(s) | Relevance |
|------|---------|-----------|
| `crates/trusty-mpm/src/bin/tm.rs` | 21 | `DEFAULT_URL = "http://127.0.0.1:7880"` — MPM daemon port |
| `crates/trusty-mpm/src/bin/tm.rs` | 37–177 | Existing `Command` enum — insert `Services` here |
| `crates/trusty-search/src/main.rs` | 368, 422 | trusty-search default port 7878 |
| `crates/trusty-search/src/commands/status.rs` | 24 | `GET /health` endpoint confirmed |
| `crates/trusty-search/src/commands/service.rs` | 58–62 | trusty-search log dir: `~/Library/Logs/trusty-search/` |
| `crates/trusty-analyze/src/main.rs` | 73, 159, 188 | trusty-analyze default port 7879 |
| `crates/trusty-analyze/src/service/mod.rs` | 308 | `.route("/health", get(health))` confirmed |
| `crates/trusty-analyze/src/commands/daemon.rs` | 28–42 | trusty-analyze data dir and PID file |
| `crates/trusty-analyze/src/commands/service.rs` | 67–80 | log dir: `~/.trusty-analyze/logs/` |
| `crates/trusty-memory/src/lib.rs` | 1208, 1248 | Port range 7070–7079, `DEFAULT_HTTP_PORT=7070` |
| `crates/trusty-memory/src/lib.rs` | 1221–1230 | `http_addr_path()` — port file location |
| `crates/trusty-common/src/lib.rs` | 375–415 | `write_daemon_addr` / `read_daemon_addr` — canonical port-file convention |
| `crates/trusty-mpm/src/bin/trusty-mpmd.rs` | 47–53 | MPM daemon log dir: `~/.trusty-mpm/logs/` |
| `crates/trusty-embedderd/src/lib.rs` | 214, 299 | embedderd is stdio/UDS sidecar, no HTTP |
| `crates/trusty-bm25-daemon/src/socket.rs` | 9, 26, 64 | BM25 daemon: `$TMPDIR/trusty-bm25-<palace>.sock` |
| `crates/trusty-mpm/Cargo.toml` | 47, 65, 93, 110 | workspace deps: `serde_yaml`, `colored`, `sysinfo`, `reqwest` (all already present) |

### Related issues

- [#339](https://github.com/bobmatnyc/trusty-tools/issues/339) — this ticket
- [#129](https://github.com/bobmatnyc/trusty-tools/issues/129) — cross-release performance tracking (reference for regression-test doc conventions)

### Effort estimates

| Phase | Estimate | Notes |
|-------|----------|-------|
| Phase 1: Manifest schema + parser | S (1–2h) | Mostly type definitions and serde; tests are straightforward |
| Phase 2: Discovery engine | M (3–5h) | Trait interfaces + mocking pattern; the uptime/sysinfo path needs care |
| Phase 3: CLI subcommands + output | M (3–4h) | Most code is mechanical; exit-code handling needs attention |
| Phase 4: Integration test + help.yaml | S (1h) | Mostly writing the test body |
| Phase 5: Docs + version bump | S (30m) | CHANGELOG + version |
| **Total** | **M–L (9–13h)** | One engineer, one or two sessions |
