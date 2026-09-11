//! `crate::mcp::extensions` — the typed accessors over `extensions` (#7454).

use serial_test::serial;
use trusty_mcp::config::McpTransport;

use super::stdio;
use crate::mcp::extensions::{self, AuthKind, AuthSpec, DriverKind, ToolDescriptor};

#[test]
fn description_defaults_to_empty() {
    let server = stdio("plain", "plain-bin");
    assert_eq!(extensions::description(&server), "");
}

#[test]
fn tools_round_trip() {
    let mut server = stdio("granola", "granola-mcp");
    let declared = vec![ToolDescriptor {
        name: "granola_search".into(),
        description: "search notes".into(),
    }];
    extensions::set(&mut server.extensions, extensions::TOOLS, &declared);
    assert_eq!(extensions::tools(&server), declared);
}

#[test]
fn discover_defaults_to_false() {
    let mut server = stdio("d", "d-bin");
    assert!(!extensions::discover(&server));
    extensions::set(&mut server.extensions, extensions::DISCOVER, &true);
    assert!(extensions::discover(&server));
}

#[test]
fn scopes_round_trip() {
    let mut server = stdio("gworkspace", "trusty-gworkspace-mcp");
    let scopes = vec!["google.gmail.*".to_string()];
    extensions::set(&mut server.extensions, extensions::SCOPES, &scopes);
    assert_eq!(extensions::scopes(&server), scopes);
}

/// A nonsense value must not drop the server — one bad optional knob is not a
/// reason to lose a connector, so the documented default applies instead.
#[test]
fn discovery_ttl_falls_back_to_the_documented_default() {
    let mut server = stdio("ttl", "ttl-bin");
    assert_eq!(
        extensions::discovery_ttl_secs(&server),
        extensions::DEFAULT_DISCOVERY_TTL_SECS
    );
    server
        .extensions
        .insert(extensions::DISCOVERY_TTL_SECS.into(), "soon".into());
    assert_eq!(
        extensions::discovery_ttl_secs(&server),
        extensions::DEFAULT_DISCOVERY_TTL_SECS
    );
    extensions::set(
        &mut server.extensions,
        extensions::DISCOVERY_TTL_SECS,
        &42u64,
    );
    assert_eq!(extensions::discovery_ttl_secs(&server), 42);
}

#[test]
fn driver_defaults_to_none() {
    let mut server = stdio("plain", "plain-bin");
    assert_eq!(extensions::driver(&server), None);
    assert!(!extensions::eager_discovery(&server));
    extensions::set(
        &mut server.extensions,
        extensions::DRIVER,
        &DriverKind::StdioMcp,
    );
    assert_eq!(extensions::driver(&server), Some(DriverKind::StdioMcp));
}

#[test]
fn transport_limits_default_to_unset() {
    let server = stdio("limits", "limits-bin");
    let limits = extensions::transport_limits(&server);
    assert_eq!(limits.timeout_ms, None);
    assert_eq!(limits.max_concurrency, None);
}

/// `McpTransport` is `#[non_exhaustive]`, so the accessors answer for the
/// variant they know and `None` for everything else rather than matching
/// exhaustively.
#[test]
fn stdio_parts_only_answer_for_stdio() {
    let local = stdio("local", "local-bin");
    assert!(extensions::stdio_parts(&local).is_some());
    assert_eq!(extensions::endpoint_url(&local), None);
    assert_eq!(extensions::transport_label(&local), "stdio");

    let mut remote = local.clone();
    remote.transport = McpTransport::Http {
        url: "https://example.com/mcp".into(),
        headers: Default::default(),
    };
    assert!(extensions::stdio_parts(&remote).is_none());
    assert_eq!(
        extensions::endpoint_url(&remote),
        Some("https://example.com/mcp")
    );
    assert_eq!(extensions::transport_label(&remote), "http");
}

/// #7454: the credential check ADR-0060 decision 6 requires. `#[file_serial]`
/// rather than `#[serial]` because nextest gives every test its own PROCESS
/// (#4162) and the env var is process-global within this one.
#[test]
#[serial(mcp_auth_env)]
fn auth_resolves_from_the_environment() {
    let mut server = stdio("bearer", "bearer-bin");
    extensions::set(
        &mut server.extensions,
        extensions::AUTH,
        &AuthSpec {
            kind: AuthKind::BearerEnv,
            env: Some("TRUSTY_TEST_MCP_TOKEN_7454".into()),
            header: None,
        },
    );
    unsafe {
        std::env::set_var("TRUSTY_TEST_MCP_TOKEN_7454", "abc123");
    }
    let resolved = extensions::resolve_auth(&server).unwrap();
    assert_eq!(
        resolved,
        Some(("Authorization".to_string(), "Bearer abc123".to_string()))
    );
    unsafe {
        std::env::remove_var("TRUSTY_TEST_MCP_TOKEN_7454");
    }
}

/// The reason names the VARIABLE, never the value — it is the only actionable
/// thing to tell a user, and the secret must not reach a status string.
#[test]
#[serial(mcp_auth_env)]
fn a_missing_credential_names_the_variable() {
    let mut server = stdio("bearer", "bearer-bin");
    extensions::set(
        &mut server.extensions,
        extensions::AUTH,
        &AuthSpec {
            kind: AuthKind::BearerEnv,
            env: Some("TRUSTY_TEST_MCP_ABSENT_7454".into()),
            header: None,
        },
    );
    unsafe {
        std::env::remove_var("TRUSTY_TEST_MCP_ABSENT_7454");
    }
    let reason = extensions::resolve_auth(&server).unwrap_err();
    assert!(
        reason.contains("TRUSTY_TEST_MCP_ABSENT_7454"),
        "got: {reason}"
    );
}

/// A server declaring no `auth` at all resolves cleanly — the credential check
/// must not turn every ordinary connector into a skipped one.
#[test]
fn no_auth_block_resolves_to_no_credential() {
    let server = stdio("plain", "plain-bin");
    assert_eq!(extensions::resolve_auth(&server).unwrap(), None);
}

/// #7454 (g1 follow-up): an absent optional field serialises ABSENT, not as
/// JSON `null` — the shared file's writer rejects nulls inside `extensions`.
#[test]
fn an_absent_auth_field_serialises_absent_not_null() {
    let spec = AuthSpec {
        kind: AuthKind::None,
        env: None,
        header: None,
    };
    let json = serde_json::to_value(&spec).unwrap();
    assert_eq!(json, serde_json::json!({"kind": "none"}));
}
