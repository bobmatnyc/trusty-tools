//! `crate::mcp::shared::migrate` — the one-time drain of the retired tables (#7454).

use trusty_mcp::config::McpConfigFile;

use super::tempdir;
use crate::mcp::extensions::{self, AuthKind, DriverKind};
use crate::mcp::shared::migrate;

const LEGACY: &str = r#"
[mcp]
inject_for_roles = ["ctrl"]

[[mcp.services]]
name = "granola-notes"
description = "Granola meeting notes"
command = "/opt/homebrew/bin/granola-mcp"
args = ["--quiet"]
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "granola_search"
description = "Search Granola notes"

[[mcp.services]]
name = "trusty-memory"
description = "Trusty memory service"
command = "trusty-memory"
args = ["serve"]
transport = "stdio"
enabled = true
discover = true

[[mcp.services]]
name = "duetto-memory"
description = "Duetto org memory"
url = "https://example.invalid/memory/mcp"
transport = "http"
enabled = false

[tool_registry]
scope_enforcement = "deny"

[[tool_registry.endpoints]]
name = "gworkspace"
driver = "direct"
description = "Google Workspace"
command = "trusty-gworkspace-mcp"
args = []
enabled = true
scopes = ["google.gmail.*"]
discovery_ttl_secs = 0
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000
"#;

#[test]
fn converts_a_legacy_stdio_service() {
    let (servers, _) = migrate::convert(LEGACY);
    let granola = servers
        .iter()
        .find(|s| s.name == "granola-notes")
        .expect("granola migrated");
    let (command, args, _) = extensions::stdio_parts(granola).expect("stdio");
    assert_eq!(command, "/opt/homebrew/bin/granola-mcp");
    assert_eq!(args, ["--quiet"]);
    assert_eq!(extensions::description(granola), "Granola meeting notes");
    assert_eq!(extensions::tools(granola).len(), 1);
    assert!(!extensions::discover(granola));

    let memory = servers
        .iter()
        .find(|s| s.name == "trusty-memory")
        .expect("trusty-memory migrated");
    assert!(extensions::discover(memory));
}

/// A `transport = "http"` service keeps its URL and its disabled flag — a
/// migration that silently re-enabled a connector the operator turned off
/// would be worse than not migrating it.
#[test]
fn converts_a_legacy_http_service() {
    let (servers, _) = migrate::convert(LEGACY);
    let duetto = servers
        .iter()
        .find(|s| s.name == "duetto-memory")
        .expect("duetto migrated");
    assert!(!duetto.enabled);
    assert_eq!(
        extensions::endpoint_url(duetto),
        Some("https://example.invalid/memory/mcp")
    );
}

#[test]
fn converts_a_legacy_registry_endpoint() {
    let (servers, report) = migrate::convert(LEGACY);
    let gw = servers
        .iter()
        .find(|s| s.name == "gworkspace")
        .expect("gworkspace migrated");
    assert_eq!(extensions::driver(gw), Some(DriverKind::Direct));
    assert_eq!(extensions::scopes(gw), ["google.gmail.*"]);
    assert_eq!(extensions::discovery_ttl_secs(gw), 0);
    assert!(extensions::eager_discovery(gw));
    assert_eq!(extensions::transport_limits(gw).timeout_ms, Some(5000));
    assert_eq!(report.endpoints, ["gworkspace"]);
}

/// An `auth` block survives with the credential REFERENCE intact — #7454 moves
/// the shape, not the secret handling (#4568 owns that).
#[test]
fn converts_a_legacy_endpoint_auth_block() {
    let legacy = r#"
[[tool_registry.endpoints]]
name = "paid"
driver = "direct"
command = "paid-rpc"

[tool_registry.endpoints.auth]
kind = "bearer-env"
env = "PAID_TOKEN"
"#;
    let (servers, _) = migrate::convert(legacy);
    let auth = extensions::auth(&servers[0]).expect("auth migrated");
    assert_eq!(auth.kind, AuthKind::BearerEnv);
    assert_eq!(auth.env.as_deref(), Some("PAID_TOKEN"));
}

/// The shared file rejects a duplicate name, so a service and an endpoint
/// claiming the same one cannot both survive; the service wins because it is
/// the entry the runtime actually spawned.
#[test]
fn a_service_wins_a_name_collision() {
    let legacy = r#"
[[mcp.services]]
name = "both"
description = "the service"
command = "svc-bin"
transport = "stdio"

[[tool_registry.endpoints]]
name = "both"
driver = "direct"
command = "endpoint-bin"
"#;
    let (servers, report) = migrate::convert(legacy);
    assert_eq!(servers.len(), 1);
    assert_eq!(extensions::description(&servers[0]), "the service");
    assert!(report.endpoints.is_empty());
}

#[test]
fn migration_reports_what_moved() {
    let (_, report) = migrate::convert(LEGACY);
    assert_eq!(
        report.services,
        ["granola-notes", "trusty-memory", "duetto-memory"]
    );
    assert!(report.summary().contains("gworkspace"));
    assert!(!report.is_empty());
}

/// A legacy file that does not parse drains nothing rather than failing — this
/// runs on a load path, and the old world's broken file is not this release's
/// problem to fail on.
#[test]
fn a_malformed_legacy_file_moves_nothing() {
    let (servers, report) = migrate::convert("[[mcp.services]\nname = ");
    assert!(servers.is_empty());
    assert!(report.is_empty());
}

/// The guard is the shared file's ABSENCE, so the migration runs exactly once
/// and a later hand-edit of the shared file is never overwritten.
#[test]
fn migrates_once_and_never_again() {
    let dir = tempdir("migrate-once");
    let legacy = dir.join("config.toml");
    let shared = dir.join("servers.toml");
    std::fs::write(&legacy, LEGACY).unwrap();

    let first = migrate::migrate_if_absent(&legacy, &shared).expect("first run migrates");
    assert_eq!(first.services.len(), 3);
    assert!(shared.exists());

    // A user edits the shared file; the second run must leave it alone.
    let mut file = McpConfigFile::load(&shared).unwrap();
    file.servers.retain(|s| s.name != "granola-notes");
    file.save(&shared).unwrap();

    assert!(migrate::migrate_if_absent(&legacy, &shared).is_none());
    let after = McpConfigFile::load(&shared).unwrap();
    assert!(!after.servers.iter().any(|s| s.name == "granola-notes"));
}

#[test]
fn an_existing_shared_file_is_left_alone() {
    let dir = tempdir("migrate-existing");
    let legacy = dir.join("config.toml");
    let shared = dir.join("servers.toml");
    std::fs::write(&legacy, LEGACY).unwrap();
    McpConfigFile::new(vec![super::stdio("mine", "mine-bin")])
        .save(&shared)
        .unwrap();

    assert!(migrate::migrate_if_absent(&legacy, &shared).is_none());
    let after = McpConfigFile::load(&shared).unwrap();
    assert_eq!(after.servers.len(), 1);
    assert_eq!(after.servers[0].name, "mine");
}

/// #7454: the absence guard has to be read INSIDE the cross-process lock.
/// Every mutation of the shared file (`mcp_add` and friends, through
/// `crate::tools::mcp_tools::dispatch::mutate`) holds that lock across its own
/// read-modify-write, so a migration that decides from `shared.exists()`
/// beforehand can be overtaken: the add creates the file and adds a server,
/// and the lock-free migration write then publishes the legacy set over the
/// top of it.
///
/// The main thread models that add — it holds the same lock through
/// `state_writer::atomic_update` and publishes one added server — while the
/// migration races it on another thread. Two assertions catch the
/// check-then-act shape whichever way the two interleave: the migration either
/// writes while the lock is held, or it clobbers the published add afterwards.
#[test]
fn migration_waits_for_the_shared_file_lock() {
    let dir = tempdir("migrate-lock");
    let legacy = dir.join("config.toml");
    let shared = dir.join("servers.toml");
    std::fs::write(&legacy, LEGACY).unwrap();

    let added = McpConfigFile::new(vec![super::stdio("added-by-mcp-add", "add-bin")])
        .render(&shared)
        .unwrap();
    let racing_legacy = legacy.clone();
    let racing_shared = shared.clone();
    let mut migration = None;

    crate::state_writer::atomic_update(&shared, |existing| {
        assert!(existing.is_none(), "the shared file must start absent");
        migration = Some(std::thread::spawn(move || {
            migrate::migrate_if_absent(&racing_legacy, &racing_shared)
        }));
        // Long enough for that thread to start and reach its decision; a
        // lock-free migration publishes within microseconds of reaching it.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !shared.exists(),
            "the migration wrote the shared file while another writer held the lock"
        );
        Ok(Some(added.into_bytes()))
    })
    .unwrap();

    assert!(
        migration
            .expect("thread spawned")
            .join()
            .expect("joined")
            .is_none(),
        "the file existed when the lock was granted, so nothing may be migrated"
    );
    let after = McpConfigFile::load(&shared).unwrap();
    let names: Vec<&str> = after.servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["added-by-mcp-add"],
        "lost update: the migration overwrote a concurrent add"
    );
}

/// A `config.toml` with neither retired table is not a migration — writing an
/// empty shared file would make the absence-guard lie on the next run.
#[test]
fn nothing_to_move_writes_no_file() {
    let dir = tempdir("migrate-nothing");
    let legacy = dir.join("config.toml");
    let shared = dir.join("servers.toml");
    std::fs::write(&legacy, "[mcp]\ninject_for_roles = [\"ctrl\"]\n").unwrap();

    assert!(migrate::migrate_if_absent(&legacy, &shared).is_none());
    assert!(!shared.exists());
}
