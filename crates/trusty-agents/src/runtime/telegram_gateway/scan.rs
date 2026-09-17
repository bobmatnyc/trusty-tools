//! Which Telegram bots this host must poll, and for whom (#8190).
//!
//! Why (owner ruling 2026-09-16): each assistant has its own Telegram bot, so
//! the gateway cannot resolve one token at startup and deliver to whoever
//! matches. The start decision is now a SET: every enabled receiving Telegram
//! destination on this host, grouped by the token its `credential_ref`
//! resolves to. Two assistants with two tokens are two pollers; two bindings
//! sharing one token are one poller that dispatches by binding.
//!
//! What: [`scan`] reads the same two sources the inbound path claims from — an
//! assistant's own binding, and a harness-wide `[[channels]]` telegram entry
//! with `route_to` — resolves each one's credential, and hands the entries to
//! [`group_by_token`]. Grouping is split out because it is the whole selection
//! rule and it must be testable without a credential store on disk. A
//! destination whose token will not resolve is a [`SkippedBinding`], recorded
//! and reported, never a silent drop.
//!
//! Test: `super::tests` — `telegram_gateway_two_tokens_are_two_pollers`,
//! `telegram_gateway_one_token_two_bindings_is_one_poller`,
//! `telegram_gateway_an_unresolvable_token_is_skipped_with_a_reason`.

use std::collections::BTreeMap;

use anyhow::Result;
use tracing::warn;
use trusty_common::credentials::Secret;

use crate::telegram::{BotKey, TelegramBot};

/// One enabled receiving Telegram destination the scan found.
///
/// Why: the two sources (an assistant binding, a routed global channel) differ
/// only in where the owner's name comes from, so collapsing them into one shape
/// before grouping keeps [`group_by_token`] free of source-specific rules.
/// Test: `telegram_gateway_two_tokens_are_two_pollers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TelegramEntry {
    /// The assistant this destination delivers to.
    pub owner: String,
    /// The binding or channel id, for the skip report.
    pub binding_id: String,
    /// The credential reference naming this destination's bot; `None` means
    /// the adapter's default key.
    pub credential_ref: Option<String>,
}

/// A destination this host cannot poll for, and why.
///
/// Why (#8190): "a binding with no resolvable token is skipped with the reason
/// recorded" is a stated requirement, and a reason nobody can read is not a
/// reason. This rides into the status surface.
/// Test: `telegram_gateway_an_unresolvable_token_is_skipped_with_a_reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SkippedBinding {
    pub owner: String,
    pub binding_id: String,
    pub reason: String,
}

/// Everything one scan learned.
pub(super) struct BotScan {
    /// One entry per distinct bot token, ordered by key so a rescan that found
    /// the same set produces the same order.
    pub bots: Vec<TelegramBot>,
    pub skipped: Vec<SkippedBinding>,
    /// Degradations that are not about one binding — an unreadable roster, an
    /// unreadable channel file.
    pub warnings: Vec<String>,
}

/// Group resolved destinations into one bot per distinct token.
///
/// Why: this is the ruling, expressed as code. Identity is the TOKEN, not the
/// credential reference — two references pointing at one stored credential are
/// one bot, and starting two pollers for them would have Telegram terminate one
/// of the two `getUpdates` loops.
/// What: `resolve` maps a credential reference to its token or to a failure
/// message; it is a parameter so a test never needs a keychain. Owners and
/// reference labels accumulate per token, deduplicated and ordered.
/// Test: `telegram_gateway_two_tokens_are_two_pollers`,
/// `telegram_gateway_one_token_two_bindings_is_one_poller`,
/// `telegram_gateway_an_unresolvable_token_is_skipped_with_a_reason`.
pub(super) fn group_by_token(
    entries: &[TelegramEntry],
    resolve: &dyn Fn(Option<&str>) -> Result<String, String>,
) -> (Vec<TelegramBot>, Vec<SkippedBinding>) {
    /// Owners and labels accumulated for one token before the bot is built.
    struct Accum {
        token: String,
        owners: Vec<String>,
        refs: Vec<String>,
    }
    let mut by_key: BTreeMap<BotKey, Accum> = BTreeMap::new();
    let mut skipped = Vec::new();
    for entry in entries {
        let token = match resolve(entry.credential_ref.as_deref()) {
            Ok(token) => token,
            Err(reason) => {
                skipped.push(SkippedBinding {
                    owner: entry.owner.clone(),
                    binding_id: entry.binding_id.clone(),
                    reason,
                });
                continue;
            }
        };
        let label = entry
            .credential_ref
            .clone()
            .unwrap_or_else(|| "telegram".to_string());
        let accum = by_key
            .entry(BotKey::from_token(&token))
            .or_insert_with(|| Accum {
                token,
                owners: Vec::new(),
                refs: Vec::new(),
            });
        if !accum.owners.contains(&entry.owner) {
            accum.owners.push(entry.owner.clone());
        }
        if !accum.refs.contains(&label) {
            accum.refs.push(label);
        }
    }
    let bots = by_key
        .into_values()
        .map(|mut a| {
            a.owners.sort();
            a.refs.sort();
            TelegramBot::new(Secret::new(a.token), Some(a.owners), a.refs)
        })
        .collect();
    (bots, skipped)
}

/// Whether one saved binding wants Telegram updates delivered to it.
///
/// Why: the gateway's start condition has to be the inbound path's claim
/// condition — `first_telegram_credential_ref` selects on exactly these three
/// flags, so a host that would claim an event is a host that must poll.
/// What: a blank target is excluded — an unresolved overlay addresses no chat.
/// Test: `telegram_gateway_counts_an_enabled_receiving_binding`,
/// `telegram_gateway_ignores_a_disabled_or_send_only_binding`.
pub(super) fn binding_receives_telegram(
    binding: &crate::api::server::agent_channels::Binding,
) -> bool {
    binding.provider == "telegram"
        && binding.enabled
        && binding.receive_enabled
        && !binding.target.is_empty()
}

/// Whether one harness-wide channel delivers Telegram updates to an assistant.
///
/// Test: `telegram_gateway_counts_a_routed_global_channel`.
pub(super) fn global_channel_receives_telegram(channel: &crate::channels::Channel) -> bool {
    channel.provider == "telegram"
        && channel.enabled
        && channel.receive_enabled
        && !channel.route_to.is_empty()
}

/// The assistant roster, or an empty scan with a recorded warning.
///
/// Why (#8190, fail-open check): a roster this host cannot enumerate is not "no
/// assistant wants Telegram" — it is an unanswered question, and answering it
/// `false` silently leaves the gateway off with nothing saying why. The scan
/// still proceeds as if empty, because starting a poller on a host whose
/// bindings we cannot read would hold a lock for a gateway that can deliver to
/// nobody.
/// Test: `telegram_gateway_roster_failure_warns_and_scans_nothing`.
pub(super) fn roster_or_warn(
    roster: Result<Vec<String>>,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    roster.unwrap_or_else(|e| {
        let warning = format!(
            "the assistant roster could not be read ({e:#}); the gateway start scan sees no \
             Telegram channels and the gateway stays off"
        );
        warn!("telegram gateway: {warning} (#8190)");
        warnings.push(warning);
        Vec::new()
    })
}

/// One assistant's bindings, or none with a recorded warning.
///
/// Why (#8190, fail-open check): a single unreadable channel file must not
/// decide the gateway for every OTHER assistant, and must not vanish silently
/// either — the same rule `receive_inbound` applies to the identical read.
/// Test: `telegram_gateway_an_unreadable_channel_file_warns_and_is_skipped`.
pub(super) fn bindings_or_warn<P, R, B, E: std::fmt::Debug>(
    name: &str,
    loaded: Result<(P, R, Vec<B>), E>,
    warnings: &mut Vec<String>,
) -> Vec<B> {
    loaded.map(|(_, _, bindings)| bindings).unwrap_or_else(|e| {
        let warning = format!(
            "assistant `{name}`: its channels could not be read ({e:?}); it is skipped by the \
             gateway start scan"
        );
        warn!("telegram gateway: {warning} (#8190)");
        warnings.push(warning);
        Vec::new()
    })
}

/// Read every enabled receiving Telegram destination on this host.
///
/// Why: the one place the gateway learns what to poll. Re-run on every rescan
/// (#8190 finding 4), so a binding an operator enables after startup is picked
/// up without restarting the API host.
/// What: globals first (a `[[channels]]` telegram entry with `route_to`), then
/// each assistant's own bindings, then grouping by resolved token. Every
/// failure degrades to a warning or a skip; this function cannot fail.
/// Test: `telegram_gateway_two_tokens_are_two_pollers` covers the grouping this
/// delegates to; the I/O half has no test bed without an agents directory.
pub(super) async fn scan() -> BotScan {
    let mut warnings = Vec::new();
    let mut entries = Vec::new();

    let globals = crate::mcp::config::GlobalConfig::load().await.channels;
    for channel in globals
        .iter()
        .filter(|c| global_channel_receives_telegram(c))
    {
        for owner in &channel.route_to {
            entries.push(TelegramEntry {
                owner: owner.clone(),
                binding_id: channel.id.clone(),
                credential_ref: channel.credential_ref.clone(),
            });
        }
    }

    let dirs = crate::agents::agents_dir_candidates();
    let roster = roster_or_warn(
        crate::listeners::wake::candidate_agent_names().await,
        &mut warnings,
    );
    for name in roster {
        let loaded = crate::api::server::agent_channels::load_at(&dirs, &name).await;
        for binding in bindings_or_warn(&name, loaded, &mut warnings)
            .iter()
            .filter(|b| binding_receives_telegram(b))
        {
            entries.push(TelegramEntry {
                owner: name.clone(),
                binding_id: binding.id.clone(),
                credential_ref: binding.credential_ref.clone(),
            });
        }
    }

    let (bots, skipped) = group_by_token(&entries, &|reference| {
        crate::channels::telegram_poll_token(reference)
            .map(|t| t.expose().clone())
            .map_err(|e| e.to_string())
    });
    BotScan {
        bots,
        skipped,
        warnings,
    }
}
