//! Role-typed credential references (#8454 S2c): `bot_ref` names the token
//! that posts, `app_ref` Slack's Socket Mode token. Every refusal denies
//! every route and never repeats the value it refused.

use trusty_common::credential_registry::REGISTRY;
use trusty_common::credentials::CredentialRef;

use super::host::assert_host_denies;
use super::{host, input, A_V2, B_V1, HOST_ALL, PROJ_A, PROJ_B};
use crate::policy::host::{allowed_refs, APP_REF, BOT_REF};
use crate::policy::{merge, Channel, HostError, RefFault};

/// [`HOST_ALL`] with the Slack connection replaced by `conn`.
fn slack(conn: &str) -> String {
    let out = HOST_ALL.replace(
        "connection: { bot_ref: slack }",
        &format!("connection: {conn}"),
    );
    assert_ne!(out, HOST_ALL, "fixture moved");
    out
}

/// [`HOST_ALL`] with a Telegram connection `conn` added.
fn telegram(conn: &str) -> String {
    let out = HOST_ALL.replace(
        "  telegram:\n    enabled: true\n",
        &format!("  telegram:\n    enabled: true\n    connection: {conn}\n"),
    );
    assert_ne!(out, HOST_ALL, "fixture moved");
    out
}

/// Assert `yaml` is refused as `is` describes, that neither the error nor
/// any finding holds `secret`, and that the load denies every route.
fn refused(what: &str, yaml: &str, is: impl Fn(&HostError) -> bool, secret: Option<&str>) {
    let result = host(yaml);
    let Err(e) = &result else {
        panic!("{what}: loaded\n{yaml}");
    };
    assert!(is(e), "{what}: wrong error {e:?}");
    if let Some(secret) = secret {
        assert!(
            !format!("{e} {e:?}").contains(secret),
            "{what}: value echoed: {e}"
        );
        let report = merge(result.clone(), vec![input(PROJ_A, A_V2)]);
        for f in &report.findings {
            assert!(!format!("{f} {f:?}").contains(secret), "{what}: {f}");
        }
    }
    assert_host_denies(what, result);
}

fn ref_fault(channel: Channel, key: &str, fault: RefFault) -> impl Fn(&HostError) -> bool + '_ {
    move |e| {
        matches!(e, HostError::CredentialRef { channel: c, key: k, fault: f }
            if *c == channel && *k == key && *f == fault)
    }
}

#[test]
fn slack_bot_and_app_ref_parse() {
    let yaml = slack("{ bot_ref: slack, app_ref: slack-app }");
    let ceiling = host(&yaml).expect("bot and app refs");
    let s = ceiling.channel(Channel::Slack).expect("slack");
    assert_eq!(s.bot_ref().map(|r| r.to_string()).as_deref(), Some("slack"));
    assert_eq!(
        s.app_ref().map(|r| r.to_string()).as_deref(),
        Some("slack-app")
    );

    // Outbound only: app_ref is optional.
    let ceiling = host(HOST_ALL).expect("bot ref only");
    let s = ceiling.channel(Channel::Slack).expect("slack");
    assert_eq!(s.bot_ref().map(|r| r.to_string()).as_deref(), Some("slack"));
    assert!(s.app_ref().is_none());

    let yaml = telegram("{ bot_ref: telegram }");
    let ceiling = host(&yaml).expect("telegram bot ref");
    let t = ceiling.channel(Channel::Telegram).expect("telegram");
    assert_eq!(
        t.bot_ref().map(|r| r.to_string()).as_deref(),
        Some("telegram")
    );
    assert!(t.app_ref().is_none());

    let report = merge(host(&yaml), vec![input(PROJ_A, A_V2), input(PROJ_B, B_V1)]);
    assert!(!report.denied, "{:?}", report.findings);
}

#[test]
fn slack_app_ref_in_bot_slot_refused() {
    let allowed = allowed_refs(Channel::Slack, BOT_REF);
    let fault = RefFault::NotAllowed { allowed };
    for conn in [
        "{ bot_ref: slack-app }",
        "{ bot_ref: slack-app, app_ref: slack-app }",
    ] {
        let is = ref_fault(Channel::Slack, BOT_REF, fault);
        refused(conn, &slack(conn), is, None);
    }
    // The roles do not swap: the bot token is not a Socket Mode token.
    let allowed = allowed_refs(Channel::Slack, APP_REF);
    let is = ref_fault(Channel::Slack, APP_REF, RefFault::NotAllowed { allowed });
    let conn = "{ bot_ref: slack, app_ref: slack }";
    refused(conn, &slack(conn), is, None);
}

#[test]
fn slack_user_ref_refused_in_either_slot() {
    for (key, conn) in [
        (BOT_REF, "{ bot_ref: slack-user }"),
        (APP_REF, "{ bot_ref: slack, app_ref: slack-user }"),
    ] {
        let allowed = allowed_refs(Channel::Slack, key);
        let is = ref_fault(Channel::Slack, key, RefFault::NotAllowed { allowed });
        refused(conn, &slack(conn), is, Some("slack-user"));
    }
}

#[test]
fn telegram_app_ref_refused() {
    for conn in [
        "{ bot_ref: telegram, app_ref: slack-app }",
        "{ bot_ref: telegram, app_ref: telegram }",
    ] {
        let is = ref_fault(Channel::Telegram, APP_REF, RefFault::NoSuchRole);
        refused(conn, &telegram(conn), is, None);
    }
}

#[test]
fn bot_ref_required_when_connection_present() {
    let missing = |channel| move |e: &HostError| *e == HostError::MissingBotRef { channel };
    for conn in ["{ app_ref: slack-app }", "{}", "{ bot_ref: ~ }"] {
        refused(conn, &slack(conn), missing(Channel::Slack), None);
    }
    refused(
        "telegram {}",
        &telegram("{}"),
        missing(Channel::Telegram),
        None,
    );
}

#[test]
fn legacy_credential_ref_key_is_refused_naming_its_replacement() {
    let legacy = |channel| move |e: &HostError| *e == HostError::LegacyCredentialRef { channel };
    for conn in [
        "{ credential_ref: slack }",
        "{ credential_ref: slack, bot_ref: slack }",
        // Present but null is still the old key, never ignored.
        "{ credential_ref: ~, bot_ref: slack }",
        "{ credential_ref: xoxb-1234-5678-AbCdEf, bot_ref: slack }",
    ] {
        refused(conn, &slack(conn), legacy(Channel::Slack), Some("xoxb"));
    }
    let conn = "{ credential_ref: telegram }";
    refused(conn, &telegram(conn), legacy(Channel::Telegram), None);

    let shown = host(&slack("{ credential_ref: slack }"))
        .expect_err("legacy key")
        .to_string();
    assert!(
        shown.contains("credential_ref") && shown.contains("bot_ref") && shown.contains("app_ref"),
        "{shown}"
    );
}

#[test]
fn secret_scheme_and_pasted_token_refused_without_echo() {
    let s_bot = allowed_refs(Channel::Slack, BOT_REF);
    let s_app = allowed_refs(Channel::Slack, APP_REF);
    let t_bot = allowed_refs(Channel::Telegram, BOT_REF);
    let scheme = |allowed| RefFault::SecretScheme { allowed };
    let not_a_name = |allowed| RefFault::NotAName { allowed };
    let not_allowed = |allowed| RefFault::NotAllowed { allowed };
    let cases = [
        // (channel, key, connection, fault, the text that must not echo)
        (
            Channel::Slack,
            BOT_REF,
            "{ bot_ref: 'secret://vault/slack' }",
            scheme(s_bot),
            "vault",
        ),
        (
            Channel::Slack,
            APP_REF,
            "{ bot_ref: slack, app_ref: 'SECRET://vault/slack-app' }",
            scheme(s_app),
            "vault",
        ),
        (
            Channel::Telegram,
            BOT_REF,
            "{ bot_ref: 'secret://vault/telegram' }",
            scheme(t_bot),
            "vault",
        ),
        (
            Channel::Slack,
            BOT_REF,
            "{ bot_ref: xoxb-1234567890-0987654321-AbCdEfGhIjKl }",
            not_a_name(s_bot),
            "AbCdEf",
        ),
        (
            Channel::Slack,
            APP_REF,
            "{ bot_ref: slack, app_ref: xapp-1-A0B1C2D3E4-1234-abcdef }",
            not_a_name(s_app),
            "xapp",
        ),
        (
            Channel::Telegram,
            BOT_REF,
            "{ bot_ref: '123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw' }",
            not_a_name(t_bot),
            "AAHdq",
        ),
        // Inside the grammar, so only the allowlist stops it.
        (
            Channel::Slack,
            BOT_REF,
            "{ bot_ref: xoxb-123-pasted }",
            not_allowed(s_bot),
            "xoxb",
        ),
        (
            Channel::Slack,
            APP_REF,
            "{ bot_ref: slack, app_ref: xapp-123-pasted }",
            not_allowed(s_app),
            "xapp",
        ),
    ];
    for (channel, key, conn, fault, secret) in cases {
        let yaml = match channel {
            Channel::Telegram => telegram(conn),
            _ => slack(conn),
        };
        refused(conn, &yaml, ref_fault(channel, key, fault), Some(secret));
    }
}

#[test]
fn every_allowed_ref_is_a_registry_key() {
    let mut seen = 0;
    for channel in [Channel::Slack, Channel::Telegram, Channel::Gchat] {
        for key in [BOT_REF, APP_REF] {
            for name in allowed_refs(channel, key) {
                seen += 1;
                assert!(
                    REGISTRY.iter().any(|(k, _)| k == name),
                    "{channel}.{key}: {name} is not a registry key"
                );
                // #8454 ruling 2026-09-23: never a user token.
                assert_ne!(*name, "slack-user");
            }
        }
    }
    assert_eq!(seen, 3, "slack bot, slack app, telegram bot");
    // The Socket Mode token cannot post, so it never fills bot_ref.
    assert!(!allowed_refs(Channel::Slack, BOT_REF).contains(&"slack-app"));
    assert!(allowed_refs(Channel::Telegram, APP_REF).is_empty());
    assert!(allowed_refs(Channel::Gchat, BOT_REF).is_empty());
}

#[test]
fn ref_names_round_trip_through_credential_ref_parse() {
    for channel in [Channel::Slack, Channel::Telegram] {
        for key in [BOT_REF, APP_REF] {
            for name in allowed_refs(channel, key) {
                let parsed = CredentialRef::parse(name).expect("a provider key");
                assert_eq!(parsed.to_string(), *name);
                assert_eq!(parsed.provider(), *name);
                assert!(parsed.qualifier().is_none());
            }
        }
    }
}
