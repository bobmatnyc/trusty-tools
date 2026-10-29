//! Tests for `orgs.rs` (#10417).

use super::orgs::*;

/// The S1 initial config: duetto on Slack, bta on Google Chat.
pub(crate) const INITIAL: &str = r#"
orgs:
  duetto:
    kind: slack
    workspace: T0TEST001
    secrets: duettoresearch
    credentials:
      bot_token: secret://duettoresearch/SLACK.SLACK_MPM_BOT_OAUTH_TOKEN
  bta:
    kind: gchat
    project: agentic-crm-508715
    service_account: ai-chat-bot
    space: AAAATEST001
    secrets: Travel-BTA
    credentials:
      sa_key: secret://Travel-BTA/GCHAT.AI_CHAT_BOT_SA_KEY
users:
  bob-duetto:
    org: duetto
    address: U0OWNER01
  owner@example.com:
    org: bta
    address: owner@example.com
owner:
  slack_org: duetto
  slack_user: U0OWNER01
"#;

#[test]
fn initial_config_parses() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let duetto = cfg.org("duetto").expect("duetto");
    assert_eq!(duetto.kind, ChannelKind::Slack);
    assert_eq!(duetto.workspace.as_deref(), Some("T0TEST001"));
    let bta = cfg.org("bta").expect("bta");
    assert_eq!(bta.kind, ChannelKind::Gchat);
    assert_eq!(bta.space.as_deref(), Some("AAAATEST001"));
}

#[test]
fn cross_org_send_is_refused() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let err = cfg
        .route("owner@example.com", Some("duetto"))
        .expect_err("refused");
    assert_eq!(err.code(), "cross_org");
    let ok = cfg.route("owner@example.com", None).expect("own org");
    assert_eq!((ok.org.as_str(), ok.kind), ("bta", ChannelKind::Gchat));
}

#[test]
fn unknown_user_has_no_route() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    assert_eq!(cfg.route("nobody", None), Err(RouteError::UnknownUser));
}

#[test]
fn either_org_binds_to_either_kind() {
    // The initial binding swapped: duetto on gchat, bta on slack.
    let swapped = r#"
orgs:
  duetto: { kind: gchat, space: S1 }
  bta: { kind: slack, workspace: T1 }
users:
  other@example.com: { org: duetto, address: other@example.com }
  b: { org: bta, address: U0B }
"#;
    let cfg = OrgsConfig::parse(swapped).expect("parses");
    assert_eq!(
        cfg.route("other@example.com", None).expect("route").kind,
        ChannelKind::Gchat
    );
    assert_eq!(
        cfg.route("b", None).expect("route").kind,
        ChannelKind::Slack
    );
}

#[test]
fn non_secret_credential_is_refused() {
    let text =
        "orgs:\n  x:\n    kind: slack\n    credentials:\n      bot_token: xoxb-literal-value\n";
    let err = OrgsConfig::parse(text).expect_err("refused");
    assert!(matches!(err, ConfigError::NotSecretRef { .. }));
    assert!(!err.to_string().contains("xoxb"), "no value echoed: {err}");
}

#[test]
fn user_with_unknown_org_is_refused() {
    let text = "orgs: {}\nusers:\n  u: { org: ghost, address: U1 }\n";
    assert!(matches!(
        OrgsConfig::parse(text),
        Err(ConfigError::UnknownOrg { .. })
    ));
}

#[test]
fn address_must_fit_the_org_kind() {
    let text = "orgs:\n  d: { kind: slack }\nusers:\n  u: { org: d, address: third@example.com }\n";
    assert!(matches!(
        OrgsConfig::parse(text),
        Err(ConfigError::AddressKind { .. })
    ));
}

#[test]
fn approvals_go_to_the_owner_dm_for_every_org() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let target = cfg.approval_target();
    assert_eq!(
        target,
        ApprovalTarget::OwnerSlackDm {
            org: "duetto".into(),
            user: "U0OWNER01".into()
        }
    );
    let mut no_owner = cfg.clone();
    no_owner.owner = None;
    assert_eq!(no_owner.approval_target(), ApprovalTarget::Dashboard);
}

/// The daemon's `orgs.toml` form of [`INITIAL`] (#10423).
pub(crate) const INITIAL_TOML: &str = r#"
[orgs.duetto]
kind = "slack"
workspace = "T0TEST001"
secrets = "duettoresearch"
credentials = { bot_token = "secret://duettoresearch/SLACK.SLACK_MPM_BOT_OAUTH_TOKEN" }

[orgs.bta]
kind = "gchat"
project = "agentic-crm-508715"
service_account = "ai-chat-bot"
space = "AAAATEST001"
secrets = "Travel-BTA"
credentials = { sa_key = "secret://Travel-BTA/GCHAT.AI_CHAT_BOT_SA_KEY" }

[users.bob-duetto]
org = "duetto"
address = "U0OWNER01"

[users."owner@example.com"]
org = "bta"
address = "owner@example.com"

[owner]
slack_org = "duetto"
slack_user = "U0OWNER01"
"#;

/// #10423: `orgs.toml` holds the same schema as the YAML form.
#[test]
fn toml_config_parses_like_yaml() {
    assert_eq!(
        OrgsConfig::parse_toml(INITIAL_TOML),
        OrgsConfig::parse(INITIAL)
    );
    let err = OrgsConfig::parse_toml("[orgs.x]\nkind = \"pigeon\"\n").expect_err("refused");
    assert!(matches!(err, ConfigError::Malformed(_)), "{err}");
    assert!(
        !err.to_string().contains("pigeon"),
        "no value echoed: {err}"
    );
}

/// #10423: DOC-74 §15.3 — a ref must stay in the org's declared lookup
/// order; `secret://KEY` resolves through it, and a foreign vault, a bad
/// ref or a missing `secrets` declaration is refused at load.
#[test]
fn credential_ref_outside_the_org_scope_is_refused() {
    let doc = |scope: &str, r: &str| {
        format!("orgs:\n  x:\n    kind: slack\n{scope}    credentials:\n      bot_token: \"{r}\"\n")
    };
    let s = "    secrets: duettoresearch/mpm\n";
    for ok in [
        "secret://SLACK_BOT",
        "secret://duettoresearch/SLACK_BOT",
        "secret://duettoresearch/mpm/SLACK_BOT",
    ] {
        OrgsConfig::parse(&doc(s, ok)).expect(ok);
    }
    for (scope, bad, code) in [
        (s, "secret://trusty/duettoresearch/X", "vault_out_of_scope"),
        (s, "secret://victim/prod/SLACK_BOT", "vault_out_of_scope"),
        (s, "secret://a b/KEY", "invalid_ref"),
        (s, "secret://", "invalid_ref"),
        ("", "secret://duettoresearch/SLACK_BOT", "no_secret_scope"),
    ] {
        let err = OrgsConfig::parse(&doc(scope, bad)).expect_err(bad);
        assert!(
            matches!(err, ConfigError::NotSecretRef { code: c, .. } if c == code),
            "{bad}: {err}"
        );
    }
}
