//! Tests for [`super`] — the exactly-one-wake selector (#7609 slice 4).

use super::*;
use crate::listeners::config::AgentBindingFilter;

/// The live global gmail channel's shape: account-wide, INBOX-scoped ingest,
/// the `bob-personal` identity.
fn global_gmail(route_to: Vec<&str>) -> Channel {
    Channel {
        id: "gmail-personal".into(),
        name: "gmail-personal".into(),
        provider: "gmail".into(),
        scope: ChannelScope::Global,
        enabled: true,
        receive_enabled: true,
        credential_ref: Some("gmail/bob-personal".into()),
        ingest_filter: crate::listeners::config::ListenerFilter {
            label_ids: vec!["INBOX".into()],
        },
        route_to: route_to.into_iter().map(str::to_string).collect(),
        ..Channel::default()
    }
}

/// Izzie's absorbed `[[listeners]]` binding: no destination of its own, the
/// per-assistant wake filter laid over the global.
fn izzie_overlay() -> Channel {
    Channel {
        id: "gmail-personal".into(),
        name: "gmail-personal".into(),
        provider: String::new(),
        scope: ChannelScope::Assistant,
        enabled: true,
        receive_enabled: true,
        instructions: "Triage the mail.".into(),
        event_types: vec!["message.received".into()],
        wake_filter: AgentBindingFilter {
            from: vec!["*".into()],
            exclude_labels: vec!["CATEGORY_PROMOTIONS".into()],
            ..AgentBindingFilter::default()
        },
        ..Channel::default()
    }
}

/// `Binding` carries no `PartialEq`, so candidates are compared by id.
fn ids(pair: (Option<Binding>, Option<Binding>)) -> (Option<String>, Option<String>) {
    (pair.0.map(|b| b.id), pair.1.map(|b| b.id))
}

fn mail(labels: &[&str]) -> StoredEvent {
    StoredEvent {
        id: "gmail-personal:19abc".into(),
        listener_id: "gmail-personal".into(),
        provider: "gmail".into(),
        event_type: "message.received".into(),
        ts: "2026-09-14T00:00:00Z".into(),
        from: Some("Alice <alice@example.com>".into()),
        subject: Some("Dinner".into()),
        snippet: Some("Are we still on?".into()),
        included: true,
        labels: labels.iter().map(|l| (*l).to_string()).collect(),
    }
}

/// Every combination of the three sources answers exactly ONE variant, and
/// the precedence is (1) then (2) then (3).
///
/// Why: this is the whole exactly-one-wake guarantee. Without it the rule is a
/// reading of `if`s spread across the inbound loop, and "assistant channel and
/// routed global both matched" has no stated answer at all.
#[test]
fn the_three_sources_select_exactly_one_wake() {
    let table = [
        (false, false, false, WakeSource::NoWake),
        (false, false, true, WakeSource::LegacyBinding),
        (false, true, false, WakeSource::GlobalRouteTo),
        // The (2)-excludes-(3) rule: a routed global wins over the legacy
        // binding it was backfilled from, so one intent never wakes twice.
        (false, true, true, WakeSource::GlobalRouteTo),
        (true, false, false, WakeSource::AssistantChannel),
        (true, false, true, WakeSource::AssistantChannel),
        (true, true, false, WakeSource::AssistantChannel),
        (true, true, true, WakeSource::AssistantChannel),
    ];
    for (assistant, routed, legacy, expected) in table {
        assert_eq!(
            select_source(assistant, routed, legacy),
            expected,
            "({assistant}, {routed}, {legacy})"
        );
    }
}

/// A global channel naming the assistant in `route_to` wakes it, with the
/// global's own filters when the assistant declared none.
#[test]
fn a_routed_global_wakes_the_named_assistant() {
    let globals = vec![global_gmail(vec!["izzie"])];
    let (routed, legacy) = route_sources(&globals, &[], "izzie", &mail(&["INBOX"]));
    assert!(legacy.is_none());
    let routed = routed.expect("route_to names izzie");
    assert_eq!(routed.id, "gmail-personal");
    assert_eq!(
        routed.provider, "gworkspace",
        "the wake dispatches through the adapter that addresses gmail events"
    );
    assert!(
        routed.credential_ref.is_none(),
        "an assistant-side channel with no destination never sends"
    );

    // An assistant the global does not name gets nothing, and is never
    // broadcast to.
    assert_eq!(
        ids(route_sources(
            &globals,
            &[],
            "cto-assistant",
            &mail(&["INBOX"])
        )),
        (None, None)
    );
}

/// The legacy absorbed binding still wakes while `route_to` is empty.
#[test]
fn a_legacy_overlay_wakes_without_route_to() {
    let globals = vec![global_gmail(vec![])];
    let own = vec![izzie_overlay()];
    let (routed, legacy) = route_sources(&globals, &own, "izzie", &mail(&["INBOX"]));
    assert!(routed.is_none());
    let legacy = legacy.expect("the absorbed binding still matches by listener name");
    assert_eq!(legacy.instructions, "Triage the mail.");
    assert_eq!(legacy.provider, "gworkspace");
}

/// Once `route_to` names the assistant, its legacy binding for the SAME global
/// must not also fire — the pair moved from source (3) to source (2).
///
/// Why: this is the shape the `route_to` backfill creates on a live host, and
/// the two sources describe one operator intent. Two candidates here means two
/// wakes for one message.
#[test]
fn a_routed_global_excludes_the_legacy_binding_for_the_same_pair() {
    let globals = vec![global_gmail(vec!["izzie"])];
    let own = vec![izzie_overlay()];
    let (routed, legacy) = route_sources(&globals, &own, "izzie", &mail(&["INBOX"]));
    assert!(routed.is_some(), "the routed global is the one that wakes");
    assert!(
        legacy.is_none(),
        "the legacy binding must not ALSO wake for the same (global, assistant) pair"
    );
    assert_eq!(
        routed.expect("routed").instructions,
        "Triage the mail.",
        "the overlay's instructions still apply under route_to"
    );
}

/// The overlay's own filter narrows the global it covers, and the same filter
/// rejecting the event yields no candidate at all.
#[test]
fn an_overlay_filter_narrows_the_global_it_covers() {
    let own = vec![izzie_overlay()];
    for globals in [
        vec![global_gmail(vec!["izzie"])],
        vec![global_gmail(vec![])],
    ] {
        let promo = mail(&["INBOX", "CATEGORY_PROMOTIONS"]);
        assert_eq!(
            ids(route_sources(&globals, &own, "izzie", &promo)),
            (None, None),
            "an excluded label wakes nobody, routed or not"
        );
        let wrong_type = StoredEvent {
            event_type: "message.sent".into(),
            ..mail(&["INBOX"])
        };
        assert_eq!(
            ids(route_sources(&globals, &own, "izzie", &wrong_type)),
            (None, None),
            "the overlay's event_types still apply"
        );
    }
}

/// A channel whose provider this build carries no adapter for claims nothing,
/// so the event stays on the path it was already taking.
#[test]
fn an_unknown_provider_claims_nothing() {
    let mut global = global_gmail(vec!["izzie"]);
    global.provider = "notion".into();
    assert_eq!(
        ids(route_sources(&[global], &[], "izzie", &mail(&["INBOX"]))),
        (None, None)
    );
    // An absorbed binding whose global this host does not declare cannot
    // resolve a provider either: it claims nothing and the `[[listeners]]`
    // wake still applies.
    assert_eq!(
        ids(route_sources(
            &[],
            &[izzie_overlay()],
            "izzie",
            &mail(&["INBOX"])
        )),
        (None, None)
    );
}

/// A disabled overlay wakes nobody even when the global routes to it.
#[test]
fn a_disabled_overlay_wakes_nobody() {
    let globals = vec![global_gmail(vec!["izzie"])];
    let own = vec![Channel {
        enabled: false,
        ..izzie_overlay()
    }];
    assert_eq!(
        ids(route_sources(&globals, &own, "izzie", &mail(&["INBOX"]))),
        (None, None)
    );
}

/// `route_to` naming an assistant this host does not have is reported, so a
/// typo is visible instead of reading as "nobody is bound".
#[test]
fn an_unknown_route_to_name_is_reported() {
    let globals = vec![global_gmail(vec!["izzie", "izzy"])];
    let known = vec!["izzie".to_string(), "cto-assistant".to_string()];
    assert_eq!(
        unknown_routes(&globals, &known),
        vec![("gmail-personal", "izzy")]
    );
    assert!(unknown_routes(&globals, &["izzie".into(), "izzy".into()]).is_empty());
}

/// An overlay of a global channel is storable; a blank target with no global
/// of that name is still refused, with the message it always had.
#[test]
fn an_overlay_of_a_global_channel_validates() {
    let globals = vec![global_gmail(vec![])];
    let mut channel = izzie_overlay();
    channel.provider = "gworkspace".into();
    let record = Binding::from(&channel);
    assert!(
        record.validate_in(&globals).is_ok(),
        "the absorbed binding must be storable now that its global is known"
    );
    assert!(is_overlay(&record.name, &record.target, &globals));
}

/// Test: the negative half of `an_overlay_of_a_global_channel_validates`.
#[test]
fn a_blank_target_with_no_global_is_still_refused() {
    let mut channel = izzie_overlay();
    channel.provider = "gworkspace".into();
    let record = Binding::from(&channel);
    let (status, body) = record
        .validate_in(&[])
        .expect_err("a blank target with no global of that name stays invalid");
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert!(
        body.0["error"]
            .as_str()
            .is_some_and(|text| text.contains("Gmail from:<address>")),
        "the existing destination message is what an operator sees: {body:?}"
    );
    // A global whose name does not match is not a licence either.
    let other = vec![Channel {
        name: "work-mail".into(),
        ..global_gmail(vec![])
    }];
    assert!(record.validate_in(&other).is_err());
    assert!(!is_overlay(&record.name, &record.target, &other));
}

/// A Gmail listener's provider is its connector; the adapter that addresses
/// its events is `gworkspace`.
#[test]
fn adapter_id_bridges_gmail_to_the_gworkspace_adapter() {
    assert_eq!(super::super::adapter_id("gmail"), "gworkspace");
    assert_eq!(super::super::adapter_id("gworkspace"), "gworkspace");
    assert_eq!(super::super::adapter_id("slack"), "slack");
    assert_eq!(super::super::adapter_id("notion"), "notion");
}
