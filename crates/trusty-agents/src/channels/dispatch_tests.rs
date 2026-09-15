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
fn ids(sources: RouteSources) -> (Option<String>, Option<String>) {
    (sources.routed.map(|b| b.id), sources.legacy.map(|b| b.id))
}

/// [`route_sources`] for a Gmail event, whose destination is its sender.
fn sources(
    globals: &[Channel],
    own: &[Channel],
    assistant: &str,
    event: &StoredEvent,
) -> RouteSources {
    let destination = event.from.clone().unwrap_or_default();
    route_sources(globals, own, assistant, "gworkspace", &destination, event)
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
    let claimed = sources(&globals, &[], "izzie", &mail(&["INBOX"]));
    assert!(claimed.claimed);
    assert!(claimed.legacy.is_none());
    let routed = claimed.routed.expect("route_to names izzie");
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
        ids(sources(&globals, &[], "cto-assistant", &mail(&["INBOX"]))),
        (None, None)
    );
}

/// The legacy absorbed binding still wakes while `route_to` is empty.
#[test]
fn a_legacy_overlay_wakes_without_route_to() {
    let globals = vec![global_gmail(vec![])];
    let own = vec![izzie_overlay()];
    let claimed = sources(&globals, &own, "izzie", &mail(&["INBOX"]));
    assert!(claimed.routed.is_none());
    let legacy = claimed
        .legacy
        .expect("the absorbed binding still matches by listener name");
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
    let claimed = sources(&globals, &own, "izzie", &mail(&["INBOX"]));
    assert!(
        claimed.routed.is_some(),
        "the routed global is the one that wakes"
    );
    assert!(
        claimed.legacy.is_none(),
        "the legacy binding must not ALSO wake for the same (global, assistant) pair"
    );
    assert_eq!(
        claimed.routed.expect("routed").instructions,
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
        let rejected = sources(&globals, &own, "izzie", &promo);
        assert!(
            rejected.claimed,
            "the overlay OWNS this channel even when its filter rejects the event"
        );
        assert_eq!(
            ids(rejected),
            (None, None),
            "an excluded label wakes nobody, routed or not"
        );
        let wrong_type = StoredEvent {
            event_type: "message.sent".into(),
            ..mail(&["INBOX"])
        };
        assert_eq!(
            ids(sources(&globals, &own, "izzie", &wrong_type)),
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
    let unknown = sources(&[global], &[], "izzie", &mail(&["INBOX"]));
    assert!(
        !unknown.claimed,
        "a provider this build cannot address claims nothing, so the event keeps its path"
    );
    assert_eq!(ids(unknown), (None, None));
    // An absorbed binding whose global this host does not declare cannot
    // resolve a provider either: it claims nothing and the `[[listeners]]`
    // wake still applies.
    assert_eq!(
        ids(sources(&[], &[izzie_overlay()], "izzie", &mail(&["INBOX"]))),
        (None, None)
    );
}

/// A disabled overlay wakes nobody even when the global routes to it — and it
/// still CLAIMS the event, so nothing else wakes for it either.
///
/// Why (#7609 review HIGH-1): `claimed: false` sent the event to
/// `wake_bound_agents`, which reads the `agent.toml` binding the UI cannot
/// disable. The end-to-end proof is
/// `a_disabled_overlay_claims_the_event_and_wakes_nobody`.
#[test]
fn a_disabled_overlay_wakes_nobody() {
    let globals = vec![global_gmail(vec!["izzie"])];
    let own = vec![Channel {
        enabled: false,
        ..izzie_overlay()
    }];
    let disabled = sources(&globals, &own, "izzie", &mail(&["INBOX"]));
    assert!(disabled.claimed);
    assert_eq!(ids(disabled), (None, None));

    // Send-only is the same answer by the other half of the pair (#7609
    // review HIGH-5): the global does not deliver incoming updates at all.
    let send_only = vec![Channel {
        receive_enabled: false,
        ..global_gmail(vec!["izzie"])
    }];
    let refused = sources(&send_only, &[izzie_overlay()], "izzie", &mail(&["INBOX"]));
    assert!(refused.claimed);
    assert_eq!(ids(refused), (None, None));
}

/// A global's own `target` decides which destinations its `route_to` follows.
///
/// Why (#7609 review HIGH-3): Slack stamps `listener_id: "slack"` on every
/// message, so keying only on that id fanned one `route_to` entry out over the
/// whole workspace. The end-to-end proof is
/// `a_slack_global_routes_only_to_its_own_destination`.
#[test]
fn a_global_target_confines_its_route_to() {
    let global = Channel {
        id: "slack".into(),
        name: "Team Slack".into(),
        provider: "slack".into(),
        scope: ChannelScope::Global,
        target: "C123".into(),
        enabled: true,
        receive_enabled: true,
        route_to: vec!["izzie".into()],
        ..Channel::default()
    };
    let event = StoredEvent {
        id: "slack:C123:1".into(),
        listener_id: "slack".into(),
        provider: "slack".into(),
        event_type: "message.channel".into(),
        ..mail(&[])
    };
    let globals = [global];
    let matched = route_sources(&globals, &[], "izzie", "slack", "C123", &event);
    assert!(matched.claimed);
    assert_eq!(ids(matched).0.as_deref(), Some("slack"));

    let elsewhere = route_sources(&globals, &[], "izzie", "slack", "COTHER", &event);
    assert!(
        !elsewhere.claimed,
        "a channel the global does not name is not its message"
    );
    assert_eq!(ids(elsewhere), (None, None));

    // A global that shares the listener id but not the provider is not this
    // event's channel either — `listener_id` alone was never enough.
    let mismatched = [Channel {
        provider: "gmail".into(),
        target: String::new(),
        ..globals[0].clone()
    }];
    assert!(!route_sources(&mismatched, &[], "izzie", "slack", "C123", &event).claimed);
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
    assert!(is_overlay(&record.id, &record.target, &globals));
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
    // A global whose id does not match is not a licence either.
    let other = vec![Channel {
        id: "work-mail".into(),
        name: "work-mail".into(),
        ..global_gmail(vec![])
    }];
    assert!(record.validate_in(&other).is_err());
    assert!(!is_overlay(&record.id, &record.target, &other));
}

/// An overlay resolves against the global's `id`, not its display `name`.
///
/// Why (#7609 review HIGH-4): dispatch keys on `id` — `global_for` matches
/// `event.listener_id` against it — so keying validation on `name` meant
/// renaming a global in `config.toml` made every overlay of it unstorable.
/// `load_at` validates on READ, so that rename 400'd the whole channels file
/// and stopped every OTHER binding of the assistant from claiming too.
///
/// Pre-fix (886d4afdc) both assertions fail: `is_overlay` compared the
/// candidate's name against `global.name`, which the rename changed.
#[test]
fn an_overlay_resolves_by_the_global_id_not_its_display_name() {
    let globals = vec![Channel {
        name: "Personal mail".into(),
        ..global_gmail(vec![])
    }];
    let mut channel = izzie_overlay();
    channel.provider = "gworkspace".into();
    let record = Binding::from(&channel);
    assert!(is_overlay(&record.id, &record.target, &globals));
    assert!(
        record.validate_in(&globals).is_ok(),
        "renaming the global must not make its overlays unstorable"
    );
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
