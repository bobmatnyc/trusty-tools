//! Which channel wakes an assistant for one inbound event — exactly one
//! (#7609, slice 4).
//!
//! Why: after the listener/channel merge an event can reach an assistant three
//! ways, and two of them describe the SAME live configuration. An assistant's
//! own channel claims a destination it names (#7427). A GLOBAL channel can name
//! the assistant in `route_to` (owner ruling 2026-09-14, option A). And a
//! `[[listeners]]` binding absorbed out of `agent.toml` still matches by
//! listener name, for the deprecation window. Running two of those for one
//! message wakes the assistant twice, from two different prompts, against one
//! poll cycle's single dispatch — and the two that overlap are exactly the
//! routed global and the legacy binding it was backfilled from, because they
//! state one intent twice.
//! What: [`select_source`] is the whole rule as a pure function of three
//! booleans — (1) beats (2) beats (3) — so "never two" is a property a truth
//! table states rather than a reading of `if`s spread through the inbound loop.
//! [`route_sources`] finds the (2) and (3) candidates and returns AT MOST ONE
//! of them: `route_to` naming the assistant is precisely what moves the pair
//! from (3) to (2). [`is_overlay`] is the discriminant this module and
//! `Binding::validate_in` share for the one channel shape that legitimately has
//! no destination of its own. Every fall-through is logged, never silent, and
//! nothing here ever broadcasts to an assistant `route_to` did not name.
//! Test: `crate::channels::dispatch_tests` — the whole module.

// #7609: slice 4, inbound dispatch generalized over channels.
use super::model::{Channel, ChannelScope};
use crate::api::server::agent_channels::Binding;
use crate::listeners::store::StoredEvent;

/// Which of the three inbound sources selects an assistant for one event.
///
/// Why: the poll loop logs which path a message took and the truth table
/// asserts exactly one was taken; both want a name for the answer rather than
/// a pair of booleans whose illegal combinations are representable.
/// Test: `the_three_sources_select_exactly_one_wake`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeSource {
    /// (1) An assistant-scope channel whose own destination addresses it.
    AssistantChannel,
    /// (2) A global channel whose `route_to` names this assistant.
    GlobalRouteTo,
    /// (3) A legacy absorbed `[[listeners]]` binding, matched by listener name.
    LegacyBinding,
    /// Nothing selects this assistant.
    NoWake,
}

impl WakeSource {
    /// The spelling the poll loop and the inbound log line use.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AssistantChannel => "assistant-channel",
            Self::GlobalRouteTo => "global-route-to",
            Self::LegacyBinding => "legacy-binding",
            Self::NoWake => "none",
        }
    }
}

/// The exactly-one-wake rule: source (1) beats (2) beats (3).
///
/// Why: an assistant's own channel is the most specific statement about a
/// destination, so it wins — the same precedence [`super::resolve_channels`]
/// already applies to storage. A routed global beats the legacy binding
/// because the backfill derives the one from the other, so firing both would
/// wake twice for one operator intent.
/// What: pure and total over all eight input combinations; every one answers
/// exactly one variant.
/// Test: `the_three_sources_select_exactly_one_wake`.
pub(crate) fn select_source(assistant: bool, routed: bool, legacy: bool) -> WakeSource {
    match (assistant, routed, legacy) {
        (true, _, _) => WakeSource::AssistantChannel,
        (false, true, _) => WakeSource::GlobalRouteTo,
        (false, false, true) => WakeSource::LegacyBinding,
        (false, false, false) => WakeSource::NoWake,
    }
}

/// Whether a stored channel with no destination of its own is an OVERLAY of a
/// global channel — the one shape an empty `target` is legitimate for.
///
/// Why (#7609): a `[[listeners]]` binding absorbed out of `agent.toml` names a
/// global channel and layers this assistant's `wake_filter`, `event_types`,
/// `instructions` and `enabled` over it. It never named a destination, because
/// the global already has one, so refusing a blank target leaves the live
/// configuration unstorable — which is why slices 1–3 had to leave it in
/// `agent.toml`. Accepting ANY blank target instead would let an operator save
/// a channel that can never address anything, the exact failure the target
/// grammar exists to prevent. The allowance is therefore conditioned on a
/// global of the same `name` actually existing.
/// What: `name` and `target` are the candidate's; `globals` is the harness-wide
/// `[[channels]]` list. An empty `name` is never an overlay.
/// Test: `an_overlay_of_a_global_channel_validates`,
/// `a_blank_target_with_no_global_is_still_refused`.
pub(crate) fn is_overlay(name: &str, target: &str, globals: &[Channel]) -> bool {
    target.is_empty()
        && !name.is_empty()
        && globals
            .iter()
            .any(|global| global.scope == ChannelScope::Global && global.name == name)
}

/// The global channel an event's listener belongs to.
///
/// What: keyed on `id`, which is the listener's stable name — `event.listener_id`
/// is exactly what `Channel::from_listener` stored there.
fn global_for<'a>(globals: &'a [Channel], event: &StoredEvent) -> Option<&'a Channel> {
    globals
        .iter()
        .find(|global| global.scope == ChannelScope::Global && global.id == event.listener_id)
}

/// The channel a routed or legacy wake actually dispatches through.
///
/// Why: the global owns the destination, the provider and what is ingested at
/// all; the assistant's overlay owns which of those events wake IT. Merging
/// them here is what lets one global filter pair be shared by every name in
/// `route_to` while an assistant that wrote its own filter still gets it
/// (PM ruling 6) — with no per-name filter map on the global.
/// What: `None` when neither side exists, and `None` when the merged provider
/// resolves to no adapter — a provider this build cannot address claims
/// nothing rather than swallowing the event. The overlay carries no
/// `credential_ref`: it has no destination of its own, so it never sends, and
/// the send credential stays on the global where the poller reads it.
/// Test: `a_routed_global_wakes_the_named_assistant`,
/// `an_overlay_filter_narrows_the_global_it_covers`,
/// `an_unknown_provider_claims_nothing`.
fn effective(global: Option<&Channel>, overlay: Option<&Channel>) -> Option<Channel> {
    let mut channel = match (global, overlay) {
        (Some(global), _) => global.clone(),
        (None, Some(overlay)) => overlay.clone(),
        (None, None) => return None,
    };
    channel.scope = ChannelScope::Assistant;
    channel.credential_ref = None;
    channel.provider = super::adapter_id(&channel.provider).to_string();
    if let Some(overlay) = overlay {
        channel.enabled = overlay.enabled;
        channel.event_types = overlay.event_types.clone();
        channel.wake_filter = overlay.wake_filter.clone();
        channel.instructions = overlay.instructions.clone();
    }
    super::adapter(&channel.provider)
        .is_some()
        .then_some(channel)
}

/// Whether this channel's stage-two filter passes `event`.
///
/// What: the SAME predicate the `[[listeners]]` wake applies, reached through
/// the derived binding view, so a filter means one thing on both paths.
fn wakes(channel: &Channel, event: &StoredEvent) -> bool {
    crate::listeners::wake::binding_matches_event(&channel.to_agent_binding(), event)
}

/// The (2) and (3) candidates for one assistant and one event.
///
/// Why: these are the two sources that can describe one configuration, so they
/// are decided together rather than by two independent scans that could both
/// answer yes.
/// What: returns `(routed, legacy)`, AT MOST ONE of which is `Some`. The
/// assistant appearing in the global's `route_to` is exactly what moves the
/// pair from (3) to (2), which is the owner's 2026-09-14 option-A rule stated
/// once. A candidate whose merged channel is disabled, whose filter rejects the
/// event, or whose provider this build carries no adapter for claims NOTHING —
/// the event stays on whatever path it was already taking.
/// Test: `a_routed_global_wakes_the_named_assistant`,
/// `a_legacy_overlay_wakes_without_route_to`,
/// `a_routed_global_excludes_the_legacy_binding_for_the_same_pair`,
/// `an_overlay_filter_narrows_the_global_it_covers`,
/// `an_unknown_provider_claims_nothing`.
pub(crate) fn route_sources(
    globals: &[Channel],
    own: &[Channel],
    assistant: &str,
    event: &StoredEvent,
) -> (Option<Binding>, Option<Binding>) {
    let global = global_for(globals, event);
    let overlay = own
        .iter()
        .find(|channel| channel.target.is_empty() && channel.id == event.listener_id);
    let routed = global.is_some_and(|global| global.route_to.iter().any(|name| name == assistant));
    if !routed && overlay.is_none() {
        return (None, None);
    }
    let Some(channel) = effective(global, overlay) else {
        tracing::warn!(
            assistant = %assistant,
            listener = %event.listener_id,
            "channel dispatch: this channel's provider has no adapter; claiming nothing (#7609)"
        );
        return (None, None);
    };
    if !channel.enabled || !wakes(&channel, event) {
        return (None, None);
    }
    let binding = Binding::from(&channel);
    if routed {
        (Some(binding), None)
    } else {
        (None, Some(binding))
    }
}

/// `route_to` entries naming an assistant this host does not have.
///
/// Why (fail-open, #7609): a typo in `route_to` has to be visible. Dropping the
/// event reads exactly like "nobody is bound to this channel", and the only
/// alternative anyone reaches for — waking every assistant instead — would turn
/// a typo into an unbounded fan-out. An unroutable name wakes nobody AND says
/// so.
/// What: `(channel id, unroutable name)` per offending entry, in stored order.
/// Test: `an_unknown_route_to_name_is_reported`.
pub(crate) fn unknown_routes<'a>(
    globals: &'a [Channel],
    known: &[String],
) -> Vec<(&'a str, &'a str)> {
    globals
        .iter()
        .flat_map(|channel| {
            channel
                .route_to
                .iter()
                .filter(|name| !known.iter().any(|value| value == *name))
                .map(move |name| (channel.id.as_str(), name.as_str()))
        })
        .collect()
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod dispatch_tests;
