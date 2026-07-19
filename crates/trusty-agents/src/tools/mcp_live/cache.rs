//! Discovery result cache + per-server timeout for live MCP tool discovery
//! (#3266 BLOCK remediation, DoS fix for #3238's live-discovery path).
//!
//! Why: The original implementation re-ran spawn + `initialize` +
//! `tools/list` for every configured server on EVERY persona turn, entirely
//! sequentially, with no bound on any individual server's discovery time
//! beyond the two chained 30s `CALL_TIMEOUT`s inside
//! `StdioMcpClient::request` (`initialize` then `tools/list`). A single slow
//! or hung server could cost up to ~60s, and N servers processed one at a
//! time meant an N*60s worst case on every turn — a straightforward DoS
//! against a persona's responsiveness from nothing more than a misbehaving
//! MCP server. What: `discover_one` bounds a single server's discovery to
//! `DISCOVERY_TIMEOUT` (well under the 60s worst case) via
//! `tokio::time::timeout`, and consults/populates a process-wide
//! `SERVER_CACHE` keyed by server name so a spec whose command/args/env are
//! unchanged since the last successful discovery reuses the already-spawned
//! `ServiceClient` and its `tools/list` result instead of respawning and
//! re-listing. Callers (`super::build_executors_from_specs`) run
//! `discover_one` concurrently across all specs via `futures::future::join_all`,
//! so the whole batch's worst case is bounded by `DISCOVERY_TIMEOUT`
//! regardless of server count, not `DISCOVERY_TIMEOUT * N`.
//! Test: `discover_one_reuses_cached_client_across_calls`,
//! `discover_one_returns_none_on_timeout`.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use super::spec::McpServerSpec;
use crate::tools::mcp_service_tools::ServiceClient;

/// Per-server discovery deadline (spawn + `initialize` + `tools/list`
/// combined). Well under the ~60s worst case two chained 30s `CALL_TIMEOUT`s
/// could otherwise cost, and — because discovery now runs concurrently
/// across servers — this is also the effective ceiling for the whole batch.
pub(super) const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a successful discovery result stays valid before the next
/// persona turn re-runs `tools/list` against the (already-spawned) server to
/// pick up any change in the server's advertised tools.
const CACHE_TTL: Duration = Duration::from_secs(120);

/// A cached, already-spawned server plus its last-discovered tool list.
struct CachedServer {
    /// Fingerprint of the spec that produced this entry (command + args +
    /// sorted env). A spec whose fingerprint changes (config edit, `.mcp.json`
    /// edit) invalidates the cache for that server name.
    fingerprint: String,
    client: Arc<ServiceClient>,
    tools: Vec<trusty_common::stdio_mcp_client::McpTool>,
    cached_at: Instant,
}

/// Process-wide cache, keyed by server name. `LazyLock` gives lazy,
/// synchronization-free initialization; the `Mutex` guards concurrent
/// persona turns (and, within a turn, concurrent `discover_one` calls from
/// `join_all`) from racing on inserts.
static SERVER_CACHE: LazyLock<Mutex<HashMap<String, CachedServer>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Build a stable fingerprint for a spec's spawn-relevant fields.
///
/// Why: The cache must invalidate itself when an operator edits a server's
/// command/args/env, not just serve stale spawn parameters forever.
/// What: `command`, `args` (order-sensitive — args order matters to the
/// spawned process), and `env` (sorted by key so map iteration order never
/// causes spurious cache misses) joined into one string.
fn fingerprint(spec: &McpServerSpec) -> String {
    let mut env_pairs: Vec<(&String, &String)> = spec.env.iter().collect();
    env_pairs.sort_by(|a, b| a.0.cmp(b.0));
    format!("{}|{:?}|{:?}", spec.command, spec.args, env_pairs)
}

/// Discover one server's tools: a cache hit reuses the already-spawned
/// `ServiceClient` and prior `tools/list` result; a cache miss (or stale/
/// changed spec) spawns fresh, bounded by `DISCOVERY_TIMEOUT`.
///
/// Why: The single per-spec unit of work `build_executors_from_specs` fans
/// out concurrently — see module docs for the DoS rationale.
/// What: Returns `None` (warn-and-skip, never panics/aborts the batch) on
/// spawn/handshake/list failure OR on exceeding `DISCOVERY_TIMEOUT`.
/// Test: `discover_one_reuses_cached_client_across_calls`,
/// `discover_one_returns_none_on_timeout`.
pub(super) async fn discover_one(
    spec: &McpServerSpec,
) -> Option<(
    Arc<ServiceClient>,
    Vec<trusty_common::stdio_mcp_client::McpTool>,
)> {
    let fp = fingerprint(spec);

    {
        let cache = SERVER_CACHE.lock().await;
        if let Some(entry) = cache.get(&spec.name)
            && entry.fingerprint == fp
            && entry.cached_at.elapsed() < CACHE_TTL
        {
            return Some((entry.client.clone(), entry.tools.clone()));
        }
    }

    let client = Arc::new(ServiceClient::with_parts(
        spec.name.clone(),
        spec.command.clone(),
        spec.args.clone(),
        spec.env.clone(),
    ));
    let tools = match tokio::time::timeout(DISCOVERY_TIMEOUT, client.list_tools()).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            tracing::warn!(
                server = %spec.name,
                source = ?spec.source,
                error = %e,
                "mcp_live: discovery failed (spawn/initialize/tools-list); skipping server"
            );
            return None;
        }
        Err(_) => {
            tracing::warn!(
                server = %spec.name,
                source = ?spec.source,
                timeout_secs = DISCOVERY_TIMEOUT.as_secs(),
                "mcp_live: discovery exceeded per-server deadline; skipping server"
            );
            return None;
        }
    };

    let mut cache = SERVER_CACHE.lock().await;
    cache.insert(
        spec.name.clone(),
        CachedServer {
            fingerprint: fp,
            client: client.clone(),
            tools: tools.clone(),
            cached_at: Instant::now(),
        },
    );

    Some((client, tools))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp_live::spec::SpecSource;
    use std::collections::HashMap as StdHashMap;

    fn fake_spec_with_marker(name: &str, marker_path: &std::path::Path) -> McpServerSpec {
        let script = format!(
            r#"echo spawned >> {marker}
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2024-11-05","serverInfo":{{"name":"fake","version":"0.1.0"}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[]}}}}\n'
      ;;
  esac
done"#,
            marker = marker_path.display()
        );
        McpServerSpec {
            name: name.to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), script],
            env: StdHashMap::new(),
            source: SpecSource::ConfigService,
        }
    }

    /// Why: The whole point of the cache (#3266 DoS fix) — a second call
    /// with an unchanged spec must reuse the already-spawned client rather
    /// than spawning a fresh subprocess, which is what made discovery
    /// expensive to run on every persona turn.
    /// What: Discover the same fake server twice; assert both calls return
    /// the identical `Arc<ServiceClient>` pointer, AND that the underlying
    /// shell script's spawn-marker file has exactly one line (proving only
    /// one subprocess was ever started).
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn discover_one_reuses_cached_client_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.txt");
        let spec = fake_spec_with_marker("cache-test-server-reuse", &marker);

        let first = discover_one(&spec).await.expect("first discovery");
        let second = discover_one(&spec).await.expect("second discovery");

        assert!(
            Arc::ptr_eq(&first.0, &second.0),
            "second call must reuse the cached client instead of spawning a new one"
        );

        tokio::time::sleep(Duration::from_millis(100)).await;
        let contents = std::fs::read_to_string(&marker).unwrap_or_default();
        assert_eq!(
            contents.lines().count(),
            1,
            "expected exactly one subprocess spawn, marker file contained: {contents:?}"
        );
    }

    /// Why: A server whose handshake never responds must not be allowed to
    /// block discovery indefinitely (or for the full chained-60s worst
    /// case) — `DISCOVERY_TIMEOUT` must actually fire.
    /// What: Point a spec at `sleep 3600` (never speaks the MCP protocol);
    /// assert `discover_one` returns `None` well before the OS would kill
    /// the process on its own. Uses a shortened effective wait by relying on
    /// `DISCOVERY_TIMEOUT`'s real value — this test's own timeout wrapper
    /// only guards against the assertion itself hanging in CI.
    /// Test: This test.
    #[tokio::test]
    #[cfg(unix)]
    async fn discover_one_returns_none_on_timeout() {
        let spec = McpServerSpec {
            name: "cache-test-server-hang".to_string(),
            command: "sleep".to_string(),
            args: vec!["3600".to_string()],
            env: StdHashMap::new(),
            source: SpecSource::ConfigService,
        };

        let result = tokio::time::timeout(DISCOVERY_TIMEOUT + Duration::from_secs(10), async {
            discover_one(&spec).await
        })
        .await
        .expect("discover_one must respect DISCOVERY_TIMEOUT, not hang indefinitely");

        assert!(
            result.is_none(),
            "a server that never speaks MCP must be skipped, not treated as discovered"
        );
    }
}
