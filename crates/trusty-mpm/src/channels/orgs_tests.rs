//! Tests for `orgs.rs` (#10417).

use super::orgs::*;

/// The S1 initial config: duetto on Slack, bta on Google Chat.
pub(crate) const INITIAL: &str = r#"
orgs:
  duetto:
    kind: slack
    workspace: T0TEST001
    credentials:
      bot_token: secret://trusty/duettoresearch/SLACK.SLACK_MPM_BOT_OAUTH_TOKEN
  bta:
    kind: gchat
    project: agentic-crm-508715
    service_account: ai-chat-bot
    space: AAQAee6_DV8
    credentials:
      sa_key: secret://trusty/bta/GCHAT.AI_CHAT_BOT_SA_KEY
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
    assert_eq!(bta.space.as_deref(), Some("AAQAee6_DV8"));
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
