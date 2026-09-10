//! Declarative listener config shapes (#3820, DOC-54 SPEC-AGENTS-04/06).
//!
//! Why: Listeners are the third leg of the agent-config triple (stores /
//! tools / listeners) and use TWO-STAGE filtering: a harness-level
//! `[[listeners]]` array in `~/.trusty-agents/config.toml` (stage one —
//! what gets ingested from the provider at all) and a per-agent
//! `[[listeners]]` array in each agent's `agent.toml` (stage two — which of
//! the ingested events wake THAT agent). Both are pure serde data structs —
//! per the standing "agents are declarative-only" rule (#2791), no behavior
//! lives here, only shape + defaults.
//! What: [`ListenerConfig`]/[`ListenerFilter`] mirror the `config.toml`
//! sketch in DOC-54 §7.5; [`AgentListenerBinding`]/[`AgentBindingFilter`]
//! mirror the `agent.toml` sketch. `ListenerConfig::enabled` defaults to
//! `false` (safe-by-default — see `crate::listeners::poll`'s module doc for
//! why this matters for a config file this code never writes itself).
//! Test: `listener_config_parses_gmail_example`,
//! `listener_config_defaults_disabled_and_poll_interval`,
//! `agent_listener_binding_parses_filter`,
//! `listener_config_clamps_poll_interval_below_floor`,
//! `listener_config_leaves_poll_interval_above_floor_untouched`.

use serde::{Deserialize, Deserializer, Serialize};

fn default_poll_interval_secs() -> u64 {
    // DOC-54 §7.3.1: history-poll fallback default (2-5 min band); 180s
    // sits in the middle, quota-conscious without being sluggish for a demo.
    180
}

/// Floor for `poll_interval_secs` (#3820 code-critic MEDIUM-2).
///
/// Why: This listener runs against Bob's REAL personal Gmail account. A
/// hand-edited (or copy-pasted-wrong) `config.toml` with e.g.
/// `poll_interval_secs = 1` would hammer the Gmail API at up to 1 req/sec
/// indefinitely — quota exhaustion at best, an account-level abuse flag at
/// worst. 15s is well under even the aggressive Pub/Sub-pull band (DOC-54
/// §7.3.1's "~20-30s") while still guarding against a pathological
/// near-zero value; it is NOT a recommendation (the documented defaults —
/// 60-180s for history-poll — remain the sane operating range).
pub const MIN_POLL_INTERVAL_SECS: u64 = 15;

/// Deserialize `poll_interval_secs`, clamping any value below
/// [`MIN_POLL_INTERVAL_SECS`] up to the floor and logging a warning so the
/// clamp is visible rather than a silent surprise.
///
/// Why: Clamping (rather than rejecting the whole config with a hard parse
/// error) keeps a typo'd interval from taking down config loading entirely
/// — `GlobalConfig::load()` already degrades to defaults on any outright
/// parse error, which would be a worse failure mode here than "run a bit
/// more conservatively than the operator typed."
/// Test: `listener_config_clamps_poll_interval_below_floor`,
/// `listener_config_leaves_poll_interval_above_floor_untouched`.
fn deserialize_clamped_poll_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    if value < MIN_POLL_INTERVAL_SECS {
        tracing::warn!(
            configured = value,
            floor = MIN_POLL_INTERVAL_SECS,
            "listener poll_interval_secs below floor; clamping up"
        );
        Ok(MIN_POLL_INTERVAL_SECS)
    } else {
        Ok(value)
    }
}

fn default_transport() -> String {
    "history-poll".to_string()
}

/// One harness-level listener definition (`config.toml` `[[listeners]]`,
/// stage-one filter).
///
/// Why: `enabled` defaults to `false` so a config file that merely declares
/// a listener (e.g. shipped as a documented example a user pastes in) never
/// silently starts polling a live account until the operator opts in
/// explicitly — the polling engine (`crate::listeners::poll`) skips every
/// listener with `enabled = false`.
/// What: `name` is the listener's stable id, referenced by `agent.toml`
/// binding entries. `connector` selects the provider implementation
/// (`"gmail"` is the only one wired to a real poller today; unrecognized
/// values are logged and skipped, never a hard error, so a config with a
/// forward-looking `"google-calendar"` entry doesn't crash the process).
/// `identity` is the `trusty-gworkspace` profile/account name (e.g.
/// `"bob-personal"`) — `None` uses that crate's default profile.
/// Test: `listener_config_parses_gmail_example`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ListenerConfig {
    pub name: String,
    pub connector: String,
    #[serde(default)]
    pub identity: Option<String>,
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(default)]
    pub enabled: bool,
    /// Poll interval, floored at [`MIN_POLL_INTERVAL_SECS`] on parse (see
    /// `deserialize_clamped_poll_interval`) — a value below the floor is
    /// clamped up, not rejected.
    #[serde(
        default = "default_poll_interval_secs",
        deserialize_with = "deserialize_clamped_poll_interval"
    )]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub filter: ListenerFilter,
}

/// Stage-one (listener-level) filter — narrows what is even fetched from
/// the provider. DOC-54 §5.3 / §7.5.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct ListenerFilter {
    /// Gmail label ids to scope `history.list` to (e.g. `["INBOX"]`).
    /// Empty = no label restriction.
    #[serde(default)]
    pub label_ids: Vec<String>,
}

impl ListenerFilter {
    /// OR across configured labels, including multi-label history responses.
    pub fn matches_labels(&self, labels: &[String]) -> bool {
        self.label_ids.is_empty() || self.label_ids.iter().any(|label| labels.contains(label))
    }

    /// The single Gmail label to pass to `history.list`'s `labelId` param,
    /// if the filter names exactly one.
    ///
    /// Why: Gmail's `history.list` accepts at most one `labelId` query
    /// param (unlike `messages.list`'s multi-label `labelIds`); a listener
    /// filter naming zero or >1 labels can't be expressed as that one param,
    /// so this returns `None` for those cases and the poller falls back to
    /// fetching unfiltered history and applying `matches_labels` client-side
    /// before persisting events or waking an assistant.
    /// What: `Some(label)` only when exactly one label id is present.
    /// Test: `listener_filter_single_label_returns_some`,
    /// `listener_filter_empty_or_multi_label_returns_none`.
    pub fn single_gmail_label(&self) -> Option<&str> {
        match self.label_ids.as_slice() {
            [only] => Some(only.as_str()),
            _ => None,
        }
    }
}

/// A per-agent listener binding (`agent.toml` `[[listeners]]`, stage-two
/// filter). DOC-54 §5.3 / §7.5.
///
/// Why: An agent opts INTO waking on a named harness listener by declaring
/// one of these; absence means the agent never wakes for any event (safe
/// default — matches `[tools].allow`'s deny-by-default posture).
/// What: `name` must match a `ListenerConfig::name` from `config.toml`.
/// `event_types` further narrows which normalized event types wake this
/// agent (e.g. `["message.received"]`); empty means "any event type this
/// listener emits". `filter` applies sender/label narrowing on top.
/// Test: `agent_listener_binding_parses_filter`,
/// `agent_listener_binding_defaults_event_types_empty`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentListenerBinding {
    pub name: String,
    #[serde(default = "default_binding_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub event_types: Vec<String>,
    #[serde(default)]
    pub filter: AgentBindingFilter,
}

/// Stage-two (per-agent-binding) filter — DOC-54 §5.3 / §7.5.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBindingFilter {
    #[serde(default)]
    pub include_labels: Vec<String>,
    #[serde(default)]
    pub subject_contains: Vec<String>,
    #[serde(default)]
    pub snippet_contains: Vec<String>,
    /// Sender glob patterns (`*` = suffix/prefix wildcard, matched via the
    /// same `match_any_glob` semantics as `[tools].allow`). Empty = any
    /// sender.
    #[serde(default)]
    pub from: Vec<String>,
    /// Gmail label ids that, if present on the event, EXCLUDE it from
    /// waking this agent (e.g. `["PROMOTIONS"]`). Empty = no exclusions.
    #[serde(default)]
    pub exclude_labels: Vec<String>,
}

fn default_binding_enabled() -> bool {
    true
}
impl Default for AgentListenerBinding {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            instructions: String::new(),
            event_types: vec![],
            filter: AgentBindingFilter::default(),
        }
    }
}
impl AgentListenerBinding {
    /// Invalid hand-edited bindings fail closed in the wake matcher as well as API writes.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty()
            || self.name.len() > 128
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err("Listener name must be a simple identifier".into());
        }
        if self.instructions.chars().count() > 8000 || self.instructions.contains('\0') {
            return Err("Instructions must be at most 8000 characters without NUL".into());
        }
        for values in [
            &self.event_types,
            &self.filter.from,
            &self.filter.include_labels,
            &self.filter.exclude_labels,
            &self.filter.subject_contains,
            &self.filter.snippet_contains,
        ] {
            if values.len() > 32
                || values.iter().any(|v| {
                    v.trim().is_empty()
                        || v.chars().count() > 256
                        || v.chars().any(char::is_control)
                })
            {
                return Err("Filters allow at most 32 nonblank values of at most 256 characters without control characters".into());
            }
        }
        if self.filter.from.iter().any(|p| {
            let n = p.chars().filter(|c| *c == '*').count();
            n > 1 || (n == 1 && !p.starts_with('*') && !p.ends_with('*'))
        }) {
            return Err("Sender filters support one wildcard at the start or end".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_config_parses_gmail_example() {
        let toml_str = r#"
name = "gmail-personal"
connector = "gmail"
identity = "bob-personal"
transport = "history-poll"
enabled = true
poll_interval_secs = 120
filter = { label_ids = ["INBOX"] }
"#;
        let cfg: ListenerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.name, "gmail-personal");
        assert_eq!(cfg.connector, "gmail");
        assert_eq!(cfg.identity.as_deref(), Some("bob-personal"));
        assert!(cfg.enabled);
        assert_eq!(cfg.poll_interval_secs, 120);
        assert_eq!(cfg.filter.label_ids, vec!["INBOX".to_string()]);
    }

    #[test]
    fn listener_config_defaults_disabled_and_poll_interval() {
        let toml_str = r#"
name = "gmail-personal"
connector = "gmail"
"#;
        let cfg: ListenerConfig = toml::from_str(toml_str).unwrap();
        assert!(!cfg.enabled, "enabled must default to false");
        assert_eq!(cfg.poll_interval_secs, 180);
        assert_eq!(cfg.transport, "history-poll");
        assert!(cfg.identity.is_none());
    }

    #[test]
    fn listener_config_clamps_poll_interval_below_floor() {
        let toml_str = r#"
name = "gmail-personal"
connector = "gmail"
poll_interval_secs = 1
"#;
        let cfg: ListenerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            cfg.poll_interval_secs, MIN_POLL_INTERVAL_SECS,
            "a below-floor value must be clamped up to the floor, not passed through"
        );
    }

    #[test]
    fn listener_config_leaves_poll_interval_above_floor_untouched() {
        let toml_str = r#"
name = "gmail-personal"
connector = "gmail"
poll_interval_secs = 20
"#;
        let cfg: ListenerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.poll_interval_secs, 20);
    }

    #[test]
    fn listener_config_poll_interval_exactly_at_floor_is_unchanged() {
        let toml_str = format!(
            "name = \"gmail-personal\"\nconnector = \"gmail\"\npoll_interval_secs = {MIN_POLL_INTERVAL_SECS}\n"
        );
        let cfg: ListenerConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(cfg.poll_interval_secs, MIN_POLL_INTERVAL_SECS);
    }

    #[test]
    fn agent_listener_binding_parses_filter() {
        let toml_str = r#"
name = "gmail-personal"
event_types = ["message.received"]
filter = { from = ["*@family.com"], exclude_labels = ["PROMOTIONS"] }
"#;
        let binding: AgentListenerBinding = toml::from_str(toml_str).unwrap();
        assert_eq!(binding.name, "gmail-personal");
        assert_eq!(binding.event_types, vec!["message.received".to_string()]);
        assert_eq!(binding.filter.from, vec!["*@family.com".to_string()]);
        assert_eq!(
            binding.filter.exclude_labels,
            vec!["PROMOTIONS".to_string()]
        );
    }

    #[test]
    fn agent_listener_binding_defaults_event_types_empty() {
        let toml_str = r#"name = "gmail-personal""#;
        let binding: AgentListenerBinding = toml::from_str(toml_str).unwrap();
        assert!(binding.event_types.is_empty());
        assert!(binding.filter.from.is_empty());
    }

    #[test]
    fn listener_filter_single_label_returns_some() {
        let f = ListenerFilter {
            label_ids: vec!["INBOX".to_string()],
        };
        assert_eq!(f.single_gmail_label(), Some("INBOX"));
    }

    #[test]
    fn listener_filter_empty_or_multi_label_returns_none() {
        assert_eq!(ListenerFilter::default().single_gmail_label(), None);
        let f = ListenerFilter {
            label_ids: vec!["INBOX".to_string(), "IMPORTANT".to_string()],
        };
        assert_eq!(f.single_gmail_label(), None);
    }
    #[test]
    fn listener_filter_multiple_labels_are_enforced() {
        let filter = ListenerFilter {
            label_ids: vec!["INBOX".into(), "IMPORTANT".into()],
        };
        assert!(filter.matches_labels(&["INBOX".into()]));
        assert!(!filter.matches_labels(&["PROMOTIONS".into()]));
        assert!(!filter.matches_labels(&[]));
    }
    #[test]
    fn listener_defaults_and_invalid_rules_fail_closed() {
        let binding: AgentListenerBinding = toml::from_str("name='mail'").unwrap();
        assert!(binding.enabled);
        assert!(binding.instructions.is_empty());
        assert!(binding.validate().is_ok());
        let mut invalid = binding.clone();
        invalid.filter.from = vec!["bad*middle".into()];
        assert!(invalid.validate().is_err());
        invalid.filter.from.clear();
        invalid.instructions = "x".repeat(8001);
        assert!(invalid.validate().is_err());
        assert!(
            toml::from_str::<AgentListenerBinding>("name='mail'\n[filter]\nunknown=['oops']")
                .is_err()
        );
    }
}
