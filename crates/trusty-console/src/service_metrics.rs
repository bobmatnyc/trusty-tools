//! Per-service CPU and memory sampling for the home-page graphs (#6642, #6773).
//!
//! Why: `GET /api/console/services` says WHETHER each service is running. The
//! owner's home-page ruling needs how HARD it is running and how much memory it
//! holds, once a second. Nothing in the console measured another process before
//! this module.
//!
//! Why one sampler for both figures (#6773): the owner's ruling puts a CPU graph
//! and a memory graph side by side in every row. Both are read off ONE `sysinfo`
//! refresh per tick, so the pair always shows the same second — a second sampler
//! or a second timer is the one way side-by-side graphs can lie.
//!
//! Why pid discovery is a separate step from sampling: a pid costs I/O to find
//! (a lock file to read, a socket to dial) and a reading costs a syscall on
//! an already-known pid. Doing both every tick would put a file read and six
//! socket dials on a one-second loop. [`ServiceMetricsSampler::pending_lookups`]
//! names the services that still need a pid, [`resolve_pid`] does the I/O off
//! the reactor, and [`ServiceMetricsSampler::record_lookups`] feeds the answers
//! back — so steady state is one `sysinfo` refresh per second and nothing else.
//!
//! Which services yield a pid, and why the rest do not:
//!
//! | Service | pid source |
//! |---|---|
//! | `trusty-mpm` | the `pid = N` line in its `daemon.lock` |
//! | `trusty-search` | `LOCAL_PEERPID` / `SO_PEERCRED` on its Unix socket |
//! | `trusty-memory` | the same, on its own Unix socket |
//! | `trusty-agents` | none — it serves TCP loopback, and a TCP peer carries no pid |
//! | `trusty-analyze`, `trusty-review` | none — on-demand members with no resident process |
//!
//! A service with no pid reports `cpu_pct: None`. It does NOT report `0.0`: an
//! idle bar and an unmeasurable one must look different on the card.
//!
//! Fail-open contract: a pid that will not resolve, a process that exits between
//! two samples, and a sampler error each yield `None` for that ONE service and
//! leave the tick running for the others. Nothing here returns an error, and
//! nothing here can stop the loop.
//! Test: the inline `tests` module, plus
//! `machine_history::tests::the_service_ring_bounds_what_history_returns` for
//! what happens to the samples afterwards.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use trusty_common::sys_metrics::ProcessCpuSampler;

use crate::connector::{ServiceInfo, ServiceStatus};
use crate::machine_history::service_samples::{ServiceSample, ServiceSampleBatch};

/// How long before a failed pid lookup is retried.
///
/// Why: a service that is `Running` but whose process the console cannot
/// identify — trusty-agents, always — would otherwise be looked up every second
/// forever. Fifteen seconds matches the health poller's cadence, which is the
/// rate at which the answer can actually change.
/// What: `15s`.
/// Test: `a_failed_lookup_is_not_retried_immediately`.
const LOOKUP_BACKOFF: Duration = Duration::from_secs(15);

/// Read a service's pid, or `None` when it cannot be identified.
///
/// Why free rather than a method: it does blocking I/O (a file read, a socket
/// dial) and the caller runs it on the blocking pool, away from the sampler's
/// borrowed state.
/// What: dispatches on the service id to the one discovery artifact that service
/// actually publishes — see the table in the module docs. An id with no known
/// source returns `None` immediately, doing no I/O at all.
/// Test: `resolve_pid_is_none_for_a_service_with_no_source`,
/// `mpm_pid_is_read_from_the_lock_file`.
#[must_use]
pub fn resolve_pid(service_id: &str) -> Option<u32> {
    match service_id {
        "trusty-mpm" => mpm_lock_pid(&mpm_lock_path()?),
        "trusty-search" => socket_peer_pid(crate::search_uds::socket_path().ok()?),
        "trusty-memory" => {
            socket_peer_pid(trusty_common::daemon_socket_path("trusty-memory").ok()?)
        }
        _ => None,
    }
}

/// Where trusty-mpm's daemon lock lives.
fn mpm_lock_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".trusty-mpm").join("daemon.lock"))
}

/// Extract the `pid` field from a trusty-mpm `daemon.lock` TOML body.
///
/// Why a line scan rather than a TOML parse: `detect::mpm` already reads this
/// same file for its `addr` field with a line scan, for the reason recorded
/// there — one field does not justify a TOML dependency in the console.
/// What: splits each line on the FIRST `=`, matches the key EXACTLY against
/// `pid` (so `pid_file` is rejected), and parses the value. Returns `None` when
/// the file is absent, unreadable, or carries no well-formed `pid`.
/// Test: `mpm_pid_is_read_from_the_lock_file`,
/// `mpm_pid_rejects_a_prefixed_key`, `mpm_pid_is_none_for_a_missing_file`.
fn mpm_lock_pid(path: &std::path::Path) -> Option<u32> {
    let body = std::fs::read_to_string(path).ok()?;
    for line in body.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "pid" {
            continue;
        }
        if let Ok(pid) = value.trim().trim_matches('"').parse::<u32>()
            && pid > 0
        {
            return Some(pid);
        }
    }
    None
}

/// Dial `socket` and ask the kernel which process is listening.
///
/// Why a connect rather than a health call: the pid is a property of the
/// CONNECTION, and the kernel answers it the moment the connect completes. There
/// is no request to send and no response to parse, so this is strictly cheaper
/// than the detection probe the connector already runs against the same path.
/// What: a blocking `UnixStream::connect`, converted to the tokio type
/// [`trusty_common::uds::peer_pid`] takes, then dropped. Any failure — no
/// socket, nothing listening, no peer-pid support on this platform — is `None`.
/// Test: `socket_peer_pid_reads_this_process_from_its_own_socket`,
/// `socket_peer_pid_is_none_when_nothing_is_listening`.
fn socket_peer_pid(socket: PathBuf) -> Option<u32> {
    let std_stream = std::os::unix::net::UnixStream::connect(&socket).ok()?;
    std_stream.set_nonblocking(true).ok()?;
    // `peer_pid` reads a socket option; it never awaits, so the stream does not
    // need a running reactor to be interrogated. `from_std` only registers it.
    let stream = tokio::net::UnixStream::from_std(std_stream).ok()?;
    trusty_common::uds::peer_pid(&stream)
}

/// Per-service CPU sampling across ticks (#6642).
///
/// Why it holds state: `sysinfo` derives CPU% from the delta between two
/// refreshes, so the underlying sampler must live for the console's lifetime,
/// and a resolved pid is worth keeping until its process exits.
/// What: one [`ProcessCpuSampler`] (the workspace's single multi-pid CPU entry
/// point), the service-id → pid map, and a per-service backoff clock for
/// lookups that failed. Not `Clone` — it carries mutable sampling state and is
/// owned by the one sampler task.
/// Test: see the inline `tests` module.
pub struct ServiceMetricsSampler {
    cpu: ProcessCpuSampler,
    pids: HashMap<String, u32>,
    retry_after: HashMap<String, Instant>,
}

impl Default for ServiceMetricsSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceMetricsSampler {
    /// An empty sampler that has resolved nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cpu: ProcessCpuSampler::new(),
            pids: HashMap::new(),
            retry_after: HashMap::new(),
        }
    }

    /// Which services need a pid lookup on this tick.
    ///
    /// Why only `Running` services: an `Absent` or `Available` member has no
    /// resident process to find, so dialling its socket would be I/O that cannot
    /// succeed. `Degraded` counts as running — a daemon answering but missing a
    /// tool is still a process burning CPU.
    /// What: the ids that are running, have no pid recorded, and are not inside
    /// their [`LOOKUP_BACKOFF`] window.
    /// Test: `pending_lookups_skips_a_service_that_is_not_running`,
    /// `a_failed_lookup_is_not_retried_immediately`.
    #[must_use]
    pub fn pending_lookups(&self, services: &[ServiceInfo], now: Instant) -> Vec<String> {
        services
            .iter()
            .filter(|s| is_live(&s.status))
            .filter(|s| !self.pids.contains_key(&s.id))
            .filter(|s| self.retry_after.get(&s.id).is_none_or(|at| now >= *at))
            .map(|s| s.id.clone())
            .collect()
    }

    /// Record what [`resolve_pid`] found for each pending service.
    ///
    /// Why the failures are recorded too: without a backoff entry, a service
    /// that can never yield a pid would be looked up on every tick forever.
    /// What: a `Some` starts tracking the pid (priming its CPU baseline, so the
    /// NEXT tick already carries a real figure); a `None` arms the backoff.
    /// Test: `a_failed_lookup_is_not_retried_immediately`,
    /// `a_resolved_pid_is_tracked`.
    pub fn record_lookups(&mut self, found: Vec<(String, Option<u32>)>, now: Instant) {
        for (id, pid) in found {
            match pid {
                Some(pid) => {
                    self.cpu.track(pid);
                    self.pids.insert(id.clone(), pid);
                    self.retry_after.remove(&id);
                }
                None => {
                    self.retry_after.insert(id, now + LOOKUP_BACKOFF);
                }
            }
        }
    }

    /// Take one tick's sample for every service.
    ///
    /// Why the whole roster and not just the running half: the card grid renders
    /// every registered service, and a row that stopped emitting samples when
    /// its daemon stopped would leave the graph frozen at its last value rather
    /// than showing the gap.
    /// What: refreshes every tracked pid in ONE `sysinfo` call — the tracked
    /// pids only, never the process table — then reads BOTH figures per service
    /// off that one refresh (#6773), so the CPU graph and the memory graph in a
    /// row always show the same second. A service whose pid vanished has already
    /// been dropped by the refresh, so it reads `None` and its entry is
    /// forgotten here, which lets
    /// [`ServiceMetricsSampler::pending_lookups`] rediscover it after a restart.
    /// Never returns an error and never panics: an unmeasurable service is one
    /// `None` among the others.
    /// Test: `a_vanished_pid_samples_as_none_and_the_tick_continues`,
    /// `every_service_gets_a_sample`,
    /// `a_live_pid_samples_cpu_and_memory_on_one_tick`.
    #[must_use]
    pub fn sample(&mut self, services: &[ServiceInfo], sampled_at_unix: u64) -> ServiceSampleBatch {
        self.cpu.refresh();

        let mut samples = Vec::with_capacity(services.len());
        for service in services {
            // #6773: one lookup, both figures — a second read path could hand
            // the two graphs different seconds.
            let (cpu_pct, rss_bytes) = match self.pids.get(&service.id) {
                Some(pid) => (self.cpu.cpu_pct(*pid), self.cpu.rss_bytes(*pid)),
                None => (None, None),
            };
            // #6642: a tracked pid whose process is gone reads None; forget it
            // so the next tick can rediscover the restarted daemon.
            if cpu_pct.is_none()
                && rss_bytes.is_none()
                && let Some(pid) = self.pids.remove(&service.id)
            {
                self.cpu.untrack(pid);
            }
            samples.push(ServiceSample {
                id: service.id.clone(),
                status: service.status.clone(),
                cpu_pct,
                rss_bytes,
            });
        }

        ServiceSampleBatch {
            sampled_at_unix,
            services: samples,
        }
    }

    /// The pid currently recorded for `service_id`, if any.
    ///
    /// Test: `a_resolved_pid_is_tracked`.
    #[must_use]
    pub fn pid_of(&self, service_id: &str) -> Option<u32> {
        self.pids.get(service_id).copied()
    }
}

/// Stamp each entry with the newest sample the history holds (#6642, #6773).
///
/// Why here rather than in the route handler: it belongs to this module's
/// subject, and `server::mod` is at its SLOC cap. Why on the ROUTE rather than
/// in detection: a connector is a synchronous probe with no sampler, and the
/// numbers must come from the same rings the graphs read or the list and the
/// newest bar could disagree.
/// What: overwrites `ServiceInfo::cpu_pct` and `ServiceInfo::rss_bytes` from the
/// newest per-service sample, and leaves the rest `None` — which the route
/// serialises as an explicit `null`. Before the first tick the history is empty
/// and every entry stays `None`, which is the correct reading of "not measured
/// yet". Both fields come from ONE sample, so the two columns cannot describe
/// different seconds.
/// Test: `the_overlay_stamps_only_the_services_with_a_measurement`.
pub async fn apply_metrics_overlay(
    services: &mut [ServiceInfo],
    history: &crate::machine_history::MachineHistory,
) {
    let latest = history.latest_service_metrics().await;
    for service in services.iter_mut() {
        let newest = latest.get(&service.id);
        service.cpu_pct = newest.and_then(|s| s.cpu_pct);
        service.rss_bytes = newest.and_then(|s| s.rss_bytes);
    }
}

/// Does this status mean a process should exist right now?
///
/// Why `Degraded` counts: a daemon that answers but is missing a tool is still a
/// running process with real CPU, and hiding its graph would hide the case an
/// operator most wants to look at.
/// Test: `pending_lookups_skips_a_service_that_is_not_running`.
fn is_live(status: &ServiceStatus) -> bool {
    matches!(status, ServiceStatus::Running | ServiceStatus::Degraded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::ServiceLifecycle;

    fn info(id: &str, status: ServiceStatus) -> ServiceInfo {
        ServiceInfo {
            id: id.to_string(),
            display_name: id.to_string(),
            status,
            version: None,
            url: None,
            hint: None,
            lifecycle: ServiceLifecycle::Daemon,
            cpu_pct: None,
            rss_bytes: None,
        }
    }

    /// Spawn a child that sleeps, so a test has a real pid it owns.
    ///
    /// Why not a spinner: this test suite must never generate machine-wide load
    /// to prove a CPU reading. The assertions are about a measurement being
    /// taken and dropped, not about it reaching a value.
    fn spawn_sleeper() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a sleeping child")
    }

    /// Why: the whole card grid renders from this batch, so a service the
    /// sampler skipped is a row with a frozen graph.
    /// What: samples four services with no pids resolved and asserts one entry
    /// each, every `cpu_pct` absent rather than zero.
    /// Test: this test itself.
    #[test]
    fn every_service_gets_a_sample() {
        let services = vec![
            info("trusty-search", ServiceStatus::Running),
            info("trusty-review", ServiceStatus::Available),
            info("trusty-analyze", ServiceStatus::Absent),
            info("trusty-agents", ServiceStatus::Degraded),
        ];
        let batch = ServiceMetricsSampler::new().sample(&services, 42);

        assert_eq!(batch.sampled_at_unix, 42);
        assert_eq!(batch.services.len(), 4);
        for sample in &batch.services {
            assert_eq!(
                sample.cpu_pct, None,
                "{} has no pid, so it must report no measurement rather than 0.0",
                sample.id
            );
        }
    }

    /// Why: dialling a socket for a service that is not running is I/O that
    /// cannot succeed, on a one-second loop.
    /// What: asserts only the running and degraded rows are pending.
    /// Test: this test itself.
    #[test]
    fn pending_lookups_skips_a_service_that_is_not_running() {
        let services = vec![
            info("trusty-search", ServiceStatus::Running),
            info("trusty-memory", ServiceStatus::Available),
            info("trusty-review", ServiceStatus::Absent),
            info("trusty-mpm", ServiceStatus::Degraded),
        ];
        let mut pending = ServiceMetricsSampler::new().pending_lookups(&services, Instant::now());
        pending.sort();
        assert_eq!(pending, vec!["trusty-mpm", "trusty-search"]);
    }

    /// REGRESSION (#6642): a lookup that failed must not be retried on the very
    /// next tick.
    ///
    /// Why: trusty-agents can never yield a pid — it serves TCP loopback. Without
    /// a backoff the console would attempt a resolution for it every second for
    /// as long as it runs.
    /// What: records a failed lookup, asserts the service is not pending
    /// immediately, and that it is pending again once the backoff has elapsed.
    /// Test: this test itself.
    #[test]
    fn a_failed_lookup_is_not_retried_immediately() {
        let services = vec![info("trusty-agents", ServiceStatus::Running)];
        let now = Instant::now();
        let mut sampler = ServiceMetricsSampler::new();

        assert_eq!(sampler.pending_lookups(&services, now).len(), 1);
        sampler.record_lookups(vec![("trusty-agents".to_string(), None)], now);
        assert!(
            sampler.pending_lookups(&services, now).is_empty(),
            "a failed lookup must back off, not retry on the next tick"
        );
        assert_eq!(
            sampler
                .pending_lookups(&services, now + LOOKUP_BACKOFF)
                .len(),
            1,
            "the backoff must expire so a daemon that starts later is found"
        );
    }

    /// Why: a resolved pid must enter the CPU sampler, or the reading never
    /// arrives however well discovery worked.
    /// What: resolves a live child's pid, asserts it is recorded and tracked and
    /// that the sample carries a figure, then reaps the child.
    /// Test: this test itself.
    #[test]
    fn a_resolved_pid_is_tracked() {
        let mut child = spawn_sleeper();
        let pid = child.id();
        let services = vec![info("trusty-search", ServiceStatus::Running)];

        let mut sampler = ServiceMetricsSampler::new();
        sampler.record_lookups(
            vec![("trusty-search".to_string(), Some(pid))],
            Instant::now(),
        );
        let batch = sampler.sample(&services, 1);

        let recorded = sampler.pid_of("trusty-search");
        let cpu = batch.services[0].cpu_pct;

        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(recorded, Some(pid));
        assert!(
            cpu.is_some(),
            "a live tracked process must produce a measurement"
        );
    }

    /// REGRESSION (#6773): ONE `sample` call must carry both figures for a live
    /// service, so the row's two graphs always draw the same second.
    ///
    /// Why: before #6773 the sampler asked `sysinfo` for CPU only. Adding the
    /// memory graph without this would leave `rss_bytes` `None` on every tick —
    /// a permanently empty memory graph beside a working CPU graph, with nothing
    /// red anywhere to say so. Sampling memory on a second tick would instead
    /// let the two graphs disagree about which second they show.
    /// What: tracks a real child, takes one tick, and asserts both fields of
    /// that single sample are present with a plausible byte figure.
    /// Test: this test itself.
    #[test]
    fn a_live_pid_samples_cpu_and_memory_on_one_tick() {
        let mut child = spawn_sleeper();
        let pid = child.id();
        let services = vec![info("trusty-search", ServiceStatus::Running)];

        let mut sampler = ServiceMetricsSampler::new();
        sampler.record_lookups(
            vec![("trusty-search".to_string(), Some(pid))],
            Instant::now(),
        );
        let batch = sampler.sample(&services, 1);
        let sample = batch.services[0].clone();

        let _ = child.kill();
        let _ = child.wait();

        assert!(sample.cpu_pct.is_some(), "the CPU half of the tick");
        let rss = sample
            .rss_bytes
            .expect("the memory half of the SAME tick — one refresh serves both");
        assert!(rss > 0, "a live process occupies memory, got {rss} bytes");
        assert!(
            rss < 1024 * 1024 * 1024 * 1024,
            "implausibly large ({rss}) — the unit must be bytes"
        );
    }

    /// REGRESSION (#6642): a process that exits between two samples reports
    /// `None`, its pid is forgotten, and the tick keeps producing samples for
    /// every other service.
    ///
    /// Why: this is the fail-open contract. A sampler that panicked, stalled, or
    /// reported `0.0` on a dead daemon would each be a distinct silent failure.
    /// What: tracks a real child alongside a service that never had a pid, kills
    /// and reaps the child, samples again, and asserts both rows are present,
    /// both `cpu_pct` are `None`, and the dead pid was dropped.
    /// Test: this test itself.
    #[test]
    fn a_vanished_pid_samples_as_none_and_the_tick_continues() {
        let mut child = spawn_sleeper();
        let pid = child.id();
        let services = vec![
            info("trusty-search", ServiceStatus::Running),
            info("trusty-agents", ServiceStatus::Running),
        ];

        let mut sampler = ServiceMetricsSampler::new();
        sampler.record_lookups(
            vec![("trusty-search".to_string(), Some(pid))],
            Instant::now(),
        );
        // Prime the CPU delta; the reading itself is asserted on tick 2.
        let _ = sampler.sample(&services, 1);

        child.kill().expect("kill the sleeper");
        child.wait().expect("reap the sleeper");

        let batch = sampler.sample(&services, 2);
        assert_eq!(
            batch.services.len(),
            2,
            "the tick must not stop or truncate"
        );
        assert_eq!(
            batch.services[0].cpu_pct, None,
            "a vanished process reports no measurement, never 0.0"
        );
        assert_eq!(batch.services[1].cpu_pct, None);
        assert_eq!(
            sampler.pid_of("trusty-search"),
            None,
            "the dead pid must be forgotten so a restart can be rediscovered"
        );

        // And the tick after that still works.
        assert_eq!(sampler.sample(&services, 3).services.len(), 2);
    }

    /// Why (#6642, #6773): the list must render its CPU and memory figures on
    /// first paint, and a service with no measurement must stay `None` rather
    /// than inherit a neighbour's number or fall back to zero.
    /// What: records one measured and one unmeasurable service into a real
    /// history, then overlays a three-entry list and asserts exactly one entry
    /// was stamped — in BOTH fields, since the memory column is overlaid the
    /// same way and from the same sample.
    /// Test: this test itself.
    #[tokio::test]
    async fn the_overlay_stamps_only_the_services_with_a_measurement() {
        let history = crate::machine_history::MachineHistory::new();
        history
            .record_service_samples(ServiceSampleBatch {
                sampled_at_unix: 1,
                services: vec![
                    ServiceSample {
                        id: "trusty-search".to_string(),
                        status: ServiceStatus::Running,
                        cpu_pct: Some(7.5),
                        rss_bytes: Some(148_897_792),
                    },
                    ServiceSample {
                        id: "trusty-review".to_string(),
                        status: ServiceStatus::Available,
                        cpu_pct: None,
                        rss_bytes: None,
                    },
                ],
            })
            .await;

        let mut services = vec![
            info("trusty-search", ServiceStatus::Running),
            info("trusty-review", ServiceStatus::Available),
            info("trusty-agents", ServiceStatus::Absent),
        ];
        apply_metrics_overlay(&mut services, &history).await;

        assert_eq!(services[0].cpu_pct, Some(7.5));
        assert_eq!(services[0].rss_bytes, Some(148_897_792));
        assert_eq!(
            services[1].cpu_pct, None,
            "a sampled-but-unmeasurable service stays null"
        );
        assert_eq!(
            services[1].rss_bytes, None,
            "a sampled-but-unmeasurable service stays null in memory too"
        );
        assert_eq!(
            services[2].cpu_pct, None,
            "a service the history never saw stays null"
        );
        assert_eq!(services[2].rss_bytes, None);
    }

    /// Why: the three services with no discovery artifact must cost no I/O at
    /// all, or the one-second loop pays for a lookup that can never succeed.
    /// What: asserts `resolve_pid` answers `None` for an unknown id.
    /// Test: this test itself.
    #[test]
    fn resolve_pid_is_none_for_a_service_with_no_source() {
        assert_eq!(resolve_pid("trusty-agents"), None);
        assert_eq!(resolve_pid("trusty-review"), None);
        assert_eq!(resolve_pid("not-a-service"), None);
    }

    /// Why: trusty-mpm is the one member that publishes its pid on disk, so the
    /// parse is the whole discovery path for it.
    /// What: writes a lock file in the shape the daemon writes and asserts the
    /// pid comes back.
    /// Test: this test itself.
    #[test]
    fn mpm_pid_is_read_from_the_lock_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock = tmp.path().join("daemon.lock");
        std::fs::write(
            &lock,
            "pid = 4242\naddr = \"http://127.0.0.1:7880\"\nstarted_at = \"x\"\n",
        )
        .expect("write");
        assert_eq!(mpm_lock_pid(&lock), Some(4242));
    }

    /// Why: `pid_file = "…"` in the same TOML must not be read as the pid — the
    /// sibling `addr` parser in `detect::mpm` records the same finding.
    /// What: asserts a prefixed key, a zero pid, and a missing file all yield
    /// `None`.
    /// Test: this test itself.
    #[test]
    fn mpm_pid_rejects_a_prefixed_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock = tmp.path().join("daemon.lock");
        std::fs::write(&lock, "pid_file = \"/tmp/x\"\npidx = 7\n").expect("write");
        assert_eq!(mpm_lock_pid(&lock), None);

        std::fs::write(&lock, "pid = 0\n").expect("write");
        assert_eq!(mpm_lock_pid(&lock), None, "pid 0 is not a process");
    }

    /// Why: a stale lock file removed under the console must degrade to no
    /// measurement, not to an error.
    /// Test: this test itself.
    #[test]
    fn mpm_pid_is_none_for_a_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(mpm_lock_pid(&tmp.path().join("absent.lock")), None);
    }

    /// Why (#6642): trusty-search and trusty-memory publish no pid anywhere, so
    /// the peer pid of their socket is the entire discovery path. A stub here
    /// leaves both graphs permanently empty.
    /// What: binds a socket in this process, dials it through the same helper
    /// the sampler uses, and asserts the pid is this process's own.
    /// Test: this test itself.
    #[tokio::test]
    async fn socket_peer_pid_reads_this_process_from_its_own_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("peer.sock");
        let _listener = tokio::net::UnixListener::bind(&path).expect("bind");

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert_eq!(socket_peer_pid(path), Some(std::process::id()));
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        assert_eq!(socket_peer_pid(path), None);
    }

    /// Why: a daemon that is not running must degrade to no measurement rather
    /// than to a hang or an error.
    /// What: dials a path nothing is listening on.
    /// Test: this test itself.
    #[test]
    fn socket_peer_pid_is_none_when_nothing_is_listening() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(socket_peer_pid(tmp.path().join("nothing.sock")), None);
    }
}
