//! RTK subprocess delegation + the async compression wrapper.
//!
//! Why: When the user has installed RTK (https://github.com/rtk-ai/rtk),
//! delegating to it gets us the upstream implementation for free. When `rtk`
//! is absent we fall back to the native filter chain.
//! What: `compress_via_rtk` (resolves the binary through
//! `trusty_common::bin_resolve`), `compress_via_rtk_with` (the same against a
//! caller-chosen [`RtkResolver`], #7325), `compress_via_rtk_binary` (the
//! spawn), `rtk_pipe_argv` / `rtk_filter_for` (the `rtk pipe` invocation),
//! and the `compress_tool_output_async` wrapper that prefers RTK then falls
//! back.
//!
//! We invoke `rtk pipe`, never a subcommand: `rtk git status` EXECUTES
//! `git status` and returns its output, discarding stdin. This module's input
//! is output that was already captured, so only `pipe` mode is correct.
//! Test: `compress_via_rtk_returns_none_when_binary_absent`,
//! `rtk_pipe_argv_never_carries_the_tool_name`,
//! `compress_tool_output_async_falls_back_when_rtk_absent`,
//! `a_resolver_naming_a_missing_binary_falls_back_to_native`,
//! `value_forces_native_fallback_accepts_only_truthy_spellings`,
//! `default_rtk_resolver_is_the_real_resolver_without_the_env_var` in
//! `tool_output::tests`.

use super::compress_tool_output;

/// Which code path produced a compressed tool output.
///
/// Why: Issue #1956's `tm compress` stats logging needs to distinguish an
/// `rtk`-binary compression from the always-available native fallback chain
/// so aggregate savings can be broken down by path, matching the spike doc's
/// "compression path: native fallback chain (rtk NOT on PATH)" framing
/// (`docs/specs/tool-output-interception-seam.md`).
/// What: `RtkBinary` when the external `rtk` subprocess produced the result;
/// `NativeFallback` when the in-tree filter chain (`compress_tool_output`)
/// did, either because `rtk` is absent from `PATH` or its invocation failed.
/// Test: `compress_tool_output_async_with_path_reports_native_fallback_when_rtk_absent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionPath {
    /// The external `rtk` CLI subprocess compressed the output.
    RtkBinary,
    /// The in-tree native filter chain (`compress_tool_output`) compressed
    /// the output, either because `rtk` is not installed or it failed.
    NativeFallback,
}

impl CompressionPath {
    /// Stable, lowercase machine-parseable name for structured log fields.
    ///
    /// Why: `tm compress`'s stats log (issue #1956) emits this as the
    /// `compression_path` tracing field; a stable string constant keeps log
    /// consumers from having to special-case `Debug` formatting.
    /// What: `"rtk_binary"` / `"native_fallback"`.
    /// Test: `compression_path_as_str_is_stable`.
    pub fn as_str(self) -> &'static str {
        match self {
            CompressionPath::RtkBinary => "rtk_binary",
            CompressionPath::NativeFallback => "native_fallback",
        }
    }
}

/// The filter names `rtk pipe -f` accepts, as of **rtk 0.48.0**.
///
/// Why: `rtk pipe` rejects an unrecognised `-f` name with exit 1 and no
/// output, so the mapping must only ever emit a name from this list. The
/// list is pinned to a version because it is rtk's, not ours: when rtk adds
/// or renames a filter this constant goes stale silently, and a stale entry
/// costs a hard failure rather than a missed optimisation.
/// What: Read verbatim from `rtk pipe -f <unknown>`, which prints the full
/// set it accepts. Re-read it after an rtk upgrade.
/// Test: `every_pinned_filter_round_trips_from_its_spaced_form`.
pub(super) const RTK_PIPE_FILTERS: &[&str] = &[
    "cargo-test",
    "pytest",
    "go-test",
    "go-build",
    "ctest",
    "tsc",
    "vitest",
    "grep",
    "rg",
    "find",
    "fd",
    "git-log",
    "git-diff",
    "git-status",
    "log",
    "mypy",
    "ruff-check",
    "ruff-format",
    "prettier",
    "phpunit",
    "pest",
    "paratest",
    "php-test",
    "ecs",
    "phpstan",
    "pint",
];

/// Longest run of leading words considered when matching a filter name.
///
/// Why: Every name in [`RTK_PIPE_FILTERS`] is one or two segments; three
/// leaves headroom without letting a long command line match by accident.
const MAX_FILTER_WORDS: usize = 3;

/// Pick the `rtk pipe -f` filter for a tool name, if one applies.
///
/// Why: A tool name here is a whole command line (`"cargo test -p foo"`).
/// Naming the matching filter lets rtk apply the right one instead of its
/// generic default, and returning `None` for anything unrecognised keeps an
/// invalid `-f` — which rtk rejects with exit 1 — unreachable.
/// What: Joins the leading words with hyphens, longest run first, and takes
/// the first exact match in [`RTK_PIPE_FILTERS`]. `"cargo test -p foo"` tries
/// `cargo-test-p`, then `cargo-test` (a hit). Unrecognised yields `None`.
/// Test: `rtk_filter_for_maps_known_tool_names`,
/// `every_pinned_filter_round_trips_from_its_spaced_form`.
pub(super) fn rtk_filter_for(tool_name: &str) -> Option<&'static str> {
    let words: Vec<&str> = tool_name.split_whitespace().collect();
    for take in (1..=words.len().min(MAX_FILTER_WORDS)).rev() {
        let candidate = words[..take].join("-");
        if let Some(hit) = RTK_PIPE_FILTERS.iter().find(|f| **f == candidate) {
            return Some(hit);
        }
    }
    None
}

/// Build the argv that runs `rtk` in stdin-filter mode.
///
/// Why: Every rtk subcommand other than `pipe` RUNS the named tool —
/// `rtk git status` executes `git status` and prints its output, discarding
/// whatever was piped in. This function receives output that has ALREADY been
/// captured, so a subcommand invocation would both re-run the tool and return
/// output unrelated to its input. `rtk pipe` is the one mode that reads stdin
/// and filters it.
/// What: `["pipe", "-f", <filter>]` when [`rtk_filter_for`] matches, else
/// `["pipe"]`. The `&'static str` return makes it impossible for any part of
/// `tool_name` to reach argv verbatim — every element is a compile-time
/// constant.
/// Test: `rtk_pipe_argv_never_carries_the_tool_name`,
/// `rtk_pipe_argv_falls_back_to_bare_pipe_for_an_unknown_tool`.
pub(super) fn rtk_pipe_argv(tool_name: &str) -> Vec<&'static str> {
    match rtk_filter_for(tool_name) {
        Some(filter) => vec!["pipe", "-f", filter],
        None => vec!["pipe"],
    }
}

/// Locates the `rtk` executable for [`compress_via_rtk_with`].
///
/// Why: `trusty_common::bin_resolve::resolve_binary` finds a Homebrew `rtk`
/// whatever `PATH` says — deliberately, so a launchd-spawned daemon still
/// reaches it. That leaves a test with no way to pin native-fallback
/// behaviour: the same assertion passes on a host without rtk and fails on a
/// host with it (#7325). Naming the resolver lets the caller decide.
/// What: the same signature as `resolve_binary` — a binary name in, an
/// existing path or `None` out. A resolver that returns a path to a missing
/// file still falls back, because [`compress_via_rtk_binary`] returns `None`
/// when the spawn fails; the seam can only ever downgrade to the native
/// chain, never claim `rtk_binary` for a binary that did not run.
/// Test: `a_resolver_naming_a_missing_binary_falls_back_to_native`.
pub type RtkResolver = fn(&str) -> Option<std::path::PathBuf>;

/// Env var that forces the native fallback chain even where `rtk` is installed.
///
/// Why: `tm compress`'s process-level tests assert the native chain's own
/// output — the 80-byte size gate, byte-for-byte passthrough — which rtk
/// rewrites (it strips the trailing newline). A spawned process cannot be
/// handed a resolver, so it reads one env var instead (#7325). It doubles as
/// the operator escape hatch for a broken or slow local rtk.
/// What: `1`, `true`, `yes` or `on` (case-insensitive, trimmed) select
/// [`no_rtk`]; every other value, and an unset variable, keep the real
/// resolver. Read once per compression call.
/// Test: `value_forces_native_fallback_accepts_only_truthy_spellings`.
pub const ENV_COMPRESS_NO_RTK: &str = "TRUSTY_COMPRESS_NO_RTK";

/// An [`RtkResolver`] that never resolves, forcing the native fallback chain.
///
/// Why: a test that asserts the native chain's own output needs the same
/// answer on a host with `rtk` installed and on one without (#7325).
/// What: ignores the name and returns `None`, so [`compress_via_rtk_with`]
/// takes its absent-binary arm.
/// Test: `no_rtk_resolver_forces_the_native_chain_on_any_host`.
pub fn no_rtk(_name: &str) -> Option<std::path::PathBuf> {
    None
}

/// Whether a raw [`ENV_COMPRESS_NO_RTK`] value asks for the native chain.
///
/// Why: taking the value rather than reading the process environment keeps
/// the spelling rules testable without `std::env::set_var`, which is
/// process-global and races every other test in the binary.
/// What: trims, lowercases, and accepts `1` / `true` / `yes` / `on`.
/// Test: `value_forces_native_fallback_accepts_only_truthy_spellings`.
pub(super) fn value_forces_native_fallback(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// The [`RtkResolver`] production compression uses.
///
/// Why: one place decides whether this process may reach rtk, so every
/// consumer — `tm compress`, `trusty-agents`'s `tool_loop` — honours
/// [`ENV_COMPRESS_NO_RTK`] identically.
/// What: [`no_rtk`] when the env var is truthy, else
/// `trusty_common::bin_resolve::resolve_binary`.
/// Test: `default_rtk_resolver_is_the_real_resolver_without_the_env_var`.
pub fn default_rtk_resolver() -> RtkResolver {
    // #7325: the one opt-out; nothing else may force the native chain.
    if value_forces_native_fallback(std::env::var(ENV_COMPRESS_NO_RTK).ok().as_deref()) {
        return no_rtk;
    }
    // See CLAUDE.md "Common entry point": trusty-common owns binary resolution.
    trusty_common::bin_resolve::resolve_binary
}

/// Pipe `output` through the `rtk` CLI subprocess if installed.
///
/// Why: When the user has installed RTK (https://github.com/rtk-ai/rtk),
/// delegating to it gets us the upstream implementation for free, with
/// updates from the source project. When `rtk` is not on `PATH` we fall
/// back to the native filter.
/// What: Resolves `rtk` through [`default_rtk_resolver`], then delegates to
/// [`compress_via_rtk_with`].
/// Test: `compress_via_rtk_returns_none_when_binary_absent`.
pub async fn compress_via_rtk(tool_name: &str, output: &str) -> Option<String> {
    compress_via_rtk_with(default_rtk_resolver(), tool_name, output).await
}

/// Pipe `output` through the `rtk` binary `resolve` names, if it names one.
///
/// Why: the injection point [`compress_via_rtk`] wraps — see [`RtkResolver`]
/// for why a test needs to choose the resolver (#7325).
/// What: calls `resolve("rtk")` and delegates to [`compress_via_rtk_binary`].
/// An absent binary is a `debug` event — the common, expected case — while a
/// binary that runs and fails is a `warn`.
/// Test: `compress_via_rtk_returns_none_when_binary_absent`,
/// `a_resolver_naming_a_missing_binary_falls_back_to_native`.
pub async fn compress_via_rtk_with(
    resolve: RtkResolver,
    tool_name: &str,
    output: &str,
) -> Option<String> {
    let Some(bin) = resolve("rtk") else {
        tracing::debug!(tool = tool_name, "rtk not on PATH; using native fallback");
        return None;
    };
    compress_via_rtk_binary(&bin, tool_name, output).await
}

/// Run one already-resolved `rtk` binary over `output`.
///
/// Why: Taking the binary as a parameter lets a test point this at a shim
/// script that records its argv, proving the invocation shape without needing
/// the real `rtk` on `PATH`.
/// What: Spawns `<bin> pipe [-f <filter>]` per [`rtk_pipe_argv`], writes
/// `output` to stdin, returns stdout. Returns `None` on any failure (spawn,
/// non-zero exit, stdin/stdout IO error, decode error) so the caller falls
/// back gracefully; a non-zero exit logs a `warn` carrying the status and the
/// head of stderr rather than failing silently.
/// Test: `rtk_binary_receives_the_pipe_invocation`,
/// `rtk_binary_returns_none_and_warns_on_non_zero_exit`.
pub(super) async fn compress_via_rtk_binary(
    bin: &std::path::Path,
    tool_name: &str,
    output: &str,
) -> Option<String> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    // See: every rtk subcommand but `pipe` RUNS the named tool
    let argv = rtk_pipe_argv(tool_name);

    let mut child = Command::new(bin)
        .args(&argv)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;

    if let Some(mut stdin) = child.stdin.take() {
        // Write output and close stdin so rtk can finish.
        stdin.write_all(output.as_bytes()).await.ok()?;
        drop(stdin);
    }

    let out = child.wait_with_output().await.ok()?;
    if !out.status.success() {
        tracing::warn!(
            tool = tool_name,
            argv = ?argv,
            status = %out.status,
            stderr = %stderr_head(&out.stderr),
            "rtk exited non-zero; falling back to native compression"
        );
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// First line of a subprocess's stderr, truncated for a log field.
///
/// Why: A failing `rtk` can emit an arbitrary amount of stderr; a log field
/// carrying all of it is worse than one carrying the part that names the
/// failure.
/// What: Lossy-decodes, takes the first non-empty line, and truncates it to
/// 200 bytes on a character boundary.
/// Test: `stderr_head_takes_the_first_line_and_truncates`.
pub(super) fn stderr_head(stderr: &[u8]) -> String {
    const MAX: usize = 200;
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut end = line.len().min(MAX);
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_string()
}

/// Compress a tool's output, trying the RTK subprocess first and falling
/// back to the native filter chain.
///
/// Why: Most users won't have RTK installed; the native filters are always
/// available. When RTK is present we delegate so we stay aligned with upstream.
/// What: Async wrapper — calls `compress_via_rtk`, falls back to
/// `compress_tool_output` (synchronous, native) on `None`. Thin wrapper over
/// [`compress_tool_output_async_with_path`] that discards the path signal,
/// kept as the original stable signature so existing callers (`trusty-agents`'s
/// `llm::tool_loop`) are unaffected by the #1959 hoist.
/// Test: `compress_tool_output_async_falls_back_when_rtk_absent`.
pub async fn compress_tool_output_async(tool_name: &str, output: &str) -> String {
    compress_tool_output_async_with_path(tool_name, output)
        .await
        .0
}

/// Compress a tool's output like [`compress_tool_output_async`], additionally
/// reporting which code path produced the result.
///
/// Why: Issue #1956's `tm compress` subcommand needs a `compression_path`
/// field for its structured stats log so compression effectiveness can be
/// broken down by rtk-binary vs. native-fallback — see [`CompressionPath`].
/// Splitting this out (rather than changing `compress_tool_output_async`'s
/// signature) keeps every existing call site source-compatible.
/// What: Tries `compress_via_rtk` first; returns `(text, RtkBinary)` on
/// success, else `(compress_tool_output(..), NativeFallback)`.
/// Test: `compress_tool_output_async_with_path_reports_native_fallback_when_rtk_absent`.
pub async fn compress_tool_output_async_with_path(
    tool_name: &str,
    output: &str,
) -> (String, CompressionPath) {
    compress_tool_output_async_with_path_using(default_rtk_resolver(), tool_name, output).await
}

/// [`compress_tool_output_async_with_path`] against a caller-chosen resolver.
///
/// Why: a test that pins the native chain's own output must be able to say
/// so — passing [`no_rtk`] makes the assertion hold on a host with rtk
/// installed and on one without, with no env mutation and no weakened
/// assertion (#7325).
/// What: identical to [`compress_tool_output_async_with_path`], except that
/// `resolve` decides whether rtk is reachable. `RtkBinary` is still reported
/// only when a subprocess actually ran and exited zero, so a resolver that
/// names a missing file yields `NativeFallback`.
/// Test: `no_rtk_resolver_forces_the_native_chain_on_any_host`,
/// `a_resolver_naming_a_missing_binary_falls_back_to_native`. `trusty-mpm`'s
/// `native_fallback_elision_reports_a_matching_non_zero_reduction` is the
/// caller this seam was added for.
pub async fn compress_tool_output_async_with_path_using(
    resolve: RtkResolver,
    tool_name: &str,
    output: &str,
) -> (String, CompressionPath) {
    if let Some(s) = compress_via_rtk_with(resolve, tool_name, output).await {
        return (s, CompressionPath::RtkBinary);
    }
    (
        compress_tool_output(tool_name, output),
        CompressionPath::NativeFallback,
    )
}
