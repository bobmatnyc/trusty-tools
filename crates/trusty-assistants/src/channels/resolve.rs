//! Global-versus-assistant channel precedence (#7609 slice 1).
//!
//! Why: once both files hold `[[channels]]`, one assistant can bind the same
//! provider destination the harness already binds globally — a personal Slack
//! DM that is also in a harness-wide channel list, say. Without one stated
//! rule, whichever list a caller happened to read first would win, differently
//! per call site. The rule is: the assistant's own binding wins, because it is
//! the more specific statement about that destination.
//! What: [`resolve_channels`] keys on `(provider, target)` — NOT on `id`,
//! which is only a per-file storage key and can legitimately differ between
//! the two files for the same destination. Global order is preserved so the
//! result reads like the global list with substitutions applied; assistant
//! channels that match no global one are appended in their own order.
//! Nothing calls this yet — dispatch (slice 4) is its first consumer.
//! Test: this module's own tests.

// #7609: slice 1 precedence rule, per the issue body's "per-assistant binding
// wins over a global one for the same provider+target".
use super::model::Channel;

/// The effective channel list for one assistant.
///
/// Why/What: see the module doc. `global` and `assistant` are both left
/// untouched; the result owns its entries.
/// Test: `assistant_channel_replaces_the_global_one_in_place`,
/// `an_unmatched_assistant_channel_is_appended`,
/// `an_empty_target_only_collides_with_another_empty_target`.
pub fn resolve_channels(global: &[Channel], assistant: &[Channel]) -> Vec<Channel> {
    let mut resolved: Vec<Channel> = global.to_vec();
    for own in assistant {
        match resolved
            .iter_mut()
            .find(|existing| existing.addresses_same(own))
        {
            Some(existing) => *existing = own.clone(),
            None => resolved.push(own.clone()),
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::model::ChannelScope;

    fn channel(id: &str, provider: &str, target: &str, scope: ChannelScope) -> Channel {
        Channel {
            id: id.into(),
            name: id.into(),
            provider: provider.into(),
            target: target.into(),
            scope,
            ..Channel::default()
        }
    }

    #[test]
    fn assistant_channel_replaces_the_global_one_in_place() {
        let global = vec![
            channel("a", "slack", "D1", ChannelScope::Global),
            channel("b", "telegram", "42", ChannelScope::Global),
        ];
        let assistant = vec![channel("mine", "slack", "D1", ChannelScope::Assistant)];
        let resolved = resolve_channels(&global, &assistant);
        assert_eq!(
            resolved.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["mine", "b"],
            "the assistant entry takes the global one's position"
        );
        assert_eq!(resolved[0].scope, ChannelScope::Assistant);
    }

    #[test]
    fn an_unmatched_assistant_channel_is_appended() {
        let global = vec![channel("a", "slack", "D1", ChannelScope::Global)];
        let assistant = vec![channel("t", "telegram", "42", ChannelScope::Assistant)];
        let resolved = resolve_channels(&global, &assistant);
        assert_eq!(
            resolved.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "t"]
        );
    }

    #[test]
    fn an_empty_target_only_collides_with_another_empty_target() {
        let global = vec![channel("gmail-personal", "gmail", "", ChannelScope::Global)];
        let targeted = vec![channel(
            "x",
            "gmail",
            "label:INBOX",
            ChannelScope::Assistant,
        )];
        assert_eq!(
            resolve_channels(&global, &targeted).len(),
            2,
            "a targeted assistant channel does not shadow the account-wide one"
        );
        let account_wide = vec![channel("x", "gmail", "", ChannelScope::Assistant)];
        let resolved = resolve_channels(&global, &account_wide);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "x");
    }

    #[test]
    fn nothing_on_either_side_resolves_to_nothing() {
        assert!(resolve_channels(&[], &[]).is_empty());
        let global = vec![channel("a", "slack", "D1", ChannelScope::Global)];
        assert_eq!(resolve_channels(&global, &[]), global);
    }
}
