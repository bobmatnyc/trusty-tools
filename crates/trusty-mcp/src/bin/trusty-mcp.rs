//! `trusty-mcp <service>` — one stdio MCP bridge for every trusty daemon (#6316).
//!
//! Why: the 2026-07-24 owner directive is "no per-crate MCP binaries". Each
//! daemon crate was growing its own `serve --stdio` verb, and every one of them
//! is the same program: resolve that daemon's Unix socket, read line-delimited
//! JSON-RPC from stdin, forward each request over the socket, write the answer
//! to stdout. Slice 2 of #6316 made the forwarding itself shared
//! (`trusty_mcp::daemon_bridge_json_rpc`); this binary is the one entry point
//! that drives it, so an MCP client's config names `trusty-mcp <service>`
//! rather than a different binary and a different verb per daemon.
//!
//! What: a table of the three UDS-backed daemons and the two facts that differ
//! between them — which methods answer as a stream, and how large a response
//! frame may be. Everything else comes from the shared crate: the socket path
//! from `trusty_common::daemon_socket_path`, which is the same call the daemon
//! itself makes to decide where to bind, and the transport from
//! `trusty_mcp::run_stdio_bridge`.
//!
//! ## It does not start anything
//!
//! No probe, no spawn, no lock. A daemon's readiness guard is that daemon's own
//! (trusty-memory's `serve --stdio` still takes the `StartLock` #5267 gave it),
//! and #1152 is the record of what N independently-spawning bridges cost. A
//! request that arrives here with nothing listening is answered with a JSON-RPC
//! error carrying the request's own id — never silence, because an unmatchable
//! answer is indistinguishable from a hang to the client (#6309).
//!
//! ## STDOUT is the JSON-RPC channel
//!
//! Nothing here writes to stdout but the forwarder. The usage text, the startup
//! line and every failure go to stderr, including a `--help` a human typed:
//! one rule with no exception is what keeps a stray `println!` reviewable.
//!
//! Test: `tests/bridge_bin_cli.rs` drives the built binary end to end; the unit
//! tests below cover the service table and pin it against the daemons' own
//! constants.

use std::process::ExitCode;

use anyhow::Context;
use trusty_mcp::{UdsBridgeConfig, run_stdio_bridge};

/// Exit code for a usage error, distinct from a runtime failure's `1`.
///
/// Why: a wrapper that spawns this binary needs to tell "you asked for a
/// service that does not exist" from "the daemon could not be reached". The
/// first is the caller's config to fix, the second is the machine's state.
const EXIT_USAGE: u8 = 2;

/// One mebibyte, so the frame budgets below read as the figures they mirror.
const MIB: u64 = 1024 * 1024;

/// A daemon this binary can bridge to.
///
/// Why: `app` is the only identifier that matters — it is what
/// `trusty_common::daemon_socket_path` resolves under, what the daemon binds
/// under, and what names the daemon in error text. The other two fields are the
/// only things that genuinely differ between the three daemons.
/// What: `streaming_methods` is the refusal list the bridge applies before it
/// dials; `max_frame_bytes` is the response budget, which must be at least the
/// daemon's own or a frame the daemon served becomes a `FrameTooLarge` on the
/// way back.
/// Test: `the_table_matches_each_daemons_own_constants`.
struct Service {
    /// The daemon's app name, e.g. `"trusty-memory"`.
    app: &'static str,
    /// Methods that answer in many frames, which MCP stdio cannot carry.
    streaming_methods: &'static [&'static str],
    /// Response-frame budget, matching the daemon's own listener budget.
    max_frame_bytes: u64,
}

/// Every daemon `trusty-mcp <service>` can bridge to.
///
/// Why these values are here and not imported: each list belongs to a crate
/// that already depends on `trusty-mcp`, so importing them would put the whole
/// of trusty-memory, trusty-search and trusty-analyze into this crate's build
/// — for three string arrays and three integers. `the_table_matches_each_
/// daemons_own_constants` reads those crates' sources instead and fails on
/// drift, which is what the import would have bought.
///
/// trusty-analyze streams nothing: its socket router registers no streaming
/// method, and its MCP surface is a tool translator that answers each call in
/// one frame.
///
/// Test: `the_table_matches_each_daemons_own_constants`,
/// `every_service_is_reachable_by_both_of_its_names`.
const SERVICES: &[Service] = &[
    // #6316: mirrors `trusty_memory::transport::uds::{STREAM_METHODS,
    // MAX_FRAME_BYTES}`.
    Service {
        app: "trusty-memory",
        streaming_methods: &["memory.chat", "memory.activity_stream"],
        max_frame_bytes: 32 * MIB,
    },
    // #6316: mirrors `trusty_search::service::rpc::streams::METHODS` and
    // `trusty_search::service::socket::MAX_FRAME_BYTES`.
    Service {
        app: "trusty-search",
        streaming_methods: &[
            "search.status.stream",
            "search.index.reindex.stream",
            "search.index.file_events",
        ],
        max_frame_bytes: 64 * MIB,
    },
    // #6316: mirrors `trusty_analyze::service::rpc::MAX_FRAME_BYTES`.
    Service {
        app: "trusty-analyze",
        streaming_methods: &[],
        max_frame_bytes: 32 * MIB,
    },
];

/// The short name a caller may type instead of the full app name.
fn short_name(app: &str) -> &str {
    app.strip_prefix("trusty-").unwrap_or(app)
}

/// Resolve the `<service>` positional to a table row.
///
/// Why both spellings: a human types `trusty-mcp memory`, while an MCP client
/// config generated from a daemon's own name carries `trusty-memory`. Refusing
/// one of them would be a usage error for a name that is unambiguous.
/// What: matches the app name exactly, or the app name with its `trusty-`
/// prefix removed. Returns `None` for anything else, which is what produces the
/// exit-2 usage error.
/// Test: `every_service_is_reachable_by_both_of_its_names`,
/// `an_unknown_service_resolves_to_nothing`.
fn lookup(arg: &str) -> Option<&'static Service> {
    SERVICES
        .iter()
        .find(|s| s.app == arg || short_name(s.app) == arg)
}

/// What the command line asked for.
enum Parsed {
    /// Print the usage text and exit 0.
    Help,
    /// Bridge stdio to this daemon.
    Serve(&'static Service),
    /// The command line is wrong; this says how.
    Usage(String),
}

/// Classify the arguments after the program name.
///
/// Why an extra argument is a usage error rather than something ignored: a
/// silently-dropped flag is a config that looks applied and is not. The only
/// shape this binary accepts is exactly one positional.
/// What: no arguments, an unknown service, or a second argument each become
/// [`Parsed::Usage`]; `-h` / `--help` becomes [`Parsed::Help`].
/// Test: `no_arguments_is_a_usage_error`, `an_unknown_service_is_a_usage_error`,
/// `a_second_argument_is_a_usage_error`, `help_is_recognised`.
fn parse<'a>(args: impl IntoIterator<Item = &'a str>) -> Parsed {
    let mut args = args.into_iter();

    let Some(first) = args.next() else {
        return Parsed::Usage("a service is required".to_string());
    };

    if first == "-h" || first == "--help" {
        return Parsed::Help;
    }

    let Some(service) = lookup(first) else {
        return Parsed::Usage(format!(
            "unknown service {first:?} — expected one of {}",
            service_names().join(", ")
        ));
    };

    if let Some(extra) = args.next() {
        return Parsed::Usage(format!(
            "unexpected argument {extra:?} — the only argument is the service"
        ));
    }

    Parsed::Serve(service)
}

/// The short service names, in table order, for the usage text.
fn service_names() -> Vec<&'static str> {
    SERVICES.iter().map(|s| short_name(s.app)).collect()
}

/// The usage text, written to stderr in every case.
///
/// Test: `usage_names_every_service`.
fn usage() -> String {
    let mut out = String::from(
        "trusty-mcp — stdio MCP bridge to a trusty daemon's Unix socket\n\
         \n\
         Usage:\n  \
         trusty-mcp <service>\n\
         \n\
         Services (either spelling):\n",
    );
    for service in SERVICES {
        out.push_str(&format!(
            "  {:<10} {}\n",
            short_name(service.app),
            service.app
        ));
    }
    out.push_str(
        "\n\
         Reads line-delimited JSON-RPC 2.0 on stdin and writes one response per\n\
         request to stdout; every diagnostic goes to stderr. It does not start the\n\
         daemon — a request that arrives with nothing listening is answered with a\n\
         JSON-RPC error carrying the request's id.\n\
         \n\
         Exit: 0 when stdin reaches EOF, 2 on a usage error, 1 on an I/O failure.\n",
    );
    out
}

/// Resolve the socket, build the config, and run the forwarder.
///
/// # Errors
///
/// The data directory could not be resolved, or stdin/stdout failed. A daemon
/// that is down is not an error here — it is a JSON-RPC error response.
///
/// Test: `a_dead_socket_answers_with_a_matchable_error` in
/// `tests/bridge_bin_cli.rs`.
async fn serve(service: &Service) -> anyhow::Result<()> {
    // The ONE resolver: the same call the daemon makes to decide where to bind,
    // so a bridge and its daemon cannot disagree about the path (#6316).
    let socket = trusty_common::daemon_socket_path(service.app)
        .with_context(|| format!("could not resolve the {} socket path", service.app))?;

    // Stderr, never stdout — stdout is the JSON-RPC channel.
    eprintln!(
        "trusty-mcp: bridging stdio to {} at {}",
        service.app,
        socket.display()
    );

    run_stdio_bridge(
        UdsBridgeConfig::new(socket, service.app)
            .with_streaming_methods(service.streaming_methods.iter().copied())
            .with_max_frame_bytes(service.max_frame_bytes),
    )
    .await
}

/// Parse, then either explain or serve.
///
/// A non-UTF-8 argument is read lossily rather than panicking: it cannot match
/// a service name, so it falls through to the same usage error any other
/// unknown service produces.
async fn run() -> anyhow::Result<ExitCode> {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    match parse(args.iter().map(String::as_str)) {
        Parsed::Help => {
            eprint!("{}", usage());
            Ok(ExitCode::SUCCESS)
        }
        Parsed::Usage(problem) => {
            eprintln!("trusty-mcp: {problem}");
            eprint!("{}", usage());
            Ok(ExitCode::from(EXIT_USAGE))
        }
        Parsed::Serve(service) => {
            serve(service).await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Current-thread runtime: this process multiplexes nothing — one stdin, one
/// request at a time, one socket dial. A work-stealing pool would add threads
/// with nothing to steal.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(cause) => {
            eprintln!("trusty-mcp: {cause:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Read a sibling crate's source out of this checkout.
    ///
    /// Returns `None` in a published tarball, where no sibling crate exists and
    /// there is nothing for the table to drift from.
    fn sibling_source(rel: &str) -> Option<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(rel);
        std::fs::read_to_string(path).ok()
    }

    /// The string literals in a `pub const NAME: &[&str] = &[…];` declaration.
    fn string_array_const(src: &str, name: &str) -> Vec<String> {
        let decl = format!("pub const {name}: &[&str] = &[");
        let start = src
            .find(&decl)
            .unwrap_or_else(|| panic!("{name} is declared"))
            + decl.len();
        let end = start + src[start..].find("];").expect("the array is terminated");
        src[start..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    }

    /// Every `pub const NAME: &str = "…";` literal in a file, in source order.
    fn str_const_literals(src: &str) -> Vec<String> {
        src.lines()
            .filter_map(|line| line.trim().strip_prefix("pub const "))
            .filter_map(|rest| rest.split_once(": &str = "))
            .filter_map(|(_, value)| value.trim().strip_prefix('"'))
            .filter_map(|value| value.split('"').next())
            .map(str::to_string)
            .collect()
    }

    /// The value of a `pub const NAME: u64 = a * b * c;` declaration.
    fn u64_const(src: &str, name: &str) -> u64 {
        let decl = format!("pub const {name}: u64 = ");
        let start = src
            .find(&decl)
            .unwrap_or_else(|| panic!("{name} is declared"))
            + decl.len();
        let end = start + src[start..].find(';').expect("the declaration ends");
        src[start..end]
            .split('*')
            .map(|part| {
                part.trim()
                    .replace('_', "")
                    .parse::<u64>()
                    .expect("a product of integer literals")
            })
            .product()
    }

    fn service(app: &str) -> &'static Service {
        lookup(app).expect("the service is in the table")
    }

    /// Why: the table is a second copy of three daemons' constants, and the
    /// silent failure mode of a second copy is exactly #6286 — a method the
    /// daemon streams and the bridge's list omits leaves an MCP client waiting
    /// for a frame that is never coming. Importing the crates would cost this
    /// lean rlib the whole of trusty-memory, trusty-search and trusty-analyze,
    /// so the equality is asserted against their sources instead.
    /// What: reads each daemon's own declaration and compares it with the row
    /// here — the streaming list for memory and search, the frame budget for
    /// all three. A frame budget below the daemon's would turn a response the
    /// daemon served into a `FrameTooLarge` on the way back.
    /// Test: this test.
    #[test]
    fn the_table_matches_each_daemons_own_constants() {
        if let Some(src) = sibling_source("trusty-memory/src/transport/uds.rs") {
            assert_eq!(
                string_array_const(&src, "STREAM_METHODS"),
                service("memory").streaming_methods,
                "trusty-memory's streaming methods drifted from this table"
            );
            assert_eq!(
                u64_const(&src, "MAX_FRAME_BYTES"),
                service("memory").max_frame_bytes,
                "trusty-memory's frame budget drifted from this table"
            );
        }

        if let Some(src) = sibling_source("trusty-search/src/service/rpc/streams.rs") {
            assert_eq!(
                str_const_literals(&src),
                service("search").streaming_methods,
                "trusty-search's streaming methods drifted from this table"
            );
        }
        if let Some(src) = sibling_source("trusty-search/src/service/socket.rs") {
            assert_eq!(
                u64_const(&src, "MAX_FRAME_BYTES"),
                service("search").max_frame_bytes,
                "trusty-search's frame budget drifted from this table"
            );
        }

        if let Some(src) = sibling_source("trusty-analyze/src/service/rpc.rs") {
            assert_eq!(
                u64_const(&src, "MAX_FRAME_BYTES"),
                service("analyze").max_frame_bytes,
                "trusty-analyze's frame budget drifted from this table"
            );
        }
    }

    /// Why: an MCP client config generated from a daemon's own name carries the
    /// full `trusty-<name>`; a human types the short one. Both are unambiguous.
    /// What: each row resolves from its app name and from that name without the
    /// `trusty-` prefix, and resolves to the same row.
    /// Test: this test.
    #[test]
    fn every_service_is_reachable_by_both_of_its_names() {
        for row in SERVICES {
            let long = lookup(row.app).expect("the app name resolves");
            let short = lookup(short_name(row.app)).expect("the short name resolves");
            assert_eq!(long.app, row.app);
            assert_eq!(short.app, row.app);
        }
    }

    /// Why: an unmatched service must reach the exit-2 arm, not a default.
    /// What: names that are close to a real one, and the empty string, all miss.
    /// Test: this test.
    #[test]
    fn an_unknown_service_resolves_to_nothing() {
        for arg in [
            "",
            "trusty-",
            "mem",
            "trusty-mcp",
            "trusty-review",
            "MEMORY",
        ] {
            assert!(lookup(arg).is_none(), "{arg:?} must not resolve");
        }
    }

    /// Why: Fail-Open Check — every wrong command line must name what is wrong.
    /// What: no arguments produces a usage error mentioning the service.
    /// Test: this test.
    #[test]
    fn no_arguments_is_a_usage_error() {
        let Parsed::Usage(problem) = parse(std::iter::empty()) else {
            panic!("an empty command line is a usage error");
        };
        assert!(problem.contains("service"), "{problem}");
    }

    /// Why: the message has to say which name was rejected and what was valid,
    /// or the caller cannot fix their config from it.
    /// What: the usage error quotes the rejected name and lists every service.
    /// Test: this test.
    #[test]
    fn an_unknown_service_is_a_usage_error() {
        let Parsed::Usage(problem) = parse(["review"]) else {
            panic!("an unknown service is a usage error");
        };
        assert!(problem.contains("review"), "{problem}");
        for name in service_names() {
            assert!(problem.contains(name), "{problem} omits {name}");
        }
    }

    /// Why: a silently-ignored flag is a config that looks applied and is not.
    /// What: a second positional is refused and named.
    /// Test: this test.
    #[test]
    fn a_second_argument_is_a_usage_error() {
        let Parsed::Usage(problem) = parse(["memory", "--socket=/tmp/x.sock"]) else {
            panic!("an extra argument is a usage error");
        };
        assert!(problem.contains("--socket=/tmp/x.sock"), "{problem}");
    }

    /// Why: `--help` is not a failure, so it must not take the exit-2 arm.
    /// What: both spellings parse as help; a service still parses as serve.
    /// Test: this test.
    #[test]
    fn help_is_recognised() {
        assert!(matches!(parse(["-h"]), Parsed::Help));
        assert!(matches!(parse(["--help"]), Parsed::Help));
        assert!(matches!(parse(["memory"]), Parsed::Serve(_)));
    }

    /// Why: the usage text is the only place a caller learns what to type, and
    /// a service added to the table without a row there is invisible.
    /// What: every service appears under both spellings, and the no-spawn
    /// contract is stated.
    /// Test: this test.
    #[test]
    fn usage_names_every_service() {
        let text = usage();
        for row in SERVICES {
            assert!(text.contains(row.app), "usage omits {}", row.app);
            assert!(
                text.contains(short_name(row.app)),
                "usage omits {}",
                short_name(row.app)
            );
        }
        assert!(text.contains("does not start the"), "{text}");
    }
}
