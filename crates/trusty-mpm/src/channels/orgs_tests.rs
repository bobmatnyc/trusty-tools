//! Tests for `orgs.rs` (#10417).

use super::orgs::*;

/// The S1 initial config: acme on Slack, globex on Google Chat.
pub(crate) const INITIAL: &str = r#"
orgs:
  acme:
    kind: slack
    workspace: T0TEST
    credentials:
      bot_token: secret://trusty/acme/SLACK.SLACK_MPM_BOT_OAUTH_TOKEN
  globex:
    kind: gchat
    project: test-project-001
    service_account: ai-chat-bot
    space: AAAATEST001
    credentials:
      sa_key: secret://trusty/globex/GCHAT.AI_CHAT_BOT_SA_KEY
users:
  bob-acme:
    org: acme
    address: U0OWNER
  owner@example.com:
    org: globex
    address: owner@example.com
owner:
  slack_org: acme
  slack_user: U0OWNER
"#;

#[test]
fn initial_config_parses() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let acme = cfg.org("acme").expect("acme");
    assert_eq!(acme.kind, ChannelKind::Slack);
    assert_eq!(acme.workspace.as_deref(), Some("T0TEST"));
    let globex = cfg.org("globex").expect("globex");
    assert_eq!(globex.kind, ChannelKind::Gchat);
    assert_eq!(globex.space.as_deref(), Some("AAAATEST001"));
}

#[test]
fn cross_org_send_is_refused() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let err = cfg
        .route("owner@example.com", Some("acme"))
        .expect_err("refused");
    assert_eq!(err.code(), "cross_org");
    let ok = cfg.route("owner@example.com", None).expect("own org");
    assert_eq!((ok.org.as_str(), ok.kind), ("globex", ChannelKind::Gchat));
}

#[test]
fn unknown_user_has_no_route() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    assert_eq!(cfg.route("nobody", None), Err(RouteError::UnknownUser));
}

#[test]
fn either_org_binds_to_either_kind() {
    // The initial binding swapped: acme on gchat, globex on slack.
    let swapped = r#"
orgs:
  acme: { kind: gchat, space: S1 }
  globex: { kind: slack, workspace: T1 }
users:
  other@example.com: { org: acme, address: other@example.com }
  b: { org: globex, address: U0B }
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
            org: "acme".into(),
            user: "U0OWNER".into()
        }
    );
    let mut no_owner = cfg.clone();
    no_owner.owner = None;
    assert_eq!(no_owner.approval_target(), ApprovalTarget::Dashboard);
}
