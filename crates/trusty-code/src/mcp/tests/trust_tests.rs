//! The trust gate over repo-tracked project entries (#5428).

use std::collections::BTreeMap;
use std::path::Path;

use trusty_mcp::config::{McpServerConfig, McpServerOverride, McpTransport};

use super::support::stdio;
use crate::mcp::config::ProjectOverrides;
use crate::mcp::trust::{gate, transport_equivalent};

/// The file the gate names in a refusal. Never read.
fn path() -> &'static Path {
    Path::new("/tmp/project/.trusty-code/mcp.toml")
}

/// A `ProjectOverrides` carrying exactly these sets and disables.
fn overrides(servers: Vec<McpServerConfig>, disabled: &[&str]) -> ProjectOverrides {
    ProjectOverrides {
        servers,
        disabled: disabled.iter().map(|d| d.to_string()).collect(),
    }
}

/// Why: the accept case the whole gate turns on.
#[test]
fn an_identical_stdio_entry_is_equivalent() {
    let a = stdio("x", "/bin/echo", &["--serve"]);
    let b = stdio("x", "/bin/echo", &["--serve"]);
    assert!(transport_equivalent(&a.transport, &b.transport));
}

/// Why: comparing only the command would let a project keep a trusted name
/// and slip an extra argument past — `--eval <payload>` is a command line too.
#[test]
fn a_differing_arg_is_not_equivalent() {
    let a = stdio("x", "/bin/echo", &["--serve"]);
    let b = stdio("x", "/bin/echo", &["--serve", "--eval", "payload"]);
    assert!(!transport_equivalent(&a.transport, &b.transport));
}

/// Why: `env` reaches the child process, so an altered value is an altered
/// program — a changed `PATH` or `NODE_OPTIONS` alone is enough to redirect it.
#[test]
fn a_differing_env_value_is_not_equivalent() {
    let mut a = stdio("x", "/bin/echo", &[]);
    let mut b = stdio("x", "/bin/echo", &[]);
    a.transport = McpTransport::Stdio {
        command: "/bin/echo".to_string(),
        args: Vec::new(),
        env: BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
    };
    b.transport = McpTransport::Stdio {
        command: "/bin/echo".to_string(),
        args: Vec::new(),
        env: BTreeMap::from([("PATH".to_string(), "/tmp/evil".to_string())]),
    };
    assert!(!transport_equivalent(&a.transport, &b.transport));
}

/// Why: the discriminant decides which protocol the client speaks, so two
/// entries at one URL under different transports are not the same connection.
#[test]
fn http_and_sse_at_the_same_url_are_not_equivalent() {
    let http = McpTransport::Http {
        url: "https://example.invalid/mcp".to_string(),
        headers: BTreeMap::new(),
    };
    let sse = McpTransport::Sse {
        url: "https://example.invalid/mcp".to_string(),
        headers: BTreeMap::new(),
    };
    assert!(!transport_equivalent(&http, &sse));
}

/// Why: a content-equivalent `Set` carries no new instruction, so refusing it
/// would block the legitimate "this project uses that server" declaration.
#[test]
fn a_matching_set_is_accepted() {
    let global = vec![stdio("alpha", "/bin/echo", &["--serve"])];
    let project = overrides(vec![stdio("alpha", "/bin/echo", &["--serve"])], &[]);

    let (accepted, refused) = gate(&global, &project, path());

    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(accepted.len(), 1);
    assert!(matches!(accepted[0], McpServerOverride::Set(_)));
}

/// Why: the attack this gate exists for — the same trusted NAME with a
/// different command behind it.
#[test]
fn a_set_with_a_new_command_is_refused() {
    let global = vec![stdio("alpha", "/bin/echo", &[])];
    let project = overrides(vec![stdio("alpha", "/tmp/attacker", &[])], &[]);

    let (accepted, refused) = gate(&global, &project, path());

    assert!(accepted.is_empty(), "nothing reaches the resolver");
    assert_eq!(refused.len(), 1);
    assert!(
        refused[0].detail.contains("alpha"),
        "the finding names the entry: {}",
        refused[0].detail,
    );
    assert!(
        !refused[0].detail.contains("attacker"),
        "the finding must not echo the command back into a log: {}",
        refused[0].detail,
    );
}

/// Why: an entirely new server is the same vector, with no global entry to
/// compare against at all.
#[test]
fn a_set_naming_no_global_server_is_refused() {
    let global = vec![stdio("alpha", "/bin/echo", &[])];
    let project = overrides(vec![stdio("brand-new", "/bin/echo", &[])], &[]);

    let (accepted, refused) = gate(&global, &project, path());

    assert!(accepted.is_empty());
    assert_eq!(refused.len(), 1);
    assert!(refused[0].detail.contains("brand-new"));
}

/// Why: `enabled` is outside the transport, so flipping it is the one thing a
/// content-equivalent `Set` legitimately changes — that is how a project turns
/// a globally-disabled connector back on for itself.
#[test]
fn a_set_may_re_enable_a_disabled_global_entry() {
    let mut off = stdio("alpha", "/bin/echo", &[]);
    off.enabled = false;
    let global = vec![off];
    let project = overrides(vec![stdio("alpha", "/bin/echo", &[])], &[]);

    let (accepted, refused) = gate(&global, &project, path());

    assert!(refused.is_empty(), "{refused:?}");
    match &accepted[0] {
        McpServerOverride::Set(config) => assert!(config.enabled),
        other => panic!("expected a Set, got {other:?}"),
    }
}

/// Why: `extensions` is an uninterpreted map consumers read their own
/// semantics out of (`trusty-agents` takes `auth`, `scopes` and `discover`
/// from it), so a project `Set` that matched on transport could otherwise
/// smuggle arbitrary consumer configuration past a gate that only compared
/// the transport — a second, unchecked channel around the trust boundary.
#[test]
fn a_set_cannot_smuggle_its_own_extensions() {
    let mut global_entry = stdio("alpha", "/bin/echo", &[]);
    global_entry.extensions = BTreeMap::from([("scopes".to_string(), serde_json::json!(["read"]))]);
    let global = vec![global_entry];

    let mut smuggler = stdio("alpha", "/bin/echo", &[]);
    smuggler.extensions = BTreeMap::from([
        ("scopes".to_string(), serde_json::json!(["read", "write"])),
        (
            "auth".to_string(),
            serde_json::json!({"token": "attacker-supplied"}),
        ),
    ]);
    let project = overrides(vec![smuggler], &[]);

    let (accepted, refused) = gate(&global, &project, path());

    assert!(refused.is_empty(), "a transport match is still accepted");
    let McpServerOverride::Set(honoured) = &accepted[0] else {
        panic!("expected a Set, got {:?}", accepted[0]);
    };
    assert_eq!(
        honoured.extensions,
        BTreeMap::from([("scopes".to_string(), serde_json::json!(["read"]))]),
        "the GLOBAL extensions survive; the project's are discarded",
    );

    // And the same holds after resolution, which is what a consumer reads.
    let resolved = trusty_mcp::config::resolve(&global, &accepted);
    assert!(
        !resolved[0].extensions.contains_key("auth"),
        "a project-supplied key must never reach a resolved server: {:?}",
        resolved[0].extensions,
    );
}

/// Why: disabling can only reduce what runs, so it needs no content check —
/// including for a name the global file never defined.
#[test]
fn a_disable_always_passes() {
    let global = vec![stdio("alpha", "/bin/echo", &[])];
    let project = overrides(Vec::new(), &["alpha", "never-configured"]);

    let (accepted, refused) = gate(&global, &project, path());

    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(accepted.len(), 2);
    assert!(
        accepted
            .iter()
            .all(|o| matches!(o, McpServerOverride::Disable { .. }))
    );
}

/// Why: a name in both lists must resolve as the more specific `Set`, which
/// only holds if the disables are emitted first for the resolver's
/// left-to-right fold.
#[test]
fn disables_are_emitted_before_sets() {
    let global = vec![stdio("alpha", "/bin/echo", &[])];
    let project = overrides(vec![stdio("alpha", "/bin/echo", &[])], &["alpha"]);

    let (accepted, _) = gate(&global, &project, path());

    assert!(matches!(accepted[0], McpServerOverride::Disable { .. }));
    assert!(matches!(accepted[1], McpServerOverride::Set(_)));
    let resolved = trusty_mcp::config::resolve(&global, &accepted);
    assert_eq!(resolved.len(), 1, "the Set wins");
}
