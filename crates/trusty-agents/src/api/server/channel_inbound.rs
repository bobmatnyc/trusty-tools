//! `POST /api/channels/{id}/inbound` — inject one inbound event over HTTP
//! (#8036).
//!
//! Why: `agent_channels::inbound::receive_inbound` is the one path every
//! provider's incoming message takes, and all three of its callers
//! (`slack::handlers`, `telegram::inbound`, `listeners::poll`) need a live
//! provider credential to reach it. `POST /api/internal/relay-event`
//! republishes onto the SSE display bus and never enters that path. So #7609's
//! wake/dispatch behaviour — which assistant a global channel's `route_to`
//! actually wakes — had no verification route short of a real Slack workspace.
//! This is that route.
//!
//! What: the injection names a STORED channel by id. Its provider, its
//! destination and its `route_to` are read from `config.toml`, so an injected
//! event cannot address a destination the operator never declared, and the
//! request body carries only the message. The event is then handed to
//! [`crate::api::server::agent_channels::inbound::receive_inbound_with`] —
//! the same roster read, the same global-config read, the same source
//! selection, the same dispatch budget a Gmail poll cycle uses. What it does
//! NOT do, exactly as `telegram::inbound` does not, is append to the
//! `EventStore` or publish `Event::ListenerEventReceived`: an injected event is
//! not a received one, and putting it in the Events pane would make test
//! traffic indistinguishable from real traffic.
//!
//! Authorization: [`ChannelWriter`] is the first handler argument, so this
//! takes the SAME gate as `PUT /api/channels`. An injected event decides which
//! assistant spends a model dispatch, which is the same power a channel write
//! has; a tokenless daemon refuses it.
//! Test: `crate::api::server::tests::channel_inbound` — the whole module.

// #8036: the HTTP seam into the provider-neutral inbound path.
use axum::{Json, extract::Path as AxumPath, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::agent_channels::inbound::{
    DispatchBudget, InboundDispatch, InboundOutcome, SpawnDispatch, receive_inbound_with,
};
use super::channel_auth::ChannelWriter;

type Error = (StatusCode, Json<Value>);

fn err(code: StatusCode, text: &str) -> Error {
    (code, Json(json!({ "error": text })))
}

fn bad(text: &str) -> Error {
    err(StatusCode::BAD_REQUEST, text)
}

/// The longest message an injected event may carry, matching the send cap.
const MAX_TEXT_CHARS: usize = 4000;

/// The longest header-shaped field an injected event may carry.
const MAX_HEADER_CHARS: usize = 256;

/// One inbound event, as a client states it.
///
/// What: the channel supplies the provider, the destination and the routing, so
/// the body is the message alone.
///
/// Why no destination override (#8036 review): the field existed and no caller
/// set it. It would also have escaped this route's containment —
/// `agent_channels::inbound::receive_selection` picks a per-assistant binding
/// by `(provider, destination)` and never by channel id, so an injection on
/// global channel A naming B's destination would wake whatever assistant binds
/// B over the same provider, which is not what the named channel routes to.
/// `deny_unknown_fields` makes a body that still carries one a 4xx rather than
/// a silent ignore.
/// Test: `an_injection_may_not_name_its_own_destination`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Injection {
    /// Message body; becomes the event's `snippet`.
    pub(crate) text: String,
    /// Normalized event type, matched against the channel's `event_types` and
    /// wake filter. Defaults to the provider-neutral `message`.
    #[serde(default = "default_event_type")]
    pub(crate) event_type: String,
    /// Sender, as a wake filter's `from` rule reads it.
    #[serde(default)]
    pub(crate) from: Option<String>,
    /// Subject, as a wake filter's `subject` rule reads it.
    #[serde(default)]
    pub(crate) subject: Option<String>,
}

fn default_event_type() -> String {
    "message".to_string()
}

/// Reject text no provider would have delivered.
///
/// What: one rule per field, all bounded and all control-character free, so an
/// injected event cannot carry something a real provider's own parsing would
/// have stripped — the point is to reproduce real traffic, not to reach past
/// what real traffic can express.
/// Test: `an_injection_body_is_bounded_like_a_real_event`.
fn validate(injection: &Injection) -> Result<(), Error> {
    let trimmed = injection.text.trim();
    if trimmed.is_empty() || injection.text.chars().count() > MAX_TEXT_CHARS {
        return Err(bad("Message must contain 1–4000 characters"));
    }
    let bounded = |value: &str| {
        !value.is_empty()
            && value.chars().count() <= MAX_HEADER_CHARS
            && !value.chars().any(char::is_control)
    };
    if !bounded(&injection.event_type) {
        return Err(bad(
            "Event type must be 1–256 characters without control characters",
        ));
    }
    for (label, value) in [
        ("Sender", injection.from.as_deref()),
        ("Subject", injection.subject.as_deref()),
    ] {
        if let Some(value) = value
            && !bounded(value)
        {
            return Err(bad(&format!(
                "{label} must be 1–256 characters without control characters"
            )));
        }
    }
    Ok(())
}

/// The project root a woken turn runs in, or a 500.
///
/// Why (fail-open, #8036): `unwrap_or_else(|_| PathBuf::from("."))` is the
/// idiom this module would otherwise have copied from its neighbours, and it
/// would start an assistant turn rooted somewhere nobody chose — the dispatch
/// still advances, in the wrong place. Splitting the decision out is what lets
/// the error arm be tested at all; `std::env::current_dir` cannot be made to
/// fail from inside a test.
/// Test: `an_unresolvable_project_root_refuses_the_injection`.
fn root_or_error(cwd: std::io::Result<std::path::PathBuf>) -> Result<std::path::PathBuf, Error> {
    cwd.map_err(|e| {
        tracing::warn!(error = %e, "channel inbound: the project root could not be resolved; the injection was refused (#8036)");
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The project root could not be resolved; the event was not dispatched",
        )
    })
}

/// The stored event an injection becomes.
///
/// What: `listener_id` is the CHANNEL id, because that is what
/// `channels::dispatch::global_for` keys a global channel on — an injected
/// event that named anything else would address no channel at all. The id is a
/// fresh UUID so a replay of the same body is a distinct event.
/// Test: `an_injected_event_carries_the_channel_and_the_message`.
fn event_from(
    channel_id: &str,
    provider: &str,
    injection: &Injection,
) -> crate::listeners::store::StoredEvent {
    crate::listeners::store::StoredEvent {
        id: format!("injected:{}", uuid::Uuid::new_v4()),
        listener_id: channel_id.to_string(),
        provider: provider.to_string(),
        event_type: injection.event_type.clone(),
        ts: chrono::Utc::now().to_rfc3339(),
        from: injection.from.clone(),
        subject: injection.subject.clone(),
        snippet: Some(injection.text.clone()),
        included: true,
        labels: Vec::new(),
    }
}

/// What one injection did, as the caller sees it.
///
/// Why: `woken` is the field this route exists for — #8036's closure condition
/// is that the dispatch is "observable via the API", and only the assistant
/// NAMES make "exactly the routed assistant, and nobody else" checkable.
fn view(channel_id: &str, provider: &str, event_id: &str, outcome: &InboundOutcome) -> Value {
    json!({
        "channel": channel_id,
        "provider": provider,
        "event_id": event_id,
        "claimed": outcome.claimed,
        "dispatched": outcome.dispatched,
        "rate_limited": outcome.rate_limited,
        "source": outcome.source.map(crate::channels::dispatch::WakeSource::as_str),
        "woken": outcome.woken,
    })
}

/// Inject one event on a stored global channel, through the live dispatcher.
///
/// Test: `an_injected_event_wakes_only_the_routed_assistant`.
pub(crate) async fn inject(channel_id: &str, injection: Injection) -> Result<Value, Error> {
    inject_with(channel_id, injection, &SpawnDispatch).await
}

/// [`inject`] against a caller-supplied dispatcher.
///
/// Why: everything above the dispatcher — resolving the channel, building the
/// event, loading the roster, selecting the source — is what the test has to
/// exercise, and the model call is the one thing it must not. See
/// [`receive_inbound_with`].
/// What: 404 when no global channel carries `channel_id`, 400 when this build
/// has no adapter for its provider, and otherwise the dispatch outcome. A
/// channel that is disabled, not receiving, or whose filter rejects the event
/// is NOT an error: it answers `dispatched: false`, which is the behaviour
/// under test.
/// Test: `an_injected_event_wakes_only_the_routed_assistant`,
/// `an_injection_into_a_disabled_channel_wakes_nobody`.
pub(crate) async fn inject_with(
    channel_id: &str,
    injection: Injection,
    dispatcher: &dyn InboundDispatch,
) -> Result<Value, Error> {
    validate(&injection)?;
    let channels = super::global_channels::stored().await?;
    let channel = channels
        .iter()
        .find(|channel| channel.id == channel_id)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "No global channel has this ID"))?;
    // #7609: a channel's stored provider may be a CONNECTOR id (`gmail`) whose
    // events are addressed by another adapter (`gworkspace`); dispatch resolves
    // it the same way, so an injection has to as well or it addresses nothing.
    let provider = crate::channels::adapter_id(&channel.provider).to_string();
    if crate::channels::adapter(&provider).is_none() {
        return Err(bad("Unsupported channel provider"));
    }
    // The STORED destination, never a client-supplied one — see [`Injection`].
    let destination = channel.target.clone();
    let event = event_from(channel_id, &provider, &injection);
    let identity = crate::rbac::UserIdentity::from_remote(
        format!("injected:{channel_id}"),
        event.from.as_deref(),
        &provider,
    );
    let root = root_or_error(std::env::current_dir())?;
    // One injected event in, one turn per selected assistant out — the same
    // allowance a single Slack or Telegram message carries.
    let outcome = receive_inbound_with(
        &provider,
        &destination,
        &event,
        &root,
        &identity,
        None,
        &mut DispatchBudget::PerEvent,
        dispatcher,
    )
    .await;
    Ok(view(channel_id, &provider, &event.id, &outcome))
}

/// `POST /api/channels/{id}/inbound` — inject one inbound channel event.
///
/// Why: `writer` is the FIRST argument because extracting it IS the
/// authorization gate — see [`super::channel_auth`]. An injected event spends a
/// model dispatch on an assistant of the caller's choosing, so it is held to
/// the same credential a channel write is.
/// Test: `an_unauthenticated_injection_is_refused`,
/// `an_injected_event_wakes_only_the_routed_assistant`.
pub(super) async fn post_route(
    writer: ChannelWriter,
    AxumPath(id): AxumPath<String>,
    Json(injection): Json<Injection>,
) -> Result<Json<Value>, Error> {
    let outcome = inject(&id, injection).await?;
    // The channel count is unchanged by an injection; the audit line exists to
    // record that a credentialed caller woke an assistant on this channel.
    writer.audit(
        "POST /api/channels/{id}/inbound",
        "global",
        Some(&id),
        None,
        outcome["woken"].as_array().map_or(0, Vec::len),
    );
    Ok(Json(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn injection(text: &str) -> Injection {
        Injection {
            text: text.to_string(),
            event_type: default_event_type(),
            from: None,
            subject: None,
        }
    }

    /// Every field an injection carries is bounded like a real provider's.
    #[test]
    fn an_injection_body_is_bounded_like_a_real_event() {
        assert!(validate(&injection("hello")).is_ok());
        assert_eq!(
            validate(&injection("   ")).unwrap_err().0,
            StatusCode::BAD_REQUEST,
            "blank text is refused"
        );
        assert_eq!(
            validate(&injection(&"x".repeat(MAX_TEXT_CHARS + 1)))
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        let mut typed = injection("hello");
        typed.event_type = String::new();
        assert_eq!(validate(&typed).unwrap_err().0, StatusCode::BAD_REQUEST);
        let mut sender = injection("hello");
        sender.from = Some("own\u{0007}er".into());
        assert_eq!(validate(&sender).unwrap_err().0, StatusCode::BAD_REQUEST);
        let mut subject = injection("hello");
        subject.subject = Some("x".repeat(MAX_HEADER_CHARS + 1));
        assert_eq!(validate(&subject).unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    /// An injection cannot state the destination it arrived on.
    ///
    /// Why (#8036 review): the destination decides which per-assistant binding
    /// claims the event, so a client-supplied one reaches past the channel the
    /// route is addressed to — see [`Injection`]. The parse refusing the field
    /// is what keeps a caller from re-opening it, so that is what is asserted;
    /// the accepted arm is asserted beside it so the test cannot pass by the
    /// body being unparseable outright.
    #[test]
    fn an_injection_may_not_name_its_own_destination() {
        let accepted: Injection = serde_json::from_value(json!({"text": "are you there"}))
            .expect("a message-only body is the whole contract");
        assert_eq!(accepted.event_type, "message");
        assert!(
            serde_json::from_value::<Injection>(
                json!({"text": "are you there", "destination": "someone-elses-desk"})
            )
            .is_err(),
            "a stated destination is refused, never quietly ignored"
        );
    }

    /// An unresolvable project root refuses the injection; it never defaults.
    ///
    /// Why (fail-open): see [`root_or_error`]. The success arm is asserted too,
    /// so the test cannot pass by the function refusing everything.
    #[test]
    fn an_unresolvable_project_root_refuses_the_injection() {
        let failure = std::io::Error::new(std::io::ErrorKind::NotFound, "no cwd");
        assert_eq!(
            root_or_error(Err(failure)).unwrap_err().0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            root_or_error(Ok(std::path::PathBuf::from("/tmp"))).expect("a resolved root"),
            std::path::PathBuf::from("/tmp")
        );
    }

    /// The event an injection becomes is addressed to the channel by id.
    #[test]
    fn an_injected_event_carries_the_channel_and_the_message() {
        let mut body = injection("are you there");
        body.from = Some("Owner".into());
        body.subject = Some("Ping".into());
        let event = event_from("stub-desk", "stub", &body);
        assert_eq!(
            event.listener_id, "stub-desk",
            "dispatch keys a global channel on the listener id"
        );
        assert_eq!(event.provider, "stub");
        assert_eq!(event.snippet.as_deref(), Some("are you there"));
        assert_eq!(event.from.as_deref(), Some("Owner"));
        assert!(event.id.starts_with("injected:"));
        assert_ne!(
            event_from("stub-desk", "stub", &body).id,
            event.id,
            "a replay is a distinct event"
        );
    }
}
