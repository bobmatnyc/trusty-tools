//! [`ChannelPolicy`]: the validated route table for every channel.
//!
//! Why: one deny-by-default table answers "may this go out" and "may this
//! come in" for gchat, Slack and Telegram (#8454 S1).
//! What: [`ChannelPolicy::build`] validates already-parsed route data and
//! fails on any broken load rule; it never returns a trimmed table. The
//! checks live in [`crate::policy::check`].
//! Test: `src/policy/tests/build.rs`.

use std::collections::{BTreeSet, HashMap};

use crate::policy::error::PolicyError;
use crate::policy::types::{Channel, RateLimit, RateLimitSpec, Route, RouteSpec};

/// Already-parsed policy data: an optional default rate limit and the routes.
///
/// Why: S1 is loader-free; S2 parses a file into this.
/// What: an absent `rate_limit` takes [`RateLimit::DEFAULT`], never
/// "unlimited". No routes means every check denies.
/// Test: `absent_rate_limit_uses_builtin_not_unlimited`,
/// `empty_policy_denies_all`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PolicySpec {
    /// The per-binding default; `None` takes the built-in default.
    pub rate_limit: Option<RateLimitSpec>,
    /// Routes in input order.
    pub routes: Vec<RouteSpec>,
}

/// The validated route table. `Default` is the empty table, which denies all.
///
/// Why: fields are private so a policy exists only as the output of
/// [`ChannelPolicy::build`].
/// What: routes in input order and the default rate limit.
/// Test: `empty_policy_denies_all`, `exact_route_and_listed_kind_allow`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelPolicy {
    routes: Vec<Route>,
    default_rate_limit: RateLimit,
}

impl Default for ChannelPolicy {
    fn default() -> Self {
        Self {
            routes: Vec::new(),
            default_rate_limit: RateLimit::DEFAULT,
        }
    }
}

impl ChannelPolicy {
    /// Validate route data into a policy.
    ///
    /// Why: a bad route must fail the whole build so the operator sees it,
    /// never vanish from a table that otherwise loads (#8454 plan §4).
    /// What: validates the default rate limit, then each route in order:
    /// name `[A-Za-z0-9_-]+`, the channel's recipient shape, a non-empty kind
    /// list whose kinds the channel carries, and an optional rate limit no
    /// higher than the default in either parameter. A name shared by two
    /// routes, or a (channel, recipient) pair shared after normalization,
    /// fails with both entries named.
    /// Test: `overlapping_routes_fail_build_and_name_both`,
    /// `empty_kinds_fails_build`, `review_notice_on_slack_or_telegram_fails_build`,
    /// `invalid_recipient_or_name_fails_build`,
    /// `bucket_zero_or_nan_params_fail_build`, `route_cannot_raise_rate_limit`.
    pub fn build(spec: PolicySpec) -> Result<Self, PolicyError> {
        let default_rate_limit = match spec.rate_limit {
            None => RateLimit::DEFAULT,
            Some(s) => RateLimit::from_spec(s).map_err(|reason| PolicyError::InvalidRateLimit {
                entry: "rate_limit".into(),
                reason,
            })?,
        };
        let mut routes = Vec::with_capacity(spec.routes.len());
        let mut by_name: HashMap<String, String> = HashMap::new();
        let mut by_recipient: HashMap<(Channel, String), String> = HashMap::new();
        for (i, raw) in spec.routes.into_iter().enumerate() {
            let entry = format!("routes[{i}] {} {:?}", raw.channel, raw.name);
            let route = validate_route(&entry, raw, default_rate_limit)?;
            // #8454: an overlap fails the build and names both entries.
            if let Some(first) = by_name.insert(route.name.clone(), entry.clone()) {
                return Err(duplicate(first, entry, "name", &route.name));
            }
            let key = (route.channel, route.recipient.clone());
            if let Some(first) = by_recipient.insert(key, entry.clone()) {
                return Err(duplicate(first, entry, "recipient", &route.recipient));
            }
            routes.push(route);
        }
        Ok(Self {
            routes,
            default_rate_limit,
        })
    }

    /// Routes in input order.
    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    /// True when the policy holds no routes, so every check denies.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// The default rate limit; also the unknown-sender bucket's limit.
    pub fn default_rate_limit(&self) -> RateLimit {
        self.default_rate_limit
    }

    /// The route naming exactly `who` on `channel`, compared in the
    /// channel's normalized form. No wildcard, no prefix match.
    pub(crate) fn route_for(&self, channel: Channel, who: &str) -> Option<&Route> {
        let who = channel.normalize(who);
        self.routes
            .iter()
            .find(|r| r.channel == channel && r.recipient == who)
    }
}

fn duplicate(first: String, second: String, field: &'static str, value: &str) -> PolicyError {
    PolicyError::Duplicate {
        first,
        second,
        field,
        value: value.to_string(),
    }
}

fn validate_route(entry: &str, raw: RouteSpec, default: RateLimit) -> Result<Route, PolicyError> {
    let invalid = |reason: String| PolicyError::InvalidRoute {
        entry: entry.to_string(),
        reason,
    };
    let name_ok = !raw.name.is_empty()
        && raw
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !name_ok {
        return Err(invalid("name must match [A-Za-z0-9_-]+".into()));
    }
    raw.channel
        .check_recipient(&raw.recipient)
        .map_err(invalid)?;
    if raw.kinds.is_empty() {
        return Err(PolicyError::EmptyKinds {
            entry: entry.to_string(),
        });
    }
    let mut kinds = BTreeSet::new();
    for kind in raw.kinds {
        // #8454 Q3: review_notice is gchat-only.
        if !raw.channel.allows_kind(kind) {
            return Err(PolicyError::KindNotOnChannel {
                entry: entry.to_string(),
                kind,
                channel: raw.channel,
            });
        }
        kinds.insert(kind);
    }
    let rate_limit = match raw.rate_limit {
        None => default,
        Some(s) => {
            let limit =
                RateLimit::from_spec(s).map_err(|reason| PolicyError::InvalidRateLimit {
                    entry: entry.to_string(),
                    reason,
                })?;
            check_not_raised(entry, limit, default)?;
            limit
        }
    };
    Ok(Route {
        recipient: raw.channel.normalize(&raw.recipient),
        name: raw.name,
        channel: raw.channel,
        kinds,
        rate_limit,
    })
}

/// #8454 Q6: a route may lower the default limit, never raise it.
fn check_not_raised(entry: &str, route: RateLimit, default: RateLimit) -> Result<(), PolicyError> {
    let raised = |field, route: f64, default: f64| PolicyError::RateLimitRaised {
        entry: entry.to_string(),
        field,
        route,
        default,
    };
    if route.capacity() > default.capacity() {
        return Err(raised(
            "capacity",
            f64::from(route.capacity()),
            f64::from(default.capacity()),
        ));
    }
    if route.refill_per_sec() > default.refill_per_sec() {
        return Err(raised(
            "refill_per_sec",
            route.refill_per_sec(),
            default.refill_per_sec(),
        ));
    }
    Ok(())
}
