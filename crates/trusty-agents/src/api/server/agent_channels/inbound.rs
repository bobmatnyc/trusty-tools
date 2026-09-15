//! The provider-neutral inbound path: one saved binding, one assistant turn.
//!
//! Why (#7427): Slack, Telegram and Gmail all arrive here, and all of them have
//! to answer the same two questions — which assistant owns this destination, and
//! may this message spend a model dispatch. Keeping that with the HTTP handlers
//! in the parent module put two unrelated concerns in one file and pushed it
//! over the 500-SLOC cap, so the dispatch path is its own module and the parent
//! keeps the configuration surface (load, validate, save, send).
//!
//! What: [`receive_selection`] picks the binding, [`DispatchBudget`] says
//! whether it may fire, and [`receive_inbound_at`] joins them. The budget is
//! what makes the Gmail poll cycle's one-dispatch cap cover BOTH inbound paths:
//! `listeners::poll` holds a single [`DispatchBudget::OnePerCycle`] and hands it
//! to the channel bindings and then to the `[[listeners]]` wake.
//!
//! Test: `agent_channels_receive_never_falls_back_for_disabled_bound_destination`,
//! `agent_channels_inbound_ignores_an_unbound_telegram_chat`,
//! `agent_channels_gmail_binding_claims_only_its_own_correspondent`,
//! `gworkspace_binding_dispatches_once_per_poll_cycle`,
//! `dispatch_budget_spends_once_per_cycle_and_never_for_per_event`.

use super::*;

/// Which saved destination, if any, this inbound event belongs to.
///
/// Why (#7427): the provider was hard-coded to `"slack"`, so no other
/// provider's event could select a binding however it was configured. Taking
/// the provider as an argument is what lets one inbound path serve all of them,
/// and it is what confines a Telegram update to bindings that name a Telegram
/// chat id — a message from an unbound chat matches nothing and is never
/// dispatched. Asking the adapter whether a target addresses the event is what
/// lets Gmail — which has no destination id — bind a correspondent or a label.
/// What: returns `(claimed, selected)` — `claimed` is true when any binding
/// addresses this event at all (so the caller knows an assistant owns this
/// conversation even when the binding is disabled), `selected` is the one
/// enabled, receive-enabled binding whose filter the event also passes.
/// Test: `agent_channels_receive_never_falls_back_for_disabled_bound_destination`,
/// `agent_channels_inbound_ignores_an_unbound_telegram_chat`,
/// `agent_channels_gmail_binding_claims_only_its_own_correspondent`.
fn receive_selection<'a>(
    bindings: &'a [Binding],
    provider: &str,
    channel: &str,
    event: &crate::listeners::store::StoredEvent,
    persona_allowed: bool,
) -> (bool, Option<&'a Binding>) {
    let Some(adapter) = crate::channels::adapter(provider) else {
        return (false, None);
    };
    let destinations: Vec<_> = bindings
        .iter()
        .filter(|b| b.provider == provider && adapter.addresses(&b.target, channel, event))
        .collect();
    let claimed = !destinations.is_empty();
    let selected = if persona_allowed {
        destinations.into_iter().find(|b| {
            b.enabled
                && b.receive_enabled
                && crate::listeners::wake::binding_matches_event(
                    &AgentListenerBinding {
                        name: event.listener_id.clone(),
                        enabled: true,
                        event_types: vec![],
                        filter: b.filter.clone(),
                        instructions: b.instructions.clone(),
                    },
                    event,
                )
        })
    } else {
        None
    };
    (claimed, selected)
}

/// How many wake dispatches one inbound call may spend.
///
/// Why (#7427 code-critic CRITICAL): the Gmail listener caps itself at ONE wake
/// dispatch per `poll_once` call — a named requirement from #3820, enforced on
/// the `[[listeners]]` path by `listeners::wake::gate_wake`. The channel-binding
/// path arrived beside it with no such cap, so a burst of mail from one bound
/// correspondent between two polls spent one LLM dispatch per message. Making
/// the allowance a value the caller owns is what lets ONE budget cover BOTH
/// paths: `poll_once` holds a single [`Self::OnePerCycle`] for the whole cycle
/// and hands it to each in turn.
/// What: Slack and Telegram hand in one message per call and have no cycle to
/// share, so they pass [`Self::PerEvent`], which never refuses.
/// Test: `gworkspace_binding_dispatches_once_per_poll_cycle`,
/// `dispatch_budget_spends_once_per_cycle_and_never_for_per_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispatchBudget {
    /// Every matching binding dispatches; nothing is shared between calls.
    PerEvent,
    /// One dispatch for a whole poll cycle, shared with the listener wake.
    OnePerCycle { spent: bool },
}

impl DispatchBudget {
    /// A fresh poll cycle's allowance: one dispatch, not yet spent.
    pub(crate) fn one_per_cycle() -> Self {
        Self::OnePerCycle { spent: false }
    }

    /// Spend the allowance. `false` means this dispatch is rate-limited.
    pub(crate) fn take(&mut self) -> bool {
        match self {
            Self::PerEvent => true,
            Self::OnePerCycle { spent: true } => false,
            Self::OnePerCycle { spent } => {
                *spent = true;
                true
            }
        }
    }

    /// Whether the cycle's single dispatch is already gone.
    pub(crate) fn is_spent(&self) -> bool {
        matches!(self, Self::OnePerCycle { spent: true })
    }
}

/// What one inbound event did on the channel-binding path.
///
/// Why (#7427 code-critic CRITICAL): `receive_inbound` returned a bare `bool`
/// meaning "claimed", which cannot distinguish a message that woke an assistant
/// from one the budget refused — and `poll_once` needs exactly that difference
/// to know whether the cycle's dispatch is still available.
/// Test: `gworkspace_binding_dispatches_once_per_poll_cycle`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InboundOutcome {
    /// A binding addresses this event, dispatched or not — so the caller's own
    /// fallback stands down either way.
    pub(crate) claimed: bool,
    /// A wake dispatch actually started.
    pub(crate) dispatched: bool,
    /// Matching bindings the budget refused, counted by
    /// [`crate::channels::status::record_rate_limited`].
    pub(crate) rate_limited: usize,
    /// Which source selected the last assistant a channel was chosen for.
    ///
    /// Why (#7609): `listeners::poll` logs which inbound path a message took,
    /// and a legacy `[[listeners]]` binding handled HERE must be named as such
    /// so the log does not read as if the listener wake had run too. `None`
    /// beside `claimed` means a binding owns the destination but nothing was
    /// selected — a disabled binding, or a filter that rejected the event.
    /// Test: `the_izzie_fixture_wakes_once_for_an_inbox_message`.
    pub(crate) source: Option<crate::channels::dispatch::WakeSource>,
}

/// Starting the assistant turn one prepared inbound event becomes.
///
/// Why: the seam that makes the per-cycle cap testable. The live implementation
/// spawns `run_pm_task_with_persona`, which needs model credentials and a real
/// agent directory, so a test that asserted "two messages, one dispatch" against
/// it would be asserting against the network.
/// Test: `gworkspace_binding_dispatches_once_per_poll_cycle`.
#[async_trait::async_trait]
trait InboundDispatch: Sync {
    /// Build the wake prompt this event becomes, or `Ok(None)` when it earns
    /// none.
    ///
    /// Why (#7427 code-critic MEDIUM): the cycle's dispatch is spent only after
    /// this succeeds, and no test could state that ordering while the adapter
    /// was reached directly — every registered adapter's `receive` is
    /// infallible in process, so nothing could make one fail. Overriding this
    /// is what lets a test fail one binding and watch the cycle's single
    /// dispatch survive for the next.
    /// What: the default is the live lookup. A provider this build carries no
    /// adapter for earns no prompt, which is the skip the caller used to write
    /// inline.
    /// Test: `a_failed_wake_prompt_leaves_the_cycle_dispatch_for_the_next_binding`.
    async fn prepare(
        &self,
        binding: &Binding,
        event: crate::channels::InboundEvent<'_>,
    ) -> Result<Option<crate::channels::WakePrompt>, crate::channels::ChannelError> {
        prepare_via_adapter(binding, event).await
    }

    /// Start `agent`'s turn for `wake`, recording the result on `binding_id`.
    async fn dispatch(
        &self,
        agent: &str,
        binding_id: &str,
        wake: crate::channels::WakePrompt,
        root: &std::path::Path,
        user: &crate::rbac::UserIdentity,
    );
}

/// The live prompt build: the binding's own provider adapter.
///
/// What: `Ok(None)` for a provider this build has no adapter for, so an
/// unsupported binding is skipped rather than dispatched or counted as failed.
async fn prepare_via_adapter(
    binding: &Binding,
    event: crate::channels::InboundEvent<'_>,
) -> Result<Option<crate::channels::WakePrompt>, crate::channels::ChannelError> {
    match crate::channels::adapter(&binding.provider) {
        Some(adapter) => adapter.receive(binding, event).await,
        None => Ok(None),
    }
}

/// The live dispatcher: a detached persona turn, its result recorded.
struct SpawnDispatch;

#[async_trait::async_trait]
impl InboundDispatch for SpawnDispatch {
    async fn dispatch(
        &self,
        agent: &str,
        binding_id: &str,
        wake: crate::channels::WakePrompt,
        root: &std::path::Path,
        user: &crate::rbac::UserIdentity,
    ) {
        let agent = agent.to_string();
        let binding_id = binding_id.to_string();
        let root = root.to_path_buf();
        let user = user.clone();
        tokio::spawn(async move {
            let dispatch = crate::listeners::wake::LISTENER_CHAT_EVENT.scope(
                wake.metadata,
                crate::ctrl::pm_task::run_pm_task_with_persona(
                    &root,
                    &agent,
                    &wake.prompt,
                    &[],
                    None,
                    crate::ctrl::config::SessionOverrides {
                        user: Some(user),
                        ..Default::default()
                    },
                ),
            );
            // Incoming updates remain private in the assistant chat; sending
            // requires an explicit UI or tool request. #7427 keeps Telegram on
            // that same rule: a turn woken by a Telegram message reaches the
            // chat only when the persona calls the `channel` tool, which routes
            // through `send` above and therefore `TelegramAdapter::send`. The
            // reply is never auto-posted, so a binding with `send_enabled`
            // false can receive without being able to answer.
            crate::channels::status::record_dispatch(&agent, &binding_id, dispatch).await;
        });
    }
}

/// The one inbound path: an authenticated provider event, matched against every
/// assistant's saved destinations and dispatched as a wake.
///
/// Why (#7427): this was `receive_slack`, with `"slack"` written into its
/// binding filter, so every other provider needed a dispatch path of its own —
/// which is how two channels end up on different prompt shapes and different
/// failure handling. One function, with the provider as an argument, is what
/// makes DOC-60 §8's "one envelope regardless of channel" true rather than
/// merely intended. The wake dispatch also used to be spawned and its result
/// discarded (`let _result = …`), so a failed `run_pm_task_with_persona`
/// produced no log line and no visible change — the binding read as healthy
/// while every inbound message was dropped. The dispatch now goes through
/// [`crate::channels::status::record_dispatch`], which logs at error level and
/// increments the per-binding counter the channel view reads.
/// What: loads every assistant's saved bindings, then hands them to
/// [`receive_inbound_at`], which does the selecting and dispatching. `budget`
/// is the caller's dispatch allowance — [`DispatchBudget::PerEvent`] for a
/// single Slack or Telegram message, or the poll cycle's shared
/// [`DispatchBudget::OnePerCycle`]. The returned [`InboundOutcome`] says whether
/// any assistant claimed the destination — a caller with its own fallback (the
/// Slack session map, the Telegram long-poll gateway, the Gmail listener wake)
/// uses that to decide whether to handle the event itself.
/// Test: `agent_channels_receive_never_falls_back_for_disabled_bound_destination`,
/// `agent_channels_inbound_ignores_an_unbound_telegram_chat`,
/// `agent_channels_gmail_binding_claims_only_its_own_correspondent`,
/// `gworkspace_binding_dispatches_once_per_poll_cycle`,
/// `channel_dispatch_failure_is_counted_per_binding`.
pub(crate) async fn receive_inbound(
    provider: &str,
    channel: &str,
    event: &crate::listeners::store::StoredEvent,
    root: &std::path::Path,
    user: &crate::rbac::UserIdentity,
    allowed_personas: Option<&[String]>,
    budget: &mut DispatchBudget,
) -> InboundOutcome {
    let dirs = crate::agents::agents_dir_candidates();
    let Some(names) = roster_or_warn(
        crate::listeners::wake::candidate_agent_names().await,
        provider,
    ) else {
        return InboundOutcome::default();
    };
    // #7609: the harness-wide channels, read ONCE per event — `route_to` and
    // the overlay allowance are both properties of this list.
    let globals = crate::mcp::config::GlobalConfig::load().await.channels;
    for (id, unroutable) in crate::channels::dispatch::unknown_routes(&globals, &names) {
        tracing::warn!(
            channel = %id,
            assistant = %unroutable,
            "channel inbound: `route_to` names an assistant this host does not have; it wakes nobody (#7609)"
        );
    }
    let mut loaded = Vec::new();
    for name in names {
        match load_at_with(&dirs, &name, &globals).await {
            Ok((_, _, mut bindings)) => {
                absorb_unstored_overlays(&mut bindings, &name).await;
                loaded.push((name, bindings));
            }
            Err((status, body)) => tracing::warn!(
                assistant = %name, %status, error = %body.0,
                "channel inbound: this assistant's channels could not be read; it claims nothing"
            ),
        }
    }
    receive_inbound_at(
        &loaded,
        provider,
        channel,
        event,
        root,
        user,
        allowed_personas,
        budget,
        &globals,
        &SpawnDispatch,
    )
    .await
}

/// The assistant roster, or `None` with a warning when it cannot be read.
///
/// Why (#7609): the enumeration failure used to be absorbed into an all-false
/// [`InboundOutcome`] with no log at all, which is indistinguishable from "no
/// assistant is bound to this destination" — the event was dropped and nothing
/// said so. Splitting the decision out is what lets a test state it, because
/// [`crate::listeners::wake::candidate_agent_names`] cannot be made to fail
/// from outside.
/// Test: `an_unreadable_roster_is_logged_and_claims_nothing`.
fn roster_or_warn(names: anyhow::Result<Vec<String>>, provider: &str) -> Option<Vec<String>> {
    match names {
        Ok(names) => Some(names),
        Err(e) => {
            tracing::warn!(
                %provider, error = %e,
                "channel inbound: the assistant roster could not be enumerated; nothing claimed"
            );
            None
        }
    }
}

/// Add any overlay that is still only in `agent.toml` to this assistant's
/// bindings.
///
/// Why (#7609): a `[[listeners]]` binding absorbed at parse time lives in
/// `AgentConfig::channels`, not in `<name>.channels.json` — and the one-shot
/// that would store it only runs when that file is ABSENT, so an assistant
/// which already has a saved channel keeps its legacy binding in `agent.toml`
/// indefinitely. The `[[listeners]]` wake read it from there; source (3) has to
/// see exactly the same list or the deprecation window drops a live wake.
/// What: appends every Assistant-scope channel with no destination whose `id`
/// the stored bindings do not already carry. The stored record wins, because it
/// is the migrated form of the same thing.
/// Test: `the_izzie_fixture_wakes_once_for_an_inbox_message`.
async fn absorb_unstored_overlays(bindings: &mut Vec<Binding>, name: &str) {
    let Ok(cfg) = crate::agents::AgentConfig::by_name_async(name).await else {
        return;
    };
    for channel in &cfg.channels {
        if channel.scope != crate::channels::ChannelScope::Assistant
            || !channel.target.is_empty()
            || bindings.iter().any(|binding| binding.id == channel.id)
        {
            continue;
        }
        bindings.push(Binding::from(channel));
    }
}

/// [`receive_inbound`] over already-loaded bindings and an injected dispatcher.
///
/// Why: everything worth testing about the inbound path — which binding is
/// selected, and whether the cycle's dispatch budget lets it fire — is in this
/// function, while everything untestable (the agents directory, the model call)
/// is in its caller and its dispatcher. #7427's per-cycle cap is a property of
/// this loop, so this is the seam that lets a test state it.
/// What: per assistant, selects the bound destination this event matches, asks
/// the provider's adapter for a wake prompt, spends the budget, and dispatches.
/// A binding the budget refuses is counted on the binding and left alone — the
/// event is already durably in the store, and it is still reported as `claimed`
/// so the listener wake does not pick up what a binding owns.
///
/// #7609: the per-assistant choice is no longer "does one of its own bindings
/// address this". [`crate::channels::dispatch::select_source`] picks ONE of
/// three sources — the assistant's own channel, a global channel routing to it,
/// or its legacy absorbed `[[listeners]]` binding — so every provider that
/// reaches this function gets global routing on the same terms, and no
/// assistant can be woken twice for one event.
/// Test: `gworkspace_binding_dispatches_once_per_poll_cycle`,
/// `a_failed_wake_prompt_leaves_the_cycle_dispatch_for_the_next_binding`,
/// `a_routed_global_and_its_legacy_binding_wake_once`.
#[allow(clippy::too_many_arguments)]
async fn receive_inbound_at(
    loaded: &[(String, Vec<Binding>)],
    provider: &str,
    channel: &str,
    event: &crate::listeners::store::StoredEvent,
    root: &std::path::Path,
    user: &crate::rbac::UserIdentity,
    allowed_personas: Option<&[String]>,
    budget: &mut DispatchBudget,
    globals: &[crate::channels::Channel],
    dispatcher: &dyn InboundDispatch,
) -> InboundOutcome {
    let mut outcome = InboundOutcome::default();
    for (name, bindings) in loaded {
        let allowed = allowed_personas.is_none_or(|allowed| allowed.iter().any(|v| v == name));
        let (bound, own) = receive_selection(bindings, provider, channel, event, allowed);
        let (routed, legacy) = if allowed {
            let channels: Vec<crate::channels::Channel> = bindings
                .iter()
                .map(crate::channels::Channel::from)
                .collect();
            crate::channels::dispatch::route_sources(globals, &channels, name, event)
        } else {
            (None, None)
        };
        let source = crate::channels::dispatch::select_source(
            own.is_some(),
            routed.is_some(),
            legacy.is_some(),
        );
        outcome.claimed |= bound || routed.is_some() || legacy.is_some();
        let selected = match source {
            crate::channels::dispatch::WakeSource::AssistantChannel => own.cloned(),
            crate::channels::dispatch::WakeSource::GlobalRouteTo => routed,
            crate::channels::dispatch::WakeSource::LegacyBinding => legacy,
            crate::channels::dispatch::WakeSource::NoWake => None,
        };
        let Some(binding) = selected else {
            continue;
        };
        outcome.source = Some(source);
        let binding = &binding;
        let wake = dispatcher
            .prepare(
                binding,
                crate::channels::InboundEvent { agent: name, event },
            )
            .await;
        let binding_id = binding.id.clone();
        let wake = match wake {
            Ok(Some(wake)) => wake,
            Ok(None) => continue,
            Err(e) => {
                // #7427: an inbound event that cannot even produce a prompt is
                // counted too, not dropped.
                tracing::error!(assistant = %name, binding = %binding_id, %e, "channel inbound could not be prepared");
                crate::channels::status::record_failure(name, &binding_id, &e.to_string());
                continue;
            }
        };
        // #7427 code-critic CRITICAL: the budget is spent HERE, after the
        // prompt exists and immediately before the model call, so a prompt
        // that failed to build never consumes the cycle's one dispatch.
        if !budget.take() {
            outcome.rate_limited += 1;
            crate::channels::status::record_rate_limited(name, &binding_id, &event.id);
            continue;
        }
        outcome.dispatched = true;
        dispatcher
            .dispatch(name, &binding_id, wake, root, user)
            .await;
    }
    outcome
}

#[cfg(test)]
mod receive_tests {
    use super::*;
    #[test]
    fn agent_channels_receive_never_falls_back_for_disabled_bound_destination() {
        let mut binding:Binding=serde_json::from_value(json!({"id":"team","name":"Team","provider":"slack","target":"C123","enabled":true,"receive_enabled":true,"filter":{"from":["Owner"]}})).unwrap();
        let event = crate::listeners::store::StoredEvent {
            id: "x".into(),
            listener_id: "slack".into(),
            provider: "slack".into(),
            event_type: "message.im".into(),
            ts: "now".into(),
            from: Some("Owner".into()),
            subject: None,
            snippet: Some("Hello".into()),
            included: true,
            labels: vec![],
        };
        let slack = |bindings: &[Binding], channel: &str, allowed: bool| {
            let (claimed, selected) =
                receive_selection(bindings, "slack", channel, &event, allowed);
            (claimed, selected.map(|b| b.id.clone()))
        };
        assert!(
            slack(std::slice::from_ref(&binding), "C123", true)
                .1
                .is_some()
        );
        assert!(!slack(std::slice::from_ref(&binding), "COTHER", true).0);
        assert!(
            slack(std::slice::from_ref(&binding), "C123", false)
                .1
                .is_none()
        );
        binding.receive_enabled = false;
        let (claimed, selected) = slack(std::slice::from_ref(&binding), "C123", true);
        assert!(claimed);
        assert!(selected.is_none());
        binding.receive_enabled = true;
        binding.filter.from = vec!["Other".into()];
        assert!(
            slack(std::slice::from_ref(&binding), "C123", true)
                .1
                .is_none()
        );
    }

    /// A Telegram update selects only the binding naming its own chat id, and a
    /// message from an unbound chat is never dispatched.
    ///
    /// Pre-change this test does not compile: `receive_selection` filtered on a
    /// literal `"slack"`, so no Telegram binding could ever be selected and the
    /// function took no provider to ask about.
    #[test]
    fn agent_channels_inbound_ignores_an_unbound_telegram_chat() {
        let binding: Binding = serde_json::from_value(json!({
            "id":"owner","name":"Owner DM","provider":"telegram","target":"123456",
            "enabled":true,"receive_enabled":true
        }))
        .unwrap();
        let event = crate::listeners::store::StoredEvent {
            id: "telegram:123456:42".into(),
            listener_id: "telegram".into(),
            provider: "telegram".into(),
            event_type: "message.private".into(),
            ts: "2026-09-11T00:00:00Z".into(),
            from: Some("Masa".into()),
            subject: None,
            snippet: Some("Move the 3pm".into()),
            included: true,
            labels: vec![],
        };
        let bound = std::slice::from_ref(&binding);
        let (claimed, selected) = receive_selection(bound, "telegram", "123456", &event, true);
        assert!(claimed);
        assert_eq!(selected.map(|b| b.id.as_str()), Some("owner"));

        // An unbound chat id: not claimed, not selected, so the gateway's own
        // fallback keeps handling it.
        let (claimed, selected) = receive_selection(bound, "telegram", "999999", &event, true);
        assert!(!claimed);
        assert!(selected.is_none());

        // The same chat id under another provider is a different destination.
        assert!(!receive_selection(bound, "slack", "123456", &event, true).0);
    }

    /// A Gmail message addressed by a gworkspace binding selects it; a message
    /// from anyone else is not claimed at all, so the listener wake still runs.
    ///
    /// Why: this pair is what makes the two inbound paths mutually exclusive —
    /// `claimed` is the flag `listeners::poll` reads to decide which one an
    /// event takes, and a `false` there is what keeps every unbound mailbox
    /// message on the pre-#7427 path.
    ///
    /// Pre-change this test does not compile: `receive_selection` filtered on a
    /// literal `"slack"` and compared targets by equality, so no gworkspace
    /// binding could ever be selected and the function took no provider.
    #[test]
    fn agent_channels_gmail_binding_claims_only_its_own_correspondent() {
        let binding: Binding = serde_json::from_value(json!({
            "id":"family","name":"Family mail","provider":"gworkspace",
            "target":"from:alice@example.com","enabled":true,"receive_enabled":true
        }))
        .unwrap();
        // A receive-enabled gworkspace binding saves. Pre-change this is the
        // first failure: `adapter("gworkspace")` was `None`, so `validate`
        // answered "Unsupported channel provider".
        assert!(binding.validate_in(&[]).is_ok());
        let mut wrong_target = binding.clone();
        wrong_target.target = "alice@example.com".into();
        assert!(wrong_target.validate_in(&[]).is_err());

        // Sending is a separate permission from receiving: this binding never
        // set `send_enabled`, so `authorized` refuses a send on it.
        let refused = authorized(std::slice::from_ref(&binding), "family", true).unwrap_err();
        assert_eq!(refused.0, StatusCode::FORBIDDEN);
        assert!(authorized(std::slice::from_ref(&binding), "family", false).is_ok());

        let event = |from: &str| crate::listeners::store::StoredEvent {
            id: "gmail-personal:19abc".into(),
            listener_id: "gmail-personal".into(),
            provider: "gmail".into(),
            event_type: "message.received".into(),
            ts: "2026-09-11T00:00:00Z".into(),
            from: Some(from.into()),
            subject: Some("Dinner".into()),
            snippet: Some("Are we still on?".into()),
            included: true,
            labels: vec!["INBOX".into()],
        };
        let bound = std::slice::from_ref(&binding);

        let alice = event("Alice <alice@example.com>");
        let (claimed, selected) = receive_selection(
            bound,
            "gworkspace",
            alice.from.as_deref().unwrap(),
            &alice,
            true,
        );
        assert!(claimed);
        assert_eq!(selected.map(|b| b.id.as_str()), Some("family"));

        let bob = event("bob@example.com");
        let (claimed, selected) = receive_selection(
            bound,
            "gworkspace",
            bob.from.as_deref().unwrap(),
            &bob,
            true,
        );
        assert!(!claimed, "an unbound sender must stay on the listener path");
        assert!(selected.is_none());

        // The sender address under another provider is a different destination.
        assert!(!receive_selection(bound, "slack", "alice@example.com", &alice, true).0);
    }

    /// Records who was dispatched, instead of starting a model turn, and can
    /// fail one named binding's prompt build.
    #[derive(Default)]
    struct RecordingDispatch {
        /// `(assistant, binding id)` per started turn, in order.
        dispatched: std::sync::Mutex<Vec<(String, String)>>,
        /// Binding id whose wake prompt fails to build, if any.
        fails: Option<String>,
    }

    #[async_trait::async_trait]
    impl InboundDispatch for RecordingDispatch {
        async fn prepare(
            &self,
            binding: &Binding,
            event: crate::channels::InboundEvent<'_>,
        ) -> Result<Option<crate::channels::WakePrompt>, crate::channels::ChannelError> {
            if self.fails.as_deref() == Some(binding.id.as_str()) {
                return Err(crate::channels::ChannelError::Provider {
                    provider: "gworkspace",
                });
            }
            prepare_via_adapter(binding, event).await
        }

        async fn dispatch(
            &self,
            agent: &str,
            binding_id: &str,
            _wake: crate::channels::WakePrompt,
            _root: &std::path::Path,
            _user: &crate::rbac::UserIdentity,
        ) {
            self.dispatched
                .lock()
                .unwrap()
                .push((agent.to_string(), binding_id.to_string()));
        }
    }

    /// Two Gmail messages from one bound correspondent in ONE poll cycle spend
    /// ONE assistant turn. The second is claimed, rate-limited, counted, and
    /// never handed to the model.
    ///
    /// Why: the cap is what bounds inference spend on a mailbox the operator
    /// does not control. A correspondent who sends five emails between two polls
    /// used to buy five LLM dispatches. The second message is still `claimed`,
    /// which is what keeps `listeners::poll` from handing it to the
    /// `[[listeners]]` wake as a consolation dispatch — so the cap holds across
    /// both paths, not just within this one.
    ///
    /// Pre-change (8621d99ea) this test does not compile: `receive_inbound` took
    /// no dispatch budget, returned a bare `bool`, and spawned
    /// `run_pm_task_with_persona` for every claimed message — two messages, two
    /// turns, with nothing in between to count or refuse them.
    #[tokio::test]
    async fn gworkspace_binding_dispatches_once_per_poll_cycle() {
        let agent = "fixture-cycle";
        let binding: Binding = serde_json::from_value(json!({
            "id":"family-cycle","name":"Family mail","provider":"gworkspace",
            "target":"from:alice@example.com","enabled":true,"receive_enabled":true
        }))
        .unwrap();
        let loaded = vec![(agent.to_string(), vec![binding])];
        let mail = |id: &str| crate::listeners::store::StoredEvent {
            id: format!("gmail-personal:{id}"),
            listener_id: "gmail-personal".into(),
            provider: "gmail".into(),
            event_type: "message.received".into(),
            ts: "2026-09-11T00:00:00Z".into(),
            from: Some("Alice <alice@example.com>".into()),
            subject: Some("Dinner".into()),
            snippet: Some("Are we still on?".into()),
            included: true,
            labels: vec!["INBOX".into()],
        };
        let user = crate::rbac::UserIdentity::new(
            "gworkspace:gmail-personal".to_string(),
            "Alice".to_string(),
            crate::rbac::ServiceTier::default(),
        );
        let dispatcher = RecordingDispatch::default();
        let root = std::path::Path::new("/nonexistent");

        // ONE budget for the whole cycle, exactly as `poll_once` holds it.
        let mut budget = DispatchBudget::one_per_cycle();
        let mut outcomes = Vec::new();
        for id in ["19abc", "19abd"] {
            let event = mail(id);
            outcomes.push(
                receive_inbound_at(
                    &loaded,
                    "gworkspace",
                    event.from.as_deref().unwrap(),
                    &event,
                    root,
                    &user,
                    None,
                    &mut budget,
                    &[],
                    &dispatcher,
                )
                .await,
            );
        }

        assert_eq!(
            outcomes[0],
            InboundOutcome {
                claimed: true,
                dispatched: true,
                rate_limited: 0,
                source: Some(crate::channels::dispatch::WakeSource::AssistantChannel),
            }
        );
        assert_eq!(
            outcomes[1],
            InboundOutcome {
                claimed: true,
                dispatched: false,
                rate_limited: 1,
                source: Some(crate::channels::dispatch::WakeSource::AssistantChannel),
            },
            "the second message belongs to the binding but must not buy a turn"
        );
        assert_eq!(
            dispatcher.dispatched.into_inner().unwrap(),
            vec![(agent.to_string(), "family-cycle".to_string())]
        );
        // Skipped, not silently dropped: the message id is on the counter the
        // channel view reads.
        assert_eq!(
            crate::channels::status::rate_limited(agent, "family-cycle"),
            1
        );
        assert!(budget.is_spent());
    }

    /// A binding whose wake prompt fails to build does not spend the cycle's
    /// one dispatch — the next matching binding still buys its turn.
    ///
    /// Why (#7427 code-critic MEDIUM): the ordering of `budget.take()` against
    /// the prompt build is the whole behaviour, and no existing test could tell
    /// the two orders apart. Spending the budget first means one unpreparable
    /// message silently costs every other assistant bound to the same mailbox
    /// its turn for that cycle — a failure on one binding rate-limiting a
    /// different one, with nothing in the logs connecting them.
    ///
    /// Moving `budget.take()` above the `match wake` fails this test twice: the
    /// second assistant comes back `dispatched: false, rate_limited: 1`, and
    /// the recorded dispatch list is empty.
    #[tokio::test]
    async fn a_failed_wake_prompt_leaves_the_cycle_dispatch_for_the_next_binding() {
        let unpreparable = "fixture-prepare-fails";
        let healthy = "fixture-prepare-ok";
        let binding = |id: &str| -> Binding {
            serde_json::from_value(json!({
                "id":id,"name":"Family mail","provider":"gworkspace",
                "target":"from:alice@example.com","enabled":true,"receive_enabled":true
            }))
            .unwrap()
        };
        // Both assistants are bound to the same correspondent, so one event
        // selects a binding under each — exactly the shape that shares a cycle.
        let loaded = vec![
            (unpreparable.to_string(), vec![binding("family-broken")]),
            (healthy.to_string(), vec![binding("family-healthy")]),
        ];
        let event = crate::listeners::store::StoredEvent {
            id: "gmail-personal:19abe".into(),
            listener_id: "gmail-personal".into(),
            provider: "gmail".into(),
            event_type: "message.received".into(),
            ts: "2026-09-11T00:00:00Z".into(),
            from: Some("Alice <alice@example.com>".into()),
            subject: Some("Dinner".into()),
            snippet: Some("Are we still on?".into()),
            included: true,
            labels: vec!["INBOX".into()],
        };
        let user = crate::rbac::UserIdentity::new(
            "gworkspace:gmail-personal".to_string(),
            "Alice".to_string(),
            crate::rbac::ServiceTier::default(),
        );
        let dispatcher = RecordingDispatch {
            fails: Some("family-broken".to_string()),
            ..Default::default()
        };

        let mut budget = DispatchBudget::one_per_cycle();
        let outcome = receive_inbound_at(
            &loaded,
            "gworkspace",
            event.from.as_deref().unwrap(),
            &event,
            std::path::Path::new("/nonexistent"),
            &user,
            None,
            &mut budget,
            &[],
            &dispatcher,
        )
        .await;

        assert_eq!(
            outcome,
            InboundOutcome {
                claimed: true,
                dispatched: true,
                rate_limited: 0,
                source: Some(crate::channels::dispatch::WakeSource::AssistantChannel),
            },
            "the failed prompt must not be charged to the cycle, and must not \
             rate-limit the binding behind it"
        );
        assert_eq!(
            dispatcher.dispatched.into_inner().unwrap(),
            vec![(healthy.to_string(), "family-healthy".to_string())]
        );
        // The failure is counted, not swallowed, and it bought nothing.
        assert_eq!(
            crate::channels::status::dispatch_failures(unpreparable, "family-broken"),
            1
        );
        assert_eq!(
            crate::channels::status::rate_limited(unpreparable, "family-broken"),
            0
        );
        assert_eq!(
            crate::channels::status::rate_limited(healthy, "family-healthy"),
            0
        );
        // Exactly one dispatch was bought for the cycle — by the binding that
        // had a prompt to dispatch.
        assert!(budget.is_spent());
    }

    /// The budget itself: one cycle spends once, a per-event caller never runs
    /// out.
    #[test]
    fn dispatch_budget_spends_once_per_cycle_and_never_for_per_event() {
        let mut cycle = DispatchBudget::one_per_cycle();
        assert!(!cycle.is_spent());
        assert!(cycle.take());
        assert!(cycle.is_spent());
        assert!(!cycle.take());
        assert!(!cycle.take());

        let mut per_event = DispatchBudget::PerEvent;
        assert!(per_event.take());
        assert!(per_event.take());
        assert!(!per_event.is_spent());
    }

    // ---------------------------------------------------------------------
    // #7609 slice 4: global routing, the legacy binding, and the guarantee
    // that one event buys exactly one wake per assistant.
    // ---------------------------------------------------------------------

    use crate::channels::dispatch::WakeSource;
    use crate::channels::{Channel, ChannelScope};

    /// The live global gmail channel: account-wide, INBOX ingest,
    /// `bob-personal`.
    fn global_gmail(route_to: &[&str]) -> Channel {
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
            route_to: route_to.iter().copied().map(String::from).collect(),
            ..Channel::default()
        }
    }

    /// Izzie's absorbed `[[listeners]]` binding, as `absorb_unstored_overlays`
    /// hands it to the inbound loop: no destination, no provider of its own.
    fn izzie_overlay() -> Binding {
        serde_json::from_value(json!({
            "id":"gmail-personal","name":"gmail-personal","provider":"","target":"",
            "enabled":true,"receive_enabled":true,
            "event_types":["message.received"],
            "filter":{"from":["*"],"exclude_labels":["CATEGORY_PROMOTIONS"]}
        }))
        .unwrap()
    }

    fn gmail_event(labels: &[&str]) -> crate::listeners::store::StoredEvent {
        crate::listeners::store::StoredEvent {
            id: "gmail-personal:19abc".into(),
            listener_id: "gmail-personal".into(),
            provider: "gmail".into(),
            event_type: "message.received".into(),
            ts: "2026-09-14T00:00:00Z".into(),
            from: Some("Alice <alice@example.com>".into()),
            subject: Some("Dinner".into()),
            snippet: Some("Are we still on?".into()),
            included: true,
            labels: labels.iter().copied().map(String::from).collect(),
        }
    }

    fn gworkspace_identity() -> crate::rbac::UserIdentity {
        crate::rbac::UserIdentity::new(
            "gworkspace:gmail-personal".to_string(),
            "Alice".to_string(),
            crate::rbac::ServiceTier::default(),
        )
    }

    /// One Gmail message through the whole inbound loop, with the dispatches it
    /// produced.
    async fn dispatches(
        loaded: &[(String, Vec<Binding>)],
        globals: &[Channel],
        event: &crate::listeners::store::StoredEvent,
    ) -> (InboundOutcome, Vec<(String, String)>) {
        let dispatcher = RecordingDispatch::default();
        let mut budget = DispatchBudget::one_per_cycle();
        let outcome = receive_inbound_at(
            loaded,
            "gworkspace",
            event.from.as_deref().unwrap_or_default(),
            event,
            std::path::Path::new("/nonexistent"),
            &gworkspace_identity(),
            None,
            &mut budget,
            globals,
            &dispatcher,
        )
        .await;
        (outcome, dispatcher.dispatched.into_inner().unwrap())
    }

    /// The live izzie fixture, BEFORE the `route_to` backfill: an INBOX message
    /// wakes her exactly once through the legacy binding, and a
    /// `CATEGORY_PROMOTIONS` message wakes nobody at all.
    ///
    /// Why: this is the one configuration a live assistant depends on. Before
    /// slice 4 it reached her through `listeners::wake`, because her binding
    /// was only in `agent.toml`; it reaches her through the channel path now,
    /// and the count on each side of that move has to be identical.
    #[tokio::test]
    async fn the_izzie_fixture_wakes_once_for_an_inbox_message() {
        let loaded = vec![("fixture-izzie-legacy".to_string(), vec![izzie_overlay()])];
        let globals = vec![global_gmail(&[])];

        let (outcome, dispatched) = dispatches(&loaded, &globals, &gmail_event(&["INBOX"])).await;
        assert_eq!(
            outcome,
            InboundOutcome {
                claimed: true,
                dispatched: true,
                rate_limited: 0,
                source: Some(WakeSource::LegacyBinding),
            }
        );
        assert_eq!(
            dispatched,
            vec![(
                "fixture-izzie-legacy".to_string(),
                "gmail-personal".to_string()
            )]
        );

        let (outcome, dispatched) =
            dispatches(&loaded, &globals, &gmail_event(&["CATEGORY_PROMOTIONS"])).await;
        assert_eq!(outcome, InboundOutcome::default());
        assert!(
            dispatched.is_empty(),
            "an excluded label must buy no turn, and `claimed` false leaves the \
             listener wake free to agree"
        );
    }

    /// The SAME fixture AFTER the `route_to` backfill: one wake, not two.
    ///
    /// Why (#7609, the (2)-excludes-(3) rule): the backfill leaves both a
    /// global naming izzie in `route_to` AND the legacy binding it was derived
    /// from. Letting each fire independently wakes her twice for one message —
    /// two prompts, two model turns, against a cycle budget of one.
    ///
    /// Pre-fix proof: make `route_sources` return BOTH candidates (drop the
    /// `if routed` split) and this test fails with two entries in `dispatched`.
    #[tokio::test]
    async fn a_routed_global_and_its_legacy_binding_wake_once() {
        let loaded = vec![("fixture-izzie-routed".to_string(), vec![izzie_overlay()])];
        let globals = vec![global_gmail(&["fixture-izzie-routed"])];

        let (outcome, dispatched) = dispatches(&loaded, &globals, &gmail_event(&["INBOX"])).await;
        assert_eq!(
            outcome,
            InboundOutcome {
                claimed: true,
                dispatched: true,
                rate_limited: 0,
                source: Some(WakeSource::GlobalRouteTo),
            },
            "the routed global is the one source that fires"
        );
        assert_eq!(
            dispatched,
            vec![(
                "fixture-izzie-routed".to_string(),
                "gmail-personal".to_string()
            )],
            "exactly one wake: the legacy binding for the same pair must not \
             also dispatch"
        );

        // The overlay's filter still governs after the backfill.
        let (outcome, dispatched) =
            dispatches(&loaded, &globals, &gmail_event(&["CATEGORY_PROMOTIONS"])).await;
        assert_eq!(outcome, InboundOutcome::default());
        assert!(dispatched.is_empty());
    }

    /// `route_to` never follows a message to a provider it does not own, and a
    /// targeted Slack binding still claims its own DM through source (1) alone.
    #[tokio::test]
    async fn the_cto_assistant_slack_binding_still_claims_its_dm() {
        let dm: Binding = serde_json::from_value(json!({
            "id":"cto-dm","name":"CTO DM","provider":"slack","target":"D0AM8GWJLFR",
            "enabled":true,"receive_enabled":true,"credential_ref":"slack"
        }))
        .unwrap();
        let loaded = vec![
            ("fixture-cto".to_string(), vec![dm]),
            ("fixture-izzie-dm".to_string(), vec![izzie_overlay()]),
        ];
        // A global gmail channel routing to izzie must not follow a Slack
        // message anywhere.
        let globals = vec![global_gmail(&["fixture-izzie-dm"])];
        let event = crate::listeners::store::StoredEvent {
            id: "slack:D0AM8GWJLFR:1".into(),
            listener_id: "slack".into(),
            provider: "slack".into(),
            event_type: "message.im".into(),
            ts: "2026-09-14T00:00:00Z".into(),
            from: Some("Masa".into()),
            subject: None,
            snippet: Some("standup?".into()),
            included: true,
            labels: vec![],
        };
        let dispatcher = RecordingDispatch::default();
        let mut budget = DispatchBudget::PerEvent;
        let outcome = receive_inbound_at(
            &loaded,
            "slack",
            "D0AM8GWJLFR",
            &event,
            std::path::Path::new("/nonexistent"),
            &gworkspace_identity(),
            None,
            &mut budget,
            &globals,
            &dispatcher,
        )
        .await;
        assert_eq!(outcome.source, Some(WakeSource::AssistantChannel));
        assert_eq!(
            dispatcher.dispatched.into_inner().unwrap(),
            vec![("fixture-cto".to_string(), "cto-dm".to_string())],
            "only the binding that names this DM claims it"
        );
    }

    /// A roster that cannot be enumerated is logged and claims nothing — never
    /// silently absorbed into "no assistant is bound".
    #[test]
    fn an_unreadable_roster_is_logged_and_claims_nothing() {
        assert_eq!(
            roster_or_warn(Ok(vec!["izzie".to_string()]), "gworkspace"),
            Some(vec!["izzie".to_string()])
        );
        assert_eq!(
            roster_or_warn(Err(anyhow::anyhow!("permission denied")), "gworkspace"),
            None
        );
    }
}
