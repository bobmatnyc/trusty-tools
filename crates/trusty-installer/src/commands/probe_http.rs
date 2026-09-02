//! Daemon health probe — the transport behind every `tctl` health verdict
//! (#4246), over HTTP or, since #6277, over a Unix socket.
//!
//! Why: `tctl` used to probe a daemon by spawning `<binary> health --json` — a
//! contract NO shipped daemon implements. Every distinct failure (clap exit 2
//! for a missing subcommand, an unsupported `--json` flag, a schema mismatch, a
//! hung child) collapsed into a single `MemberHealth::Down`, so `tctl status`
//! reported six healthy daemons as `down`. That false `down` then drove
//! `verify_tail::needs_kickstart` into `launchctl kickstart -k`, meaning **every
//! `tctl install` hard-restarted a fully healthy stack** — the real harm behind
//! #4246, up to and including a SIGKILL of trusty-search mid-index-flush (at
//! the time the shared plist renderer emitted no `ExitTimeOut` at all, so
//! launchd applied its measured 5s default, well inside search's 30s-per-index
//! flush floor — #4393 has since made the renderer declare the window).
//!
//! Every one of those daemons *does* answer a health query on a transport it
//! actually serves, so this module replaces the subprocess contract with that
//! transport. Since #6277 (ADR-0032) which transport that is varies per member:
//! trusty-analyze and trusty-memory serve a hardened Unix socket and are probed
//! through [`uds_socket_for`] / [`classify_rpc_response`]; trusty-review has no
//! transport at all since #6290 and is probed by presence ([`presence_only`]);
//! trusty-search serves BOTH transports while #6285 retires its axum surface
//! and is probed over both at once ([`dual_transport`]);
//! everything else still answers `GET /health` over loopback. Three properties are load-bearing and each has a
//! named regression test — remove any one of them and #4246 comes straight
//! back:
//!
//! 1. **[`build_probe_client`] is proxy-free.** reqwest 0.12 honours
//!    `HTTP_PROXY`/`http_proxy`/`ALL_PROXY` for `127.0.0.1` — hyper-util's proxy
//!    matcher has no loopback exemption, so a developer with a proxy exported
//!    reproduces the identical false `down` through the NEW transport. Since
//!    #4392 the `.no_proxy()` call itself lives in
//!    `trusty_common::http_client::loopback_client_builder`, which this module
//!    now builds on.
//! 2. **Dual resolution with an explicit precedence rule** — see [`reconcile`].
//!    A daemon that port-walked off its documented default (trusty-memory walks
//!    `7070..=7079`) makes the fixed-port leg refuse while it is perfectly
//!    healthy; treating that refusal as authoritative would kickstart a healthy
//!    daemon, i.e. re-ship the bug.
//! 3. **Classification reads the BODY and accepts non-2xx** — see
//!    [`classify_response`]. trusty-analyze answers **HTTP 503** with
//!    `status:"degraded"` when search is unreachable; a 2xx-only liveness check
//!    (`ensure::daemon::health_ok`) calls that `down` and kickstarts it, while
//!    a UDS member answers the same `degraded` in a result frame for the
//!    identical condition. [`classify_rpc_response`] applies the same body-first
//!    rule on the socket side, which is why a degraded UDS member is `Serving`
//!    and only an `error` frame is `RpcError`.
//!
//! What:
//! - [`ProbeOutcome`] — the typed verdict, replacing the old flat `String`. It
//!   keeps *why* a probe failed (`Refused` vs `Timeout` vs `HttpError` vs
//!   `RpcError` vs `BadEnvelope`) instead of collapsing them, which is what lets
//!   `verify_tail` gate its destructive repair on a genuine transport-level
//!   observation.
//! - [`probe_member_http_blocking`] — the sync entry point `super::probe` calls.
//! - [`resolve_probe_bases`] / [`probe_bases`] / [`reconcile`] /
//!   [`classify_response`] / [`effective_status`] / [`fixed_port_for`] — the
//!   composable halves, unit-tested without a live daemon.
//! - [`uds_socket_for`] / [`classify_rpc_response`] — the same split for the UDS
//!   members (#6277).
//!
//! Test: `tests` (sibling `probe_http_tests.rs`) — covers the proxy immunity,
//! the port-walk precedence rule, 503+degraded, envelope rejection, the
//! failure-cause table, and a table over the six REAL daemon payloads captured
//! off a live stack.

use std::time::Duration;

use super::up::member::MemberHealth;
use super::up::system_runner::classify_status;

/// TCP connect bound for one probe leg.
///
/// Why: a closed loopback port refuses instantly, but a firewalled or wedged one
/// can hang for the OS default (~75s) — far past `verify_tail`'s whole ~120s
/// aggregate budget. Bounding connect separately from the total request keeps a
/// dead leg cheap.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Whole-request bound for one probe leg (connect + headers + body).
///
/// Why: a live daemon answers `/health` in single-digit milliseconds; 5s is
/// generous for a loaded one and short enough that both legs of a dual probe
/// (run concurrently) can never exceed it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The health endpoint every trusty daemon mounts.
const HEALTH_PATH: &str = "/health";

/// How much of an unusable response body [`ProbeOutcome::BadEnvelope`] keeps.
///
/// Why: the sample exists to make a misconfiguration diagnosable, not to dump a
/// megabyte of a squatter's HTML into a status row.
const ENVELOPE_SAMPLE_LEN: usize = 120;

/// The status word a serving-but-not-yet-usable daemon is reported as.
const DEGRADED: &str = "degraded";

/// The documented default loopback port for a stable-set daemon.
///
/// Why: the `http_addr` discovery file is the primary resolution path, but it is
/// not universally reliable — trusty-search deliberately no-ops the shared write
/// under `TRUSTY_DATA_DIR`, and a daemon that exited uncleanly can leave a stale
/// one. The documented fixed port is the independent second leg (see
/// [`reconcile`] for how the two are combined).
/// What: the `docs/architecture/port-assignments.md` table, restricted to the
/// stable-set daemons `tctl` probes. `None` for anything else — a member with
/// neither a recorded address nor a known default yields
/// [`ProbeOutcome::NoAddress`] rather than a guessed port (a wrong guessed port
/// fails worse than a clean skip: it invites the #3364 squatter class).
/// Test: `tests::fixed_ports_match_port_assignments_doc`.
pub fn fixed_port_for(binary: &str) -> Option<u16> {
    match binary {
        "trusty-console" => Some(7788),
        // #6285: trusty-search now answers `search.health` on a socket too, but
        // this row STAYS until the axum surface is deleted. It is what
        // [`dual_transport`] reads to know a second leg exists, and an
        // installed daemon older than the listener answers on nothing else.
        "trusty-search" => Some(7878),
        "trusty-mpm" => Some(7880),
        // #6277 / #6287 / #6286: NO trusty-review, trusty-analyze or
        // trusty-memory row. None serves a TCP port any more (ADR-0032) — see
        // [`uds_socket_for`]. Leaving 7891, 7879 or 7070 here would dial
        // whatever happens to be on that port and report it as the daemon.
        _ => None,
    }
}

/// The Unix socket a member serves, for the daemons that no longer serve TCP.
///
/// Why (#6277, ADR-0032): trusty-review was the first member to move its own
/// transport onto UDS; trusty-analyze followed in #6287 and trusty-memory in
/// #6286. A probe has to move
/// with its daemon IN THE SAME CHANGE, or `tctl` dials a dead 7891 / 7879,
/// reads `Refused`, and — because `Refused` is one of the two variants
/// [`ProbeOutcome::is_confirmed_down`] accepts — kickstarts a daemon that is
/// running perfectly. That is #4246 exactly, which is why the design review
/// made each daemon swap and its consumers one PR.
///
/// What: the path from `trusty_common::daemon_socket_path`, the same call the
/// daemon binds through, for members that serve UDS; `None` for every member
/// still on HTTP. A member that has RETIRED HTTP is probed ONLY over the
/// socket — there is no second leg to reconcile, because there is no second
/// transport, and no discovery file that could disagree with a derived path.
/// [`dual_transport`] is the exception, and states its own case.
///
/// ADR-0035's console-side aggregator routing is deliberately NOT done here:
/// its own open questions are unresolved, so this is a transport swap and
/// nothing more.
///
/// Test: `tests::uds_members_have_no_fixed_port`,
/// `tests::probe_uds_reads_the_health_envelope_off_a_result_frame`,
/// `tests::uds_socket_for_matches_the_shared_entry_point`.
/// Whether a member is started on demand rather than kept resident (#6350).
///
/// Why this exists as its own predicate: `uds_socket_for` answers "which
/// transport", and this answers "who owns the lifetime" — two different
/// questions that happen to have overlapping answers today. A member listed
/// here has no launchd unit, so a socket that is not answering means nothing is
/// using it, not that anything is broken.
///
/// Kept as a literal for the reason `uds_health_method` records: `tctl` has no
/// Cargo edge on any daemon, and adding one to share a `&str` would pull an
/// analysis engine into its build.
///
/// Test: `tests::on_demand_members_are_a_subset_of_the_uds_members`.
pub fn on_demand_member(binary: &str) -> bool {
    binary == trusty_common::uds::ANALYZE_SERVICE
}

pub fn uds_socket_for(binary: &str) -> Option<std::path::PathBuf> {
    match binary {
        // #6290: NO trusty-review row. It has no daemon and no socket — see
        // [`presence_only`].
        // #6285: trusty-search binds this same path from
        // `trusty_search::service::socket::socket_path`, which is
        // `daemon_socket_path` under another name. It is the one member here
        // that still serves HTTP as well — see [`dual_transport`].
        "trusty-analyze" | "trusty-memory" | "trusty-search" => {
            trusty_common::daemon_socket_path(binary).ok()
        }
        _ => None,
    }
}

/// Whether a member answers on a socket AND on HTTP, so both legs must be read.
///
/// Why (#6285): every earlier migration deleted the daemon's axum surface in
/// the PR that added its socket, so "has a socket" and "has no port" were the
/// same fact. trusty-search breaks that pairing: its listener went up beside
/// the axum server in #6367 and the HTTP surface is deleted several slices
/// later. Probing only the socket in that window reads every INSTALLED
/// trusty-search older than the listener as `Refused` — no published version
/// binds one — and `Refused` is one of the two variants
/// [`ProbeOutcome::is_confirmed_down`] accepts, so `verify_tail` would
/// `launchctl kickstart -k` a healthy daemon on every `tctl install`. That is
/// #4246, and on this member it lands as a SIGKILL mid-index-flush.
///
/// Probing only HTTP would be safe today and wrong tomorrow: it is the surface
/// being retired, and the row that carries it disappears with it.
///
/// What: `true` for `trusty-search` only. Such a member's socket answer and its
/// HTTP legs are reconciled through [`reconcile`], which already ranks an
/// answer above a refusal — so whichever transport the installed binary serves
/// is the one that decides the verdict. Deleting this arm is the LAST step of
/// the retire program, once no daemon on the axum surface can still be
/// installed; deleting it early re-ships #4246.
///
/// Test: `tests::search_is_probed_over_both_transports`,
/// `tests::a_pre_socket_search_is_healthy_over_http_alone`,
/// `tests::dual_transport_members_keep_a_fixed_port`.
pub fn dual_transport(binary: &str) -> bool {
    binary == "trusty-search"
}

/// Whether this member's health is a PRESENCE question, not a liveness one.
///
/// Why (#6290): trusty-review runs per invocation. There is no process to be up
/// or down, so every liveness answer a probe could give is wrong — `Refused` is
/// the one it would give, and `Refused` is one of the two variants
/// [`ProbeOutcome::is_confirmed_down`] accepts, which arms `launchctl kickstart
/// -k` against a launchd label that no longer exists. The honest question for a
/// per-invocation tool is whether the binary is installed and runnable, and
/// [`probe_presence`] is what asks it.
///
/// What: `true` for `trusty-review`; `false` for everything else. Kept as its
/// own predicate rather than folded into `probe_daemon_http`'s body so the
/// "this member is not a daemon" claim is one testable place.
///
/// Test: `tests::review_is_probed_by_presence_not_by_dialling`.
pub(super) fn presence_only(binary: &str) -> bool {
    binary == "trusty-review"
}

/// Probe a per-invocation member by running `<binary> --version`.
///
/// Why: presence alone would report a binary that cannot execute — a broken
/// signature, a truncated download, a missing dylib — as healthy, and on macOS
/// the cdhash hazard in CLAUDE.md makes "on PATH but SIGKILLed on exec" a real
/// state rather than a hypothetical one. Running the binary and reading the
/// version it prints is the cheapest check that distinguishes the two, and it
/// yields the version the status rollup renders anyway.
///
/// What: spawns `<binary> --version` under [`PRESENCE_TIMEOUT`] and returns
/// [`ProbeOutcome::Serving`] carrying the parsed version. A binary that cannot
/// be spawned is [`ProbeOutcome::NotInstalled`]; one that runs and fails is
/// [`ProbeOutcome::ProbeFailed`] — NOT `Refused` or `Timeout`, because neither
/// is true and both would arm the kickstart this function exists to keep
/// disarmed.
///
/// Test: `tests::review_is_probed_by_presence_not_by_dialling`,
/// `tests::presence_probe_of_an_absent_binary_is_not_installed`.
fn probe_presence(binary: &str) -> ProbeOutcome {
    let Some(path) = super::probe::resolve_binary_path(binary) else {
        return ProbeOutcome::NotInstalled;
    };
    let output = std::process::Command::new(&path).arg("--version").output();
    match output {
        Ok(out) if out.status.success() => ProbeOutcome::Serving {
            status: "ok".to_owned(),
            version: parse_version_line(&String::from_utf8_lossy(&out.stdout)),
        },
        Ok(out) => ProbeOutcome::ProbeFailed {
            detail: format!(
                "`{} --version` exited {}: {}",
                path.display(),
                out.status,
                sample(&out.stderr)
            ),
        },
        Err(e) => ProbeOutcome::ProbeFailed {
            detail: format!("spawn `{} --version`: {e}", path.display()),
        },
    }
}

/// Pull the version out of a `clap` `--version` line.
///
/// What: `"trusty-review 0.24.1"` → `Some("0.24.1")`. A line with no
/// whitespace-separated second token yields `None` rather than the whole line,
/// so a status card never renders a binary name where a version belongs.
/// Test: `tests::version_line_parses_the_clap_shape`.
fn parse_version_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)
        .map(str::to_owned)
}

/// The method a UDS member answers a health probe on.
///
/// Why this is a per-binary function and not one constant (#6287): it WAS one
/// constant, `"review.health"`, because trusty-review was the only UDS member.
/// Each daemon names its health method under its own `<domain>.` prefix, so a
/// second member made the single constant wrong for one of them — and the way
/// that wrongness surfaces is `method_not_found`, which
/// [`classify_rpc_response`] reads as a daemon answering badly rather than as a
/// probe asking wrongly.
///
/// Duplicated as literals rather than imported: `trusty-installer` has no Cargo
/// edge on either daemon, and adding one to share a `&str` would pull an
/// LLM-pipeline crate and an analysis engine into `tctl`'s build.
/// `trusty_analyze::service::METHOD_HEALTH`,
/// `trusty_memory::transport::uds::METHOD_HEALTH` and
/// `trusty_search::service::socket::METHOD_HEALTH` are the definitions; each
/// daemon crate's `uds_consumer_contract` test is what keeps them equal.
///
/// # Panics
///
/// Never for a binary [`uds_socket_for`] returned a socket for — the two match
/// arms are kept in step by `tests::every_uds_member_has_a_health_method`.
fn uds_health_method(binary: &str) -> Option<&'static str> {
    match binary {
        // #6290: NO trusty-review row. `review.health` is gone with the daemon
        // that answered it; the member is probed by presence instead.
        "trusty-analyze" => Some("analyze.health"),
        // #6286: `trusty_memory::transport::uds::METHOD_HEALTH`. It is the only
        // folded method `tctl` needs, and the one trusty-console dials too.
        "trusty-memory" => Some("memory.health"),
        // #6285: `trusty_search::service::socket::METHOD_HEALTH`. It reads the
        // same `health_report()` the axum route does, so the two legs of a
        // dual-transport probe cannot answer differently about the same daemon.
        "trusty-search" => Some("search.health"),
        _ => None,
    }
}

/// The typed result of probing one member's health (#4246).
///
/// Why: the pre-#4246 probe returned a bare `String` in which clap's exit 2, a
/// connection refusal, a hung daemon and a schema mismatch were all literally
/// the same value — `"down"`. `verify_tail` then fired `launchctl kickstart -k`
/// on that, so an *unimplemented CLI contract* was indistinguishable from a
/// *dead process* and healthy daemons got hard-restarted. Keeping the cause
/// makes the destructive repair gateable on a genuine TCP-level observation
/// ([`Self::is_confirmed_down`]) instead of on "something went wrong".
/// What: one variant per distinguishable cause. Two derived views keep the
/// existing call sites honest: [`Self::health_string`] is the flat display
/// vocabulary `status`/`stack` render, and [`Self::member_health`] is the
/// orchestration vocabulary `tctl up`'s `ensure_member` acts on.
/// Test: `tests::probe_distinguishes_failure_causes`,
/// `tests::health_string_maps_every_variant`,
/// `tests::only_transport_failures_are_confirmed_down`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The binary is on neither `PATH` nor the default install dir (#3876).
    NotInstalled,
    /// Health is not probeable through the standard contract for this member's
    /// lifecycle strategy (trusty-mpm — see `super::probe::probe_member_health`).
    Unprobeable,
    /// Neither an `http_addr` discovery file nor a documented default port
    /// resolved, so there was nothing to probe.
    NoAddress,
    /// TCP connect was refused — nothing is listening. A genuine "down".
    Refused,
    /// The connect or the request exceeded its bound — a wedged daemon. Also a
    /// genuine "down" for repair purposes.
    Timeout,
    /// A non-2xx response whose body was not a usable trusty health envelope.
    /// (A non-2xx WITH a usable envelope is [`Self::Serving`] — trusty-analyze
    /// answers 503 + `status:"degraded"` by design.)
    HttpError {
        /// The HTTP status code received.
        status: u16,
    },
    /// Something answered, but the body is not a trusty health envelope (no
    /// string `status` field). A squatter, a proxy error page, or a schema
    /// change — never a reason to restart anything.
    BadEnvelope {
        /// A bounded sample of what was received, for diagnosis.
        got: String,
    },
    /// A UDS member answered with a JSON-RPC error frame rather than a result
    /// (#6277).
    ///
    /// Why its own variant: there is no HTTP status code on a socket, so
    /// [`Self::HttpError`] has nothing to carry, and folding this into
    /// [`Self::BadEnvelope`] would lose the coded reason — a method name the
    /// client has drifted off (`-32601`) reads completely differently from a
    /// handler that failed (`-32603`), and only the code says which. Like
    /// `HttpError` it is NOT confirmed-down: the daemon accepted the
    /// connection, read the frame, and chose to refuse, which is the strongest
    /// possible evidence that it is alive.
    RpcError {
        /// The JSON-RPC error code.
        code: i64,
        /// The daemon's own message, bounded for display.
        message: String,
    },
    /// The probe could not be performed locally (HTTP client or async runtime
    /// could not be constructed). Says nothing about the daemon.
    ProbeFailed {
        /// The local failure detail.
        detail: String,
    },
    /// A trusty health envelope was received and understood.
    Serving {
        /// The EFFECTIVE status (see [`effective_status`] — a `warming` daemon
        /// reporting `status:"ok"` is reported `degraded`, not fully serving).
        status: String,
        /// The daemon's self-reported version, when the payload carries one
        /// (trusty-mpm's does not).
        version: Option<String>,
    },
}

impl ProbeOutcome {
    /// Map to the flat health-string vocabulary `status` / `stack` / the verify
    /// tail render.
    ///
    /// Why: six call sites already render a one-word health per member and
    /// serialise it into `--json` contracts; this shim keeps that output stable
    /// while the transport underneath changes.
    /// What: `Serving` routes through the shared [`classify_status`] vocabulary;
    /// `NotInstalled`/`Unprobeable` keep their own words; **every** remaining
    /// variant is `down`.
    ///
    /// `BadEnvelope` deliberately maps to `down`, NOT `unknown`: `unknown` is
    /// tolerated by `VerifyTailReport::build` and by `status`'s exit code, so
    /// mapping it there would print `VERIFIED` / exit 0 for a stack about which
    /// we hold no actual health information. `down` is the honest verdict — and
    /// it is now safe to say, because the kickstart is gated on
    /// [`Self::is_confirmed_down`], not on this string.
    ///
    /// # Invariant — this string is for DISPLAY ONLY
    /// `health_string() == "down"` does **NOT** imply
    /// [`Self::is_confirmed_down`], and that gap is deliberate rather than an
    /// oversight. `NoAddress`, `HttpError`, `BadEnvelope`, `RpcError` and
    /// `ProbeFailed` all
    /// render `down` while being explicitly NOT confirmed-down: each means
    /// "something answered, or we never looked", which is not evidence that a
    /// process is dead. The two views answer different questions — this one asks
    /// *what should we report*, [`Self::is_confirmed_down`] asks *may we destroy
    /// state*.
    ///
    /// So this string MUST NOT be used to authorise a destructive action
    /// (`launchctl kickstart -k`, a restart, a teardown). Gating on
    /// `health_string() == "down"` would re-merge exactly the causes
    /// [`ProbeOutcome`] exists to keep apart, restoring the #4246 harm: a
    /// `NoAddress` member (address never resolved) or a squatter's
    /// `BadEnvelope` would once again hard-restart a daemon nobody has shown to
    /// be down. Authorise on the VARIANT, via [`Self::is_confirmed_down`].
    /// Test: `tests::health_string_maps_every_variant`,
    /// `tests::down_health_string_is_not_a_kickstart_licence`.
    pub fn health_string(&self) -> &'static str {
        use super::probe::health_str;
        match self {
            Self::NotInstalled => health_str::NOT_INSTALLED,
            Self::Unprobeable => health_str::UNKNOWN,
            Self::Serving { status, .. } => super::probe::health_string(classify_status(status)),
            Self::NoAddress
            | Self::Refused
            | Self::Timeout
            | Self::HttpError { .. }
            | Self::BadEnvelope { .. }
            | Self::RpcError { .. }
            | Self::ProbeFailed { .. } => health_str::DOWN,
        }
    }

    /// Map to the `tctl up` orchestration vocabulary.
    ///
    /// Why: `ensure_member` (DOC-12 §3.4) branches on `MemberHealth`, not on a
    /// string. `Unprobeable` maps to `Down` here — NOT to a health verdict — so a
    /// member whose health could not be established falls through to `start`,
    /// which is idempotent. `ensure_member` treats `Down` and `HealthyStale`
    /// identically, so nothing in `up`'s action set turns on that choice.
    /// (#4925: trusty-mpm is no longer the example. It is probed over HTTP now,
    /// so it reaches a real `Serving`/`Refused` verdict and only a NON-daemon
    /// still arrives here as `Unprobeable`.)
    /// What: `Serving` via [`classify_status`]; `NotInstalled` preserved; every
    /// other variant `Down`.
    /// Test: `tests::member_health_maps_every_variant`.
    pub fn member_health(&self) -> MemberHealth {
        match self {
            Self::NotInstalled => MemberHealth::NotInstalled,
            Self::Serving { status, .. } => classify_status(status),
            _ => MemberHealth::Down,
        }
    }

    /// Whether this outcome is a CONFIRMED-down observation — the only thing
    /// that may authorise a destructive repair (#4246).
    ///
    /// Why: this is the gate that stops `tctl install` hard-restarting healthy
    /// daemons. `launchctl kickstart -k` SIGTERMs and SIGKILLs at the
    /// `ExitTimeOut` boundary; before #4393 the shared plist renderer emitted no
    /// such key, so launchd applied its measured 5s default while
    /// trusty-search's graceful index flush floors at 30s per index. A restart
    /// triggered by a schema mismatch or a squatter on a default port therefore
    /// cost unflushed HNSW vectors. Only an observation
    /// at the TRANSPORT layer — nothing accepted the connection, or nothing
    /// answered in time — is evidence a process is actually not serving. Every
    /// other failure means *something* answered, so restarting is at best a
    /// guess.
    ///
    /// This was unobservable before the transport change: a subprocess probe
    /// cannot produce a `Refused` (that is a TCP concept), and a fast clap error
    /// never hits the timeout — which is exactly why the gate and the transport
    /// had to land together rather than in sequence.
    /// What: `true` iff `Refused` or `Timeout`.
    ///
    /// # Invariant — this is NARROWER than [`Self::health_string`]`() == "down"`
    /// The two are deliberately NOT equivalent, and callers must not treat them
    /// as interchangeable. Five variants — `NoAddress`, `HttpError`,
    /// `BadEnvelope`, `RpcError`, `ProbeFailed` — report `down` for display
    /// while returning `false` here. `NoAddress` is the sharpest example: it reports `down`
    /// on purpose (mapping it to `unknown` would make
    /// `VerifyTailReport::build` and `status`'s exit code print `VERIFIED` for a
    /// member whose address was never resolved), yet it is emphatically not
    /// grounds for a restart, because nothing was ever observed.
    ///
    /// This asymmetry is the whole safety mechanism, so it is pinned by a test
    /// rather than left to be rediscovered: any future caller reaching for the
    /// display string to decide whether to repair something is reintroducing
    /// #4246 and must use this predicate instead.
    /// Test: `tests::only_transport_failures_are_confirmed_down`,
    /// `tests::down_health_string_is_not_a_kickstart_licence`,
    /// `verify_tail::tests::verify_one_does_not_kickstart_a_healthy_launchd_daemon`.
    pub fn is_confirmed_down(&self) -> bool {
        matches!(self, Self::Refused | Self::Timeout)
    }
}

/// Build the HTTP client used for every health probe.
///
/// Why: **`.no_proxy()` is load-bearing, not hygiene.** reqwest 0.12 routes
/// `127.0.0.1` through `HTTP_PROXY`/`http_proxy`/`ALL_PROXY` when they are
/// exported — hyper-util's proxy matcher (`client/proxy/matcher.rs`) has no
/// runtime loopback exemption, and the only bypass is an explicit `NO_PROXY`
/// entry the user is not required to have. Without this call a developer with a
/// proxy exported gets every daemon reported `down` through the new transport,
/// and (pre-gate) every healthy daemon kickstarted — the exact #4246 signature,
/// reintroduced. #4392 moved the `.no_proxy()` call to
/// `trusty_common::http_client::loopback_client_builder` so every loopback
/// caller in the workspace inherits it; this module keeps its own regression
/// test because the property is load-bearing HERE regardless of where the call
/// lives. The client is built per probe rather than cached because reqwest reads
/// the proxy environment at BUILD time; a cached client would make the behaviour
/// depend on process start order.
/// What: a client with proxies disabled and both a connect and a whole-request
/// bound.
/// Test: `tests::probe_ignores_http_proxy_env` — sets `HTTP_PROXY` to a dead
/// address and asserts the stub is still reached, AND that a client built
/// *without* `.no_proxy()` fails under the same environment.
pub fn build_probe_client() -> reqwest::Result<reqwest::Client> {
    build_probe_client_with(CONNECT_TIMEOUT, REQUEST_TIMEOUT)
}

/// [`build_probe_client`] with caller-chosen bounds.
///
/// Why: the `Timeout` arm of the failure taxonomy has to be provable, and a test
/// that waits out the production [`REQUEST_TIMEOUT`] would add 5 seconds to
/// every run. Parameterising the bounds keeps the production defaults in ONE
/// place ([`build_probe_client`]) while letting the timeout test drive much
/// shorter ones against a deliberately silent peer. The caller is expected to
/// pass `connect` ≪ `request`, so that a silent peer's verdict is reached on the
/// READ path rather than on the handshake.
/// What: same client as [`build_probe_client`] — proxies stay disabled, since
/// the proxy test needs both bounds AND the flag — with `connect`/`request`
/// substituted.
/// Test: `tests::probe_distinguishes_failure_causes`.
pub fn build_probe_client_with(
    connect: Duration,
    request: Duration,
) -> reqwest::Result<reqwest::Client> {
    // #4392: `.no_proxy()` now lives at the shared entry point, so this module's
    // property and every other loopback caller's cannot drift apart.
    trusty_common::http_client::loopback_client_builder()
        .connect_timeout(connect)
        .timeout(request)
        .build()
}

/// Map a reqwest transport error to the outcome that names its cause.
///
/// Why: the whole point of #4246's taxonomy is that "refused" and "answered with
/// garbage" must not be the same value — only the former may authorise a
/// kickstart.
/// What: a timeout (connect or request) → `Timeout`; a connect failure →
/// `Refused`; anything else happened AFTER a connection was established (body
/// decode, malformed response) so it is `BadEnvelope`, which never kickstarts.
/// Test: `tests::probe_distinguishes_failure_causes`.
fn classify_transport_error(e: &reqwest::Error) -> ProbeOutcome {
    if e.is_timeout() {
        ProbeOutcome::Timeout
    } else if e.is_connect() {
        ProbeOutcome::Refused
    } else {
        ProbeOutcome::BadEnvelope {
            got: sample(e.to_string().as_bytes()),
        }
    }
}

/// Bound an arbitrary byte slice into a short, printable diagnosis sample.
fn sample(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.chars().count() <= ENVELOPE_SAMPLE_LEN {
        return trimmed.to_owned();
    }
    trimmed.chars().take(ENVELOPE_SAMPLE_LEN).collect()
}

/// Derive the EFFECTIVE status from a parsed health envelope.
///
/// Why: `/health` returning 200 `{"status":"ok"}` does not mean the daemon can
/// serve. trusty-memory reports `status:"ok"` with `daemon_state:"warming"`
/// while its embedder is still initialising, and trusty-search reports
/// `status:"ok"` with `embedder:"initializing"`/`"stalled"`/`"error"` — both say
/// so out loud in their handlers precisely so a probe can read it. Calling such
/// a daemon fully healthy hides a real capability gap; calling it `down` would
/// be worse (it IS up, and a kickstart would only restart the warm-up).
/// `degraded` is the honest middle, and it maps to `stale`, which
/// `VerifyTailReport::build` already tolerates.
/// What: returns the raw `status` unchanged unless it is a HEALTHY word AND a
/// readiness sub-field says otherwise, in which case `"degraded"`. Readiness
/// sub-fields read: `daemon_state` (trusty-memory) and `embedder` /
/// `embedder_status` (trusty-search — the wire field is `embedder`;
/// `embedder_status` is accepted too so a rename cannot silently defeat this).
/// A daemon that carries none of them, or whose sub-field is not a string, is
/// unaffected.
/// Test: `tests::warming_daemon_is_degraded_not_serving`,
/// `tests::real_daemon_payloads_are_never_down`.
pub fn effective_status(v: &serde_json::Value) -> String {
    let raw = v.get("status").and_then(|s| s.as_str()).unwrap_or_default();
    if classify_status(raw) != MemberHealth::HealthyVersionOk {
        // Already degraded/down by its own account — never upgrade a verdict.
        return raw.to_owned();
    }
    for key in ["daemon_state", "embedder", "embedder_status"] {
        let Some(state) = v.get(key).and_then(|x| x.as_str()) else {
            continue;
        };
        if !matches!(state.to_ascii_lowercase().as_str(), "ready" | "ok") {
            return DEGRADED.to_owned();
        }
    }
    raw.to_owned()
}

/// Classify one HTTP response into a [`ProbeOutcome`] — on the BODY, not the
/// status code.
///
/// Why: the two daemons that compute real dependency status disagree about HTTP
/// semantics. trusty-analyze answers **503** + `status:"degraded"` when
/// trusty-search is unreachable; trusty-review answers **200** + the same
/// `status:"degraded"`. A 2xx-only liveness check (`ensure::daemon::health_ok`)
/// therefore reads one healthy daemon as `degraded` and the other as `down` for
/// the identical condition — and pre-gate, would have kickstarted analyze every
/// time search was slow. The body is the signal; the status code is not.
///
/// The envelope check (a string `status` field) is what stops a stale
/// `http_addr` plus an unrelated squatter on a default loopback port reading as
/// healthy. It is not a complete defence — a squatter emitting a generic
/// `{"status":"ok"}` still passes, the #3364 collision class. Closing that needs
/// a `service` discriminator in each daemon's payload, which is deliberately out
/// of scope here and tracked separately.
///
/// What: parses `body` as JSON. With a string `status` field → `Serving` (status
/// via [`effective_status`], `version` when present) REGARDLESS of
/// `status_code`. Without one → `HttpError { status }` if the code was non-2xx
/// (the code is then the most informative thing we have), else
/// `BadEnvelope { got }`.
/// Test: `tests::probe_accepts_503_degraded`,
/// `tests::probe_rejects_non_trusty_envelope`,
/// `tests::classify_response_prefers_status_code_for_unusable_5xx`.
pub fn classify_response(status_code: u16, body: &[u8]) -> ProbeOutcome {
    let parsed = serde_json::from_slice::<serde_json::Value>(body).ok();
    let envelope = parsed
        .as_ref()
        .filter(|v| v.get("status").and_then(|s| s.as_str()).is_some());
    match envelope {
        Some(v) => ProbeOutcome::Serving {
            status: effective_status(v),
            version: v.get("version").and_then(|x| x.as_str()).map(str::to_owned),
        },
        None if !(200..300).contains(&status_code) => ProbeOutcome::HttpError {
            status: status_code,
        },
        None => ProbeOutcome::BadEnvelope { got: sample(body) },
    }
}

/// Combine the outcomes of the dual-resolution legs into ONE verdict — the
/// precedence rule (#4246).
///
/// Why: this is the rule that stops the fix re-shipping the bug. The two legs
/// are EXPECTED to disagree in normal operation, not just in edge cases:
/// trusty-memory port-walks `7070..=7079`, so a daemon that found 7070 taken and
/// bound 7071 has a `Serving` `http_addr` leg and a `Refused` fixed-port leg
/// while being perfectly healthy. Treating the refusal as authoritative would
/// hard-restart it — precisely the harm this issue exists to remove.
/// Symmetrically, trusty-search no-ops the shared `http_addr` write under
/// `TRUSTY_DATA_DIR`, so an isolated daemon has only the fixed-port leg.
///
/// # Postconditions
/// - If ANY leg is `Serving`, the result is a `Serving` — so another leg's
///   `Refused`/`Timeout` can NEVER reach [`ProbeOutcome::is_confirmed_down`] and
///   therefore never feed `needs_kickstart`.
/// - If any leg got an ANSWER that was not a usable envelope
///   (`BadEnvelope`/`HttpError`), that outranks a refusal on the other leg:
///   something is listening, so restarting is a guess, not a repair.
/// - `Refused`/`Timeout` win only when NO leg saw anything at all.
/// - An empty leg set is `NoAddress`.
///
/// What: picks the minimum-rank outcome — healthy `Serving` (0) < degraded
/// `Serving` (1) < `BadEnvelope` (2) < `HttpError` / `RpcError` (3) <
/// `Timeout` (4) < `Refused` (5) < everything else (6).
/// Test: `tests::reconcile_serving_leg_beats_refused_leg`,
/// `tests::reconcile_answered_leg_beats_refused_leg`,
/// `tests::reconcile_all_refused_stays_refused`,
/// `tests::probe_port_walked_daemon_is_healthy`.
pub fn reconcile(outcomes: Vec<ProbeOutcome>) -> ProbeOutcome {
    fn rank(o: &ProbeOutcome) -> u8 {
        match o {
            ProbeOutcome::Serving { status, .. } => {
                if classify_status(status) == MemberHealth::HealthyVersionOk {
                    0
                } else {
                    1
                }
            }
            ProbeOutcome::BadEnvelope { .. } => 2,
            // #6277: an RPC error frame ranks with `HttpError` — both mean a
            // daemon accepted the connection and refused with a reason, which
            // outranks any refusal on another leg.
            ProbeOutcome::HttpError { .. } | ProbeOutcome::RpcError { .. } => 3,
            ProbeOutcome::Timeout => 4,
            ProbeOutcome::Refused => 5,
            _ => 6,
        }
    }
    outcomes
        .into_iter()
        .min_by_key(rank)
        .unwrap_or(ProbeOutcome::NoAddress)
}

/// Issue `GET {base}/health` against one resolved base URL.
///
/// Why: one leg of [`probe_bases`]' dual resolution.
/// What: sends the request, then classifies the transport error, or the status +
/// body via [`classify_response`].
/// Test: exercised by every stub-server test in `tests`.
async fn probe_url(client: &reqwest::Client, base: &str) -> ProbeOutcome {
    let url = format!("{base}{HEALTH_PATH}");
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => return classify_transport_error(&e),
    };
    let status_code = resp.status().as_u16();
    match resp.bytes().await {
        Ok(body) => classify_response(status_code, &body),
        Err(e) => classify_transport_error(&e),
    }
}

/// Classify one JSON-RPC response frame into a [`ProbeOutcome`] (#6277).
///
/// Why: this is [`classify_response`]'s counterpart for the UDS transport, and
/// it reuses the same envelope rule deliberately — a health body is a health
/// body regardless of what carried it, so [`effective_status`]'s
/// warming/degraded downgrade and the `status`-field check that keeps a squatter
/// out apply unchanged. What it cannot reuse is the status code, because a
/// socket has none: the `error` half of a JSON-RPC frame is the analogue, and it
/// gets its own [`ProbeOutcome::RpcError`] arm rather than being flattened into
/// `HttpError { status: 0 }`.
///
/// What: a `result` carrying a string `status` → `Serving`. A `result` without
/// one → `BadEnvelope` (something answered on this socket that is not the
/// daemon we asked for). An `error` → `RpcError` with the code and a bounded
/// message.
///
/// **Neither present, or BOTH present → `BadEnvelope`.** JSON-RPC 2.0 §5 says a
/// response carries exactly one of the two, so a frame with both is malformed
/// and its sender is not a peer this probe understands. Taking the `error`
/// branch on a both-present frame — which an earlier version of this function
/// did silently — reports a specific coded reason read off a frame whose shape
/// already proves the sender is not speaking the protocol. `BadEnvelope` is the
/// honest verdict, and it keeps the same never-confirmed-down safety.
///
/// Test: `tests::probe_uds_reads_the_health_envelope_off_a_result_frame`,
/// `tests::classify_rpc_response_reports_an_error_frame_as_answered_not_down`,
/// `tests::classify_rpc_response_rejects_a_non_trusty_result`,
/// `tests::classify_rpc_response_rejects_a_frame_carrying_both_result_and_error`.
pub fn classify_rpc_response(frame: &serde_json::Value) -> ProbeOutcome {
    let has_result = frame.get("result").is_some_and(|r| !r.is_null());
    let has_error = frame.get("error").is_some_and(|e| !e.is_null());
    if has_result && has_error {
        return ProbeOutcome::BadEnvelope {
            got: sample(frame.to_string().as_bytes()),
        };
    }

    if let Some(error) = frame.get("error").filter(|e| !e.is_null()) {
        return ProbeOutcome::RpcError {
            code: error
                .get("code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
            message: sample(
                error
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("(no message)")
                    .as_bytes(),
            ),
        };
    }

    let Some(result) = frame.get("result").filter(|r| !r.is_null()) else {
        return ProbeOutcome::BadEnvelope {
            got: sample(frame.to_string().as_bytes()),
        };
    };
    if result.get("status").and_then(|s| s.as_str()).is_none() {
        return ProbeOutcome::BadEnvelope {
            got: sample(result.to_string().as_bytes()),
        };
    }
    ProbeOutcome::Serving {
        status: effective_status(result),
        version: result
            .get("version")
            .and_then(|x| x.as_str())
            .map(str::to_owned),
    }
}

/// Dial `socket` and classify one health exchange (#6277).
///
/// Why: the UDS counterpart of [`probe_url`]. The transport failures map onto
/// the SAME taxonomy the HTTP leg uses, because `verify_tail`'s kickstart gate
/// reads the variant and must not care which transport produced it: a dial the
/// kernel refused (no listener, or no socket file) is `Refused`, an elapsed
/// budget is `Timeout`, and everything else means something answered and is
/// therefore never confirmed-down.
///
/// A peer that accepts and hangs up without a frame arrives as
/// `UdsRpcError::NoResponse` and is classified `BadEnvelope`, not `Refused`. It
/// accepted the connection, so a process is alive on that socket and restarting
/// it would be a guess.
///
/// What: one `send_framed_request` for `method` at [`REQUEST_TIMEOUT`], then
/// [`classify_rpc_response`].
///
/// #6630: the frame carries an explicit `"params": {}`, never an absent
/// `params` field. `RpcRouter::typed` decodes a missing `params` as
/// `Value::Null`, and trusty-memory's `memory.health` binds `HealthQuery` — a
/// plain derived `Deserialize` that refuses `null` however many of its fields
/// carry `#[serde(default)]` (that attribute governs a MISSING key inside an
/// object, not a `null` in place of the object). `analyze.health` and
/// `search.health` bind `NoParams`, whose hand-written `Deserialize` calls
/// `IgnoredAny` and accepts anything including `null` or `{}` — so this was
/// invisible for both and reached only trusty-memory, exactly as
/// `trusty-console`'s `detect::memory::probe_health` documents at its own
/// `#6356` fix for the identical mismatch. Sending `{}` here fixed it there;
/// it fixes the same defect in this crate's independent copy of the request.
///
/// Test: `tests::probe_uds_reads_the_health_envelope_off_a_result_frame`,
/// `tests::probe_uds_reports_refused_for_an_absent_socket`,
/// `tests::probe_uds_sends_explicit_empty_params_so_a_strict_handler_answers`.
async fn probe_socket(socket: &std::path::Path, method: &str) -> ProbeOutcome {
    // #6555: a params-less frame decodes to null, which a struct-bound method rejects with -32602.
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": {},
    });
    match trusty_common::uds::send_framed_request::<_, serde_json::Value>(
        socket,
        &request,
        REQUEST_TIMEOUT,
    )
    .await
    {
        Ok(frame) => classify_rpc_response(&frame),
        Err(trusty_common::uds::UdsRpcError::Timeout { .. }) => ProbeOutcome::Timeout,
        Err(trusty_common::uds::UdsRpcError::Dial { .. }) => ProbeOutcome::Refused,
        Err(e) => ProbeOutcome::BadEnvelope {
            got: sample(e.to_string().as_bytes()),
        },
    }
}

/// Resolve a daemon's base URL BOTH ways: recorded `http_addr` first, documented
/// default port second.
///
/// Why: neither path is individually sufficient. `http_addr` is the primary — it
/// survives `--port` overrides and auto-port-walking — but trusty-search
/// deliberately skips writing it under `TRUSTY_DATA_DIR`, and an uncleanly-exited
/// daemon can leave a stale one. The documented default covers those, but misses
/// every port-walked daemon.
/// What: returns `(recorded, fixed)`; `recorded` is
/// `http://<trusty_common::read_daemon_addr(app)>` when the file exists and is
/// non-empty, `fixed` is `http://127.0.0.1:<fixed_port_for(binary)>`. Either may
/// be `None`. A read error is treated as "no recorded address" — it is not
/// evidence about the daemon.
/// Test: `tests::resolve_probe_bases_reads_http_addr`,
/// `tests::probe_no_address_when_nothing_resolves`.
pub fn resolve_probe_bases(app: &str, binary: &str) -> (Option<String>, Option<String>) {
    let recorded = trusty_common::read_daemon_addr(app)
        .ok()
        .flatten()
        .map(|a| a.trim().to_owned())
        .filter(|a| !a.is_empty())
        .map(|a| format!("http://{a}"));
    let fixed = fixed_port_for(binary).map(|p| format!("http://127.0.0.1:{p}"));
    (recorded, fixed)
}

/// Probe both resolved legs concurrently and apply [`reconcile`]'s precedence
/// rule.
///
/// Why: kept separate from [`resolve_probe_bases`] so the precedence rule can be
/// exercised over the REAL HTTP transport with both legs staged independently —
/// a serving stub on one leg and a genuinely-refusing address on the other. That
/// combination is the port-walk case, and it is the one that must never
/// kickstart; testing it through `probe_daemon_http` alone would require a
/// developer's live daemon to be absent from its documented port.
/// What: dedupes when both legs resolve to the same address (the common case),
/// otherwise runs them concurrently on the caller's runtime. `(None, None)` →
/// [`ProbeOutcome::NoAddress`].
/// Test: `tests::probe_port_walked_daemon_is_healthy`,
/// `tests::probe_accepts_503_degraded`, `tests::probe_distinguishes_failure_causes`.
pub async fn probe_bases(
    client: &reqwest::Client,
    recorded: Option<String>,
    fixed: Option<String>,
) -> ProbeOutcome {
    let outcomes = match (recorded, fixed) {
        (Some(a), Some(b)) if a == b => vec![probe_url(client, &a).await],
        (Some(a), Some(b)) => {
            let (x, y) = tokio::join!(probe_url(client, &a), probe_url(client, &b));
            vec![x, y]
        }
        (Some(only), None) | (None, Some(only)) => vec![probe_url(client, &only).await],
        (None, None) => return ProbeOutcome::NoAddress,
    };
    reconcile(outcomes)
}

/// Resolve a daemon's address both ways and probe it (#4246).
///
/// Why: the async entry point — [`resolve_probe_bases`] plus [`probe_bases`]
/// plus the one client build, in the order a caller wants them.
/// What: builds the probe client (a build failure is
/// [`ProbeOutcome::ProbeFailed`] — a LOCAL failure, never a daemon verdict, and
/// so never a reason to kickstart), resolves both legs for `app`/`binary`, and
/// probes them.
/// #6277: a member with a Unix socket ([`uds_socket_for`]) is probed over it
/// INSTEAD, and the HTTP legs are not attempted at all. There is nothing to
/// reconcile: a retired-HTTP member has one transport, one derived path, and no
/// discovery file, so a second leg could only contribute a false refusal.
/// #6285: a [`dual_transport`] member is the one exception — its socket leg and
/// its HTTP legs run concurrently and go through [`reconcile`], because during
/// the retire window either transport may be the only one its installed binary
/// serves.
///
/// Test: `tests::probe_uses_http_addr_when_fixed_port_unknown`,
/// `tests::probe_no_address_when_nothing_resolves`,
/// `tests::probe_uds_reads_the_health_envelope_off_a_result_frame`,
/// `tests::search_is_probed_over_both_transports`,
/// `tests::a_pre_socket_search_is_healthy_over_http_alone`.
pub async fn probe_daemon_http(app: &str, binary: &str) -> ProbeOutcome {
    // #6290: checked FIRST. A per-invocation member has neither a socket nor an
    // address, so falling through would reach `NoAddress` — which renders
    // `down` for a tool that is installed and working.
    if presence_only(binary) {
        return probe_presence(binary);
    }
    if let Some(socket) = uds_socket_for(binary) {
        // #6287: the method is per-binary. `uds_socket_for` and
        // `uds_health_method` cover the same set, which
        // `every_uds_member_has_a_health_method` is what enforces — so this
        // fallback is unreachable in practice, and it reports a probe failure
        // rather than guessing a method whose absence would read as a broken
        // daemon.
        let Some(method) = uds_health_method(binary) else {
            return ProbeOutcome::ProbeFailed {
                detail: format!("{binary} serves a socket but names no health method"),
            };
        };
        // #6350: an on-demand member is not expected to be running. A socket
        // that does not answer is its NORMAL resting state, so probing it
        // directly would report a healthy installation as `down` and send
        // `verify_tail` off to repair a service that has nothing wrong with it.
        // Starting it is the probe: a member that can be started and answers
        // `<domain>.health` is working, and one that cannot is not.
        if on_demand_member(binary) {
            if let Err(e) = trusty_common::uds::OnDemandAnalyze::at(&socket)
                .ensure_running()
                .await
            {
                return ProbeOutcome::ProbeFailed {
                    detail: format!("{binary} could not be started on demand: {e}"),
                };
            }
        }
        // #6285: a member that has retired HTTP has exactly one transport and
        // stops here. trusty-search has not retired it yet, so its socket
        // answer is RECONCILED with the HTTP legs instead of replacing them —
        // an installed daemon older than the listener answers only on 7878.
        if !dual_transport(binary) {
            return probe_socket(&socket, method).await;
        }
        let client = match build_probe_client() {
            Ok(c) => c,
            // A client that could not be built is a LOCAL failure and says
            // nothing about the daemon, so the socket leg alone is still the
            // honest answer — never a `ProbeFailed` that discards it.
            Err(_) => return probe_socket(&socket, method).await,
        };
        let (recorded, fixed) = resolve_probe_bases(app, binary);
        // Concurrently: a refusing leg costs a connect, but a wedged one costs
        // REQUEST_TIMEOUT, and `verify_tail` polls every member repeatedly.
        let (uds, http) = tokio::join!(
            probe_socket(&socket, method),
            probe_bases(&client, recorded, fixed)
        );
        return reconcile(vec![uds, http]);
    }

    let client = match build_probe_client() {
        Ok(c) => c,
        Err(e) => {
            return ProbeOutcome::ProbeFailed {
                detail: format!("build probe client: {e}"),
            }
        }
    };
    let (recorded, fixed) = resolve_probe_bases(app, binary);
    probe_bases(&client, recorded, fixed).await
}

/// Synchronous entry point for [`probe_daemon_http`].
///
/// Why: `tctl`'s command dispatch is synchronous, and the probe is called from
/// `status`, `stack health`, `stack doctor`, `verify_tail` and `up` — none of
/// which run inside a reactor. The probe runs on a dedicated thread with its own
/// current-thread runtime rather than calling `Runtime::block_on` in place: that
/// makes it unconditionally safe to call even if a future caller *is* inside a
/// runtime (building a runtime and blocking on it from within another runtime's
/// worker thread panics), and a current-thread runtime is far cheaper to spin up
/// than `runtime::block_on`'s multi-thread one — which matters because
/// `verify_tail`'s poll loop can probe a single member a dozen times.
/// `tokio::join!` still runs both legs concurrently on it.
/// What: spawns the probe thread, joins it, and returns its outcome. A thread
/// that cannot be spawned, or that panics, is reported as
/// [`ProbeOutcome::ProbeFailed`] — never as a daemon verdict.
/// Test: `super::probe::tests::probe_member_health_*` and
/// `verify_tail::tests::verify_one_*` drive this path against a stub server.
pub fn probe_member_http_blocking(app: &str, binary: &str) -> ProbeOutcome {
    let app = app.to_owned();
    let binary = binary.to_owned();
    match std::thread::Builder::new()
        .name("tctl-health-probe".to_owned())
        .spawn(move || probe_on_new_runtime(&app, &binary))
    {
        Ok(handle) => handle.join().unwrap_or_else(|_| ProbeOutcome::ProbeFailed {
            detail: "health-probe thread panicked".to_owned(),
        }),
        Err(e) => ProbeOutcome::ProbeFailed {
            detail: format!("spawn health-probe thread: {e}"),
        },
    }
}

/// Drive [`probe_daemon_http`] on a fresh current-thread runtime.
///
/// Why: the body of [`probe_member_http_blocking`]'s worker thread, extracted so
/// the runtime-construction failure path is expressible without nesting two
/// `match`es inside a closure.
/// What: builds a current-thread runtime and blocks on the probe; a runtime that
/// cannot be built is [`ProbeOutcome::ProbeFailed`].
/// Test: as [`probe_member_http_blocking`].
fn probe_on_new_runtime(app: &str, binary: &str) -> ProbeOutcome {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(probe_daemon_http(app, binary)),
        Err(e) => ProbeOutcome::ProbeFailed {
            detail: format!("build probe runtime: {e}"),
        },
    }
}

#[cfg(test)]
#[path = "probe_http_tests.rs"]
mod tests;
