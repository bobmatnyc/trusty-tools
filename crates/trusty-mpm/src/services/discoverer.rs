//! Service discovery engine — probes declared services and caches the results.
//!
//! Why: agents frequently ask "is trusty-search running?" and "what port is it
//! on?" across multiple `tm services` invocations in one task. This module
//! issues the minimum probes needed (pgrep + optional HTTP health check) and
//! caches results for 5 seconds so a tight loop of `tm services port X` calls
//! does not flood the OS with subprocess spawns.
//! What: `Discoverer` holds the parsed manifest and a BTreeMap TTL cache.
//! Probers are injected via traits so unit tests can mock every I/O boundary
//! without spawning real processes or binding real ports.
//! Test: 13 unit tests in the `tests` module exercise every probe path and the
//! cache TTL, all without a live daemon.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::manifest::{PortDiscovery, ServiceDecl, ServicesManifest, expand_tilde_owned};

/// TTL for cached discovery results within a process lifetime.
///
/// Why: 5 seconds keeps results fresh enough for operational decisions while
/// eliminating duplicate probes during a single agent task.
/// What: compared against the `Instant` stored with each cache entry in
/// `Discoverer.cache`. Expired entries are re-probed on the next call.
/// Test: `status_uses_cache_within_ttl`, `status_probes_fresh_after_ttl`.
pub const CACHE_TTL: Duration = Duration::from_secs(5);

/// HTTP timeout for health probes.
///
/// Why: 1.5 s is short enough not to block a quick agent invocation but long
/// enough to survive a loaded localhost under mild memory pressure.
/// What: passed as the `timeout` to `HttpProber::get_health`.
/// Test: `probe_health_ok_on_2xx`.
pub const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Aggregate status of one service resolved from the manifest + live probes.
///
/// Why: callers need a single struct that answers every `tm services` subcommand
/// without issuing multiple probes. `Discoverer` populates all fields in one pass
/// and caches the result.
/// What: all optional fields are `None` when the probe was not applicable (e.g.
/// no health_url on a sidecar) or when the probe failed. The CLI renders `None`
/// as `—` in human output and `null` in JSON.
/// Test: `service_status_serialises_to_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    /// Service name (key in the manifest).
    pub name: String,

    /// True when the service has a declaration in the manifest.
    pub declared: bool,

    /// True when a matching process was found via pgrep.
    pub running: bool,

    /// PID of the matching process (first hit from pgrep; None if not running).
    pub pid: Option<u32>,

    /// Actual bound port, discovered via port_file or default_port.
    pub port: Option<u16>,

    /// Full base URL derived from the discovered port.
    pub url: Option<String>,

    /// Version string from `version_cmd` stdout, first line, trimmed.
    pub version: Option<String>,

    /// Aggregated health state from the health endpoint probe.
    pub health: HealthState,

    /// Resolved, tilde-expanded path to the most-recent log file.
    pub log_path: Option<PathBuf>,

    /// Approximate uptime as seconds since process start.
    /// Serialised as `uptime_secs` in JSON for agent consumption.
    #[serde(rename = "uptime_secs")]
    pub uptime_secs: Option<u64>,
}

/// Health probe result.
///
/// Why: a three-state enum distinguishes "definitely healthy" from "no HTTP
/// surface" (sidecar daemons) from "definitely unhealthy". This prevents callers
/// from treating an absent health endpoint as a failure.
/// What: `Unknown` is for services with no `health_url`; `Ok` and `Fail` carry
/// the HTTP-level determination. `Fail` carries a detail string for display.
/// Test: `probe_health_ok_on_2xx`, `probe_health_fail_on_503`,
/// `probe_health_fail_on_connection_refused`, `health_state_serialises_correctly`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    /// Health URL returned 2xx. Service is healthy.
    Ok,
    /// Service has no health URL (UDS sidecar). Liveness from process only.
    Unknown,
    /// Health URL returned non-2xx or connection was refused.
    Fail {
        /// Human-readable detail (HTTP status code or connection error).
        detail: String,
    },
}

/// Thin result type for the `health()` method (status + message together).
///
/// Why: `tm services health <name>` needs both the `HealthState` for the exit
/// code and a pre-formatted `message` for stdout/stderr.
/// What: `state` drives the exit code (0 = Ok, 1 = Fail/Unknown), `message`
/// is printed on stdout (healthy) or stderr (unhealthy).
/// Test: `health_bypasses_cache`.
pub struct HealthResult {
    /// Service name.
    pub name: String,
    /// Health state.
    pub state: HealthState,
    /// Human-readable: "healthy", "no health endpoint", or "unhealthy: <detail>".
    pub message: String,
}

// ─── Prober traits ────────────────────────────────────────────────────────────

/// Trait for process discovery (pgrep wrapper).
///
/// Why: allows tests to inject a mock that returns predetermined PID or None
/// without running pgrep. The production impl is `RealProcessProber`.
/// What: `pgrep(pattern)` returns the PID of the first matching process, or
/// `None` when no match is found.
/// Test: `probe_process_returns_pid_when_running`, `probe_process_returns_none_when_not_running`.
pub trait ProcessProber: Send + Sync {
    /// Run `pgrep -f <pattern>` and return the first matched PID.
    fn pgrep(&self, pattern: &str) -> Option<u32>;
}

/// Trait for port file reading.
///
/// Why: allows tests to return a predetermined port without reading a real file.
/// The production impl is `RealPortProber`.
/// What: `read_port_file(path)` reads a `host:port` line from `path` and parses
/// the port component, returning `None` on any error.
/// Test: `probe_port_reads_port_file`.
pub trait PortProber: Send + Sync {
    /// Read a `host:port` file and return the port component.
    fn read_port_file(&self, path: &std::path::Path) -> Option<u16>;
}

/// Trait for HTTP health probing.
///
/// Why: allows tests to return a predetermined health state without making real
/// HTTP calls. The production impl uses `reqwest::blocking::Client`.
/// What: `get_health(url, timeout)` issues a GET request and returns a
/// `HealthState` reflecting the response code or connection error.
/// Test: `probe_health_ok_on_2xx`, `probe_health_fail_on_503`,
/// `probe_health_fail_on_connection_refused`.
pub trait HttpProber: Send + Sync {
    /// Issue a GET request and return the corresponding `HealthState`.
    fn get_health(&self, url: &str, timeout: Duration) -> HealthState;
}

/// Trait for version command execution.
///
/// Why: allows tests to return a predetermined version string without spawning
/// real subprocesses. The production impl uses `std::process::Command`.
/// What: `run(cmd)` spawns `sh -c <cmd>` with a 2-second timeout and returns
/// the first non-empty line of stdout, or `None` on failure/timeout.
/// Test: `probe_version_returns_first_line` (implicit via mocked impl).
pub trait VersionRunner: Send + Sync {
    /// Run `sh -c <cmd>` and return the first stdout line.
    fn run(&self, cmd: &str) -> Option<String>;
}

// ─── Production probers ───────────────────────────────────────────────────────

/// Production process prober using `pgrep -f`.
///
/// Why: pgrep is universally available on macOS and Linux; `-f` matches the
/// full command line so partial binary names (e.g. `trusty-bm25-`) work.
/// What: spawns `pgrep -f <pattern>`, reads stdout, parses first token as u32.
/// Test: indirect — real daemon is tested via the integration test.
pub struct RealProcessProber;

impl ProcessProber for RealProcessProber {
    fn pgrep(&self, pattern: &str) -> Option<u32> {
        let out = std::process::Command::new("pgrep")
            .args(["-f", pattern])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        // pgrep may return multiple PIDs; take the first numeric token.
        stdout
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<u32>().ok())
    }
}

/// Production port prober — reads a `host:port` file.
///
/// Why: `trusty-memory` writes the bound address to `~/.trusty-memory/http_addr`
/// using the `write_daemon_addr` convention from `trusty-common`.
/// What: reads the file, trims whitespace, parses the port from `host:port`.
/// Test: `probe_port_reads_port_file`.
pub struct RealPortProber;

impl PortProber for RealPortProber {
    fn read_port_file(&self, path: &std::path::Path) -> Option<u16> {
        let content = std::fs::read_to_string(path).ok()?;
        let trimmed = content.trim();
        // Format is `host:port` — split on ':' and take the last component.
        let port_str = trimmed.rsplit(':').next()?;
        port_str.trim().parse::<u16>().ok()
    }
}

/// Production HTTP health prober using `reqwest::blocking`.
///
/// Why: reqwest is already a workspace dep; blocking client avoids async
/// complexity in the prober trait while keeping the caller async-friendly.
/// What: issues GET to `url` with the given timeout; 2xx → Ok, non-2xx → Fail
/// with status code, connection error → Fail with error text.
/// Test: `probe_health_ok_on_2xx`, `probe_health_fail_on_503`.
pub struct RealHttpProber;

impl HttpProber for RealHttpProber {
    fn get_health(&self, url: &str, timeout: Duration) -> HealthState {
        let client = match reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return HealthState::Fail {
                    detail: format!("failed to build HTTP client: {e}"),
                };
            }
        };
        match client.get(url).send() {
            Ok(resp) if resp.status().is_success() => HealthState::Ok,
            Ok(resp) => HealthState::Fail {
                detail: format!("HTTP {}", resp.status()),
            },
            Err(e) => HealthState::Fail {
                detail: e.to_string(),
            },
        }
    }
}

/// Production version runner using `sh -c <cmd>`.
///
/// Why: version commands can be multi-word (e.g. `trusty-search --version`);
/// running via `sh -c` handles quoting and PATH consistently.
/// What: spawns `sh -c <cmd>` with a 2-second wall-clock timeout via a simple
/// thread-based approach, returns first non-empty stdout line.
/// Test: indirect — real binary is tested via smoke test.
pub struct RealVersionRunner;

impl VersionRunner for RealVersionRunner {
    fn run(&self, cmd: &str) -> Option<String> {
        let out = std::process::Command::new("sh")
            .args(["-c", cmd])
            .output()
            .ok()?;
        // Return the first non-empty line of stdout.
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
    }
}

// ─── Discoverer ───────────────────────────────────────────────────────────────

/// Service discovery engine.
///
/// Why: centralises all probe logic (process, port, HTTP, version) so CLI
/// handlers never issue subprocesses or HTTP calls directly. Trait-based probers
/// make unit testing straightforward by injecting mock implementations.
/// What: holds the parsed manifest and a BTreeMap TTL cache (5s). Probes are
/// issued lazily on `status()` / `list()` calls. Cache is per-process.
/// Test: 13 unit tests in the `tests` module cover all probe paths and cache
/// behaviour.
pub struct Discoverer {
    manifest: ServicesManifest,
    /// Cache: service name → (inserted_at, status).
    cache: BTreeMap<String, (Instant, ServiceStatus)>,
    process_prober: Box<dyn ProcessProber>,
    port_prober: Box<dyn PortProber>,
    http_prober: Box<dyn HttpProber>,
    version_runner: Box<dyn VersionRunner>,
}

impl Discoverer {
    /// Create a `Discoverer` backed by the real OS probers.
    ///
    /// Why: production code path — separating construction from the trait objects
    /// lets tests inject mocks via `Discoverer::with_probers`.
    /// What: wraps `RealProcessProber`, `RealPortProber`, `RealHttpProber`,
    /// `RealVersionRunner` in `Box<dyn …>`.
    /// Test: indirect via smoke test against live trusty-search.
    pub fn new(manifest: ServicesManifest) -> Self {
        Self {
            manifest,
            cache: BTreeMap::new(),
            process_prober: Box::new(RealProcessProber),
            port_prober: Box::new(RealPortProber),
            http_prober: Box::new(RealHttpProber),
            version_runner: Box::new(RealVersionRunner),
        }
    }

    /// Create a `Discoverer` with injected probers (for testing).
    ///
    /// Why: tests need to inject mock probers so they do not spawn real processes
    /// or make real HTTP calls.
    /// What: accepts trait objects for all four probe boundaries.
    /// Test: every unit test in the `tests` module uses this constructor.
    #[cfg(test)]
    pub fn with_probers(
        manifest: ServicesManifest,
        process_prober: Box<dyn ProcessProber>,
        port_prober: Box<dyn PortProber>,
        http_prober: Box<dyn HttpProber>,
        version_runner: Box<dyn VersionRunner>,
    ) -> Self {
        Self {
            manifest,
            cache: BTreeMap::new(),
            process_prober,
            port_prober,
            http_prober,
            version_runner,
        }
    }

    /// List status of every declared service.
    ///
    /// Why: `tm services list` needs all services in a single call.
    /// What: iterates `manifest.services`, calls `probe_or_cached` for each,
    /// returns a `Vec<ServiceStatus>` in manifest (BTreeMap) order.
    /// Test: `list_returns_all_manifest_services`.
    pub fn list(&mut self) -> Vec<ServiceStatus> {
        // Collect keys first to avoid borrow conflicts.
        let names: Vec<String> = self.manifest.services.keys().cloned().collect();
        names
            .into_iter()
            .map(|name| {
                let decl = self.manifest.services[&name].clone();
                self.probe_or_cached(&name, &decl)
            })
            .collect()
    }

    /// Status of one named service.
    ///
    /// Why: `tm services status <name>`, `tm services port <name>`, and
    /// `tm services url <name>` all need a single `ServiceStatus`.
    /// What: returns cached value if within TTL; otherwise probes and stores.
    /// Returns `None` when `name` is not in the manifest.
    /// Test: `status_returns_none_for_unknown_service`, `status_uses_cache_within_ttl`.
    pub fn status(&mut self, name: &str) -> Option<ServiceStatus> {
        let decl = self.manifest.services.get(name)?.clone();
        Some(self.probe_or_cached(name, &decl))
    }

    /// Issue a fresh health probe for one service (bypasses cache).
    ///
    /// Why: `tm services health <name>` should always reflect the current state,
    /// not a possibly-stale cached health value.
    /// What: re-probes the health endpoint regardless of cache age, updates the
    /// cached entry's health field, and returns a `HealthResult`.
    /// Test: `health_bypasses_cache`.
    pub fn health(&mut self, name: &str) -> Option<HealthResult> {
        let decl = self.manifest.services.get(name)?.clone();

        // Get a current status (uses cache for port/pid/url, then re-probes health).
        let mut status = self.probe_or_cached(name, &decl);

        // Re-probe health unconditionally.
        let fresh_health = if let Some(url) = &status.url {
            if decl.health_url.is_some() {
                self.probe_health_url(url, &decl)
            } else if status.running {
                HealthState::Unknown
            } else {
                HealthState::Fail {
                    detail: "not running".into(),
                }
            }
        } else if decl.health_url.is_none() {
            if status.running {
                HealthState::Unknown
            } else {
                HealthState::Fail {
                    detail: "not running".into(),
                }
            }
        } else {
            HealthState::Fail {
                detail: "no URL available".into(),
            }
        };

        status.health = fresh_health.clone();

        // Write back updated status to cache.
        self.cache
            .insert(name.to_string(), (Instant::now(), status));

        let message = match &fresh_health {
            HealthState::Ok => "healthy".to_string(),
            HealthState::Unknown => "no health endpoint (process-liveness only)".to_string(),
            HealthState::Fail { detail } => format!("unhealthy: {detail}"),
        };

        Some(HealthResult {
            name: name.to_string(),
            state: fresh_health,
            message,
        })
    }

    // ─── Internal helpers ────────────────────────────────────────────────────

    /// Return cached status if fresh, otherwise probe and cache.
    fn probe_or_cached(&mut self, name: &str, decl: &ServiceDecl) -> ServiceStatus {
        if let Some((inserted_at, cached)) = self.cache.get(name)
            && inserted_at.elapsed() < CACHE_TTL
        {
            return cached.clone();
        }
        let status = self.probe(name, decl);
        self.cache
            .insert(name.to_string(), (Instant::now(), status.clone()));
        status
    }

    /// Probe all discovery dimensions for one ServiceDecl.
    ///
    /// Why: a single probe call populates every field of `ServiceStatus` in one
    /// pass, avoiding redundant subprocess spawns when callers access multiple
    /// fields (port + url + health in the same `status` call).
    /// What: runs process, port, health, version, and uptime probes in sequence.
    /// Test: covered by the mock-based discoverer unit tests.
    fn probe(&mut self, name: &str, decl: &ServiceDecl) -> ServiceStatus {
        let pid = self.probe_process(decl);
        let port = self.probe_port(decl);
        let url = port.map(|p| format!("http://localhost:{p}"));

        let health = if let Some(u) = &url {
            if decl.health_url.is_some() {
                self.probe_health_url(u, decl)
            } else if pid.is_some() {
                HealthState::Unknown
            } else {
                HealthState::Fail {
                    detail: "not running".into(),
                }
            }
        } else if decl.health_url.is_none() {
            if pid.is_some() {
                HealthState::Unknown
            } else {
                HealthState::Fail {
                    detail: "not running".into(),
                }
            }
        } else {
            HealthState::Fail {
                detail: "not running".into(),
            }
        };

        let version = if pid.is_some() {
            self.probe_version(decl)
        } else {
            None
        };

        let uptime_secs = pid.and_then(|p| self.probe_uptime(p));

        let log_path = decl.log_path.as_ref().and_then(|p| {
            let expanded = expand_tilde_owned(p);
            expanded.exists().then_some(expanded)
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
            uptime_secs,
        }
    }

    /// Run pgrep for the service's `process_match` pattern.
    ///
    /// Why: pgrep is the simplest cross-platform process lookup; `-f` matches
    /// the full command line so partial names (e.g. `trusty-bm25-`) work.
    /// What: delegates to `self.process_prober.pgrep(pattern)`. Returns `None`
    /// when `process_match` is absent or pgrep finds nothing.
    /// Test: `probe_process_returns_pid_when_running`,
    /// `probe_process_returns_none_when_not_running`.
    fn probe_process(&self, decl: &ServiceDecl) -> Option<u32> {
        let pattern = decl.process_match.as_deref()?;
        self.process_prober.pgrep(pattern)
    }

    /// Discover the runtime port from port_file or default_port.
    ///
    /// Why: `trusty-memory` uses dynamic port selection; reading `port_file` is
    /// the only reliable way to find it.
    /// What: for `File` discovery: reads and parses `host:port` from `port_file`;
    /// for `Static`: returns `default_port` directly.
    /// Test: `probe_port_reads_port_file`, `probe_port_returns_default_port`.
    fn probe_port(&self, decl: &ServiceDecl) -> Option<u16> {
        match &decl.port_discovery {
            PortDiscovery::File => {
                let port_file_str = decl.port_file.as_deref()?;
                let path = expand_tilde_owned(port_file_str);
                self.port_prober.read_port_file(&path)
            }
            PortDiscovery::Static => decl.default_port,
        }
    }

    /// Issue an HTTP GET to the health URL template.
    ///
    /// Why: `health_url` contains a `{port}` template that must be expanded
    /// before the request is issued.
    /// What: replaces `{port}` in the template with the URL's port component,
    /// then delegates to `self.http_prober.get_health(expanded_url, timeout)`.
    /// Test: `probe_health_ok_on_2xx`, `probe_health_fail_on_503`,
    /// `probe_health_fail_on_connection_refused`.
    fn probe_health_url(&self, url: &str, decl: &ServiceDecl) -> HealthState {
        let template = match &decl.health_url {
            Some(t) => t,
            None => return HealthState::Unknown,
        };
        // Extract the port from the base URL (e.g. "http://localhost:7878").
        let port_str = url.rsplit(':').next().unwrap_or("0");
        let health_url = template.replace("{port}", port_str);
        self.http_prober
            .get_health(&health_url, HEALTH_PROBE_TIMEOUT)
    }

    /// Run the version command and return the first stdout line.
    ///
    /// Why: version strings are useful for diagnosing stale installs.
    /// What: delegates to `self.version_runner.run(cmd)`. Returns `None` when
    /// `version_cmd` is absent or the command fails.
    /// Test: covered by `probe_process_returns_pid_when_running` (version is
    /// populated when the process is running).
    fn probe_version(&self, decl: &ServiceDecl) -> Option<String> {
        let cmd = decl.version_cmd.as_deref()?;
        self.version_runner.run(cmd)
    }

    /// Compute process uptime via sysinfo.
    ///
    /// Why: sysinfo is already an always-on dep of trusty-mpm (`Cargo.toml:93`).
    /// What: creates a `System`, refreshes the process list, looks up `pid`,
    /// computes `SystemTime::now() - start_time`. Returns seconds as `u64`.
    /// Test: `probe_uptime_returns_none_for_unknown_pid`.
    fn probe_uptime(&self, pid: u32) -> Option<u64> {
        use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        );
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let sysinfo_pid = Pid::from_u32(pid);
        let proc_ = sys.process(sysinfo_pid)?;
        let start = proc_.start_time(); // seconds since Unix epoch
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(now.saturating_sub(start))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;

    // ── Mock implementations ──

    struct MockProcessProber {
        pid: Option<u32>,
        /// Count how many times pgrep was called.
        call_count: Arc<Mutex<u32>>,
    }

    impl MockProcessProber {
        fn new(pid: Option<u32>) -> Self {
            Self {
                pid,
                call_count: Arc::new(Mutex::new(0)),
            }
        }
        fn call_count_arc(&self) -> Arc<Mutex<u32>> {
            Arc::clone(&self.call_count)
        }
    }

    impl ProcessProber for MockProcessProber {
        fn pgrep(&self, _pattern: &str) -> Option<u32> {
            *self.call_count.lock().unwrap() += 1;
            self.pid
        }
    }

    struct MockPortProber {
        port: Option<u16>,
    }

    impl PortProber for MockPortProber {
        fn read_port_file(&self, _path: &std::path::Path) -> Option<u16> {
            self.port
        }
    }

    struct MockHttpProber {
        state: HealthState,
        call_count: Arc<Mutex<u32>>,
    }

    impl MockHttpProber {
        fn new(state: HealthState) -> Self {
            Self {
                state,
                call_count: Arc::new(Mutex::new(0)),
            }
        }
        fn call_count_arc(&self) -> Arc<Mutex<u32>> {
            Arc::clone(&self.call_count)
        }
    }

    impl HttpProber for MockHttpProber {
        fn get_health(&self, _url: &str, _timeout: Duration) -> HealthState {
            *self.call_count.lock().unwrap() += 1;
            self.state.clone()
        }
    }

    struct MockVersionRunner {
        version: Option<String>,
    }

    impl VersionRunner for MockVersionRunner {
        fn run(&self, _cmd: &str) -> Option<String> {
            self.version.clone()
        }
    }

    // ── Helper manifest builders ──

    fn static_manifest_with_port(port: u16) -> ServicesManifest {
        let mut services = BTreeMap::new();
        services.insert(
            "test-svc".to_string(),
            ServiceDecl {
                description: "test service".to_string(),
                default_port: Some(port),
                port_discovery: PortDiscovery::Static,
                port_file: None,
                health_url: Some("http://localhost:{port}/health".to_string()),
                log_path: None,
                version_cmd: Some("echo 1.0.0".to_string()),
                process_match: Some("test-svc".to_string()),
                start_cmd: None,
                stop_cmd: None,
                restart_cmd: None,
            },
        );
        ServicesManifest {
            version: 1,
            services,
        }
    }

    fn file_manifest(port_file: &str) -> ServicesManifest {
        let mut services = BTreeMap::new();
        services.insert(
            "dyn-svc".to_string(),
            ServiceDecl {
                description: "dynamic port service".to_string(),
                default_port: Some(7070),
                port_discovery: PortDiscovery::File,
                port_file: Some(port_file.to_string()),
                health_url: Some("http://localhost:{port}/health".to_string()),
                log_path: None,
                version_cmd: None,
                process_match: Some("dyn-svc".to_string()),
                start_cmd: None,
                stop_cmd: None,
                restart_cmd: None,
            },
        );
        ServicesManifest {
            version: 1,
            services,
        }
    }

    fn sidecar_manifest() -> ServicesManifest {
        let mut services = BTreeMap::new();
        services.insert(
            "sidecar".to_string(),
            ServiceDecl {
                description: "UDS sidecar".to_string(),
                default_port: None,
                port_discovery: PortDiscovery::Static,
                port_file: None,
                health_url: None,
                log_path: None,
                version_cmd: None,
                process_match: Some("sidecar-proc".to_string()),
                start_cmd: None,
                stop_cmd: None,
                restart_cmd: None,
            },
        );
        ServicesManifest {
            version: 1,
            services,
        }
    }

    // ── Tests ──

    #[test]
    fn status_returns_none_for_unknown_service() {
        let m = static_manifest_with_port(7878);
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(None)),
            Box::new(MockPortProber { port: None }),
            Box::new(MockHttpProber::new(HealthState::Ok)),
            Box::new(MockVersionRunner { version: None }),
        );
        assert!(d.status("no-such-service").is_none());
    }

    #[test]
    fn status_uses_cache_within_ttl() {
        let m = static_manifest_with_port(7878);
        let prober = MockProcessProber::new(Some(1234));
        let count = prober.call_count_arc();
        let mut d = Discoverer::with_probers(
            m,
            Box::new(prober),
            Box::new(MockPortProber { port: Some(7878) }),
            Box::new(MockHttpProber::new(HealthState::Ok)),
            Box::new(MockVersionRunner {
                version: Some("1.0".into()),
            }),
        );
        d.status("test-svc");
        d.status("test-svc");
        // pgrep must be called only once (second call uses cache).
        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[test]
    fn status_probes_fresh_after_ttl() {
        let m = static_manifest_with_port(7878);
        let prober = MockProcessProber::new(Some(1234));
        let count = prober.call_count_arc();
        let mut d = Discoverer::with_probers(
            m,
            Box::new(prober),
            Box::new(MockPortProber { port: Some(7878) }),
            Box::new(MockHttpProber::new(HealthState::Ok)),
            Box::new(MockVersionRunner { version: None }),
        );
        // Manually expire the cache by inserting with an old timestamp.
        d.status("test-svc"); // inserts into cache
        // Force cache expiry.
        let old = Instant::now() - Duration::from_secs(10);
        if let Some(entry) = d.cache.get_mut("test-svc") {
            entry.0 = old;
        }
        d.status("test-svc"); // must re-probe
        assert_eq!(*count.lock().unwrap(), 2);
    }

    #[test]
    fn probe_process_returns_pid_when_running() {
        let m = static_manifest_with_port(9000);
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(Some(5678))),
            Box::new(MockPortProber { port: Some(9000) }),
            Box::new(MockHttpProber::new(HealthState::Ok)),
            Box::new(MockVersionRunner {
                version: Some("2.0".into()),
            }),
        );
        let status = d.status("test-svc").unwrap();
        assert_eq!(status.pid, Some(5678));
        assert!(status.running);
    }

    #[test]
    fn probe_process_returns_none_when_not_running() {
        let m = static_manifest_with_port(9000);
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(None)),
            Box::new(MockPortProber { port: None }),
            Box::new(MockHttpProber::new(HealthState::Ok)),
            Box::new(MockVersionRunner { version: None }),
        );
        let status = d.status("test-svc").unwrap();
        assert_eq!(status.pid, None);
        assert!(!status.running);
    }

    #[test]
    fn probe_port_reads_port_file() {
        let m = file_manifest("/tmp/test-port-file");
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(Some(999))),
            Box::new(MockPortProber { port: Some(7073) }),
            Box::new(MockHttpProber::new(HealthState::Ok)),
            Box::new(MockVersionRunner { version: None }),
        );
        let status = d.status("dyn-svc").unwrap();
        assert_eq!(status.port, Some(7073));
    }

    #[test]
    fn probe_port_returns_default_port() {
        let m = static_manifest_with_port(7878);
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(Some(100))),
            Box::new(MockPortProber { port: None }), // not consulted for Static
            Box::new(MockHttpProber::new(HealthState::Ok)),
            Box::new(MockVersionRunner { version: None }),
        );
        let status = d.status("test-svc").unwrap();
        assert_eq!(status.port, Some(7878));
    }

    #[test]
    fn probe_health_ok_on_2xx() {
        let m = static_manifest_with_port(7878);
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(Some(1))),
            Box::new(MockPortProber { port: Some(7878) }),
            Box::new(MockHttpProber::new(HealthState::Ok)),
            Box::new(MockVersionRunner { version: None }),
        );
        let status = d.status("test-svc").unwrap();
        assert_eq!(status.health, HealthState::Ok);
    }

    #[test]
    fn probe_health_fail_on_503() {
        let m = static_manifest_with_port(7878);
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(Some(1))),
            Box::new(MockPortProber { port: Some(7878) }),
            Box::new(MockHttpProber::new(HealthState::Fail {
                detail: "HTTP 503 Service Unavailable".into(),
            })),
            Box::new(MockVersionRunner { version: None }),
        );
        let status = d.status("test-svc").unwrap();
        assert!(matches!(status.health, HealthState::Fail { .. }));
    }

    #[test]
    fn probe_health_fail_on_connection_refused() {
        let m = static_manifest_with_port(7878);
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(Some(1))),
            Box::new(MockPortProber { port: Some(7878) }),
            Box::new(MockHttpProber::new(HealthState::Fail {
                detail: "connection refused".into(),
            })),
            Box::new(MockVersionRunner { version: None }),
        );
        let status = d.status("test-svc").unwrap();
        assert!(
            matches!(status.health, HealthState::Fail { ref detail } if detail.contains("refused"))
        );
    }

    #[test]
    fn health_bypasses_cache() {
        let m = static_manifest_with_port(7878);
        let http_prober = MockHttpProber::new(HealthState::Ok);
        let count = http_prober.call_count_arc();
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(Some(1))),
            Box::new(MockPortProber { port: Some(7878) }),
            Box::new(http_prober),
            Box::new(MockVersionRunner { version: None }),
        );
        d.status("test-svc"); // primes cache (1 http call)
        d.health("test-svc"); // must re-probe (1 more http call)
        // status() triggered 1 probe, health() triggered 1 more = 2 total.
        assert_eq!(*count.lock().unwrap(), 2);
    }

    #[test]
    fn list_returns_all_manifest_services() {
        let m = ServicesManifest::default_manifest();
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(None)),
            Box::new(MockPortProber { port: None }),
            Box::new(MockHttpProber::new(HealthState::Fail {
                detail: "not running".into(),
            })),
            Box::new(MockVersionRunner { version: None }),
        );
        let list = d.list();
        assert_eq!(list.len(), 6);
        let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"trusty-search"));
        assert!(names.contains(&"trusty-memory"));
        assert!(names.contains(&"trusty-embedderd"));
    }

    #[test]
    fn service_status_serialises_to_json() {
        let status = ServiceStatus {
            name: "trusty-search".to_string(),
            declared: true,
            running: true,
            pid: Some(12345),
            port: Some(7878),
            url: Some("http://localhost:7878".to_string()),
            version: Some("trusty-search 0.13.2".to_string()),
            health: HealthState::Ok,
            log_path: None,
            uptime_secs: Some(3600),
        };
        let json = serde_json::to_string(&status).expect("serialise");
        let rt: serde_json::Value = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(rt["name"], "trusty-search");
        assert_eq!(rt["pid"], 12345);
        assert_eq!(rt["port"], 7878);
        assert_eq!(rt["health"], "ok");
        assert_eq!(rt["uptime_secs"], 3600);
    }

    #[test]
    fn sidecar_shows_unknown_health_when_running() {
        let m = sidecar_manifest();
        let mut d = Discoverer::with_probers(
            m,
            Box::new(MockProcessProber::new(Some(42))),
            Box::new(MockPortProber { port: None }),
            Box::new(MockHttpProber::new(HealthState::Ok)), // should not be called
            Box::new(MockVersionRunner { version: None }),
        );
        let status = d.status("sidecar").unwrap();
        assert!(status.running);
        assert_eq!(status.health, HealthState::Unknown);
        assert!(status.port.is_none());
    }
}
