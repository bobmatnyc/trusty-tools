//! Live-instance registry: `definition_id` → `instance_id` resolution.
//!
//! Why: DOC-60 §6b names the agent INSTANCE id as the identifier that "does
//! not exist today" and that the spec introduces — definition-addressed
//! delivery (§5.3) is meaningless without something that answers "which live
//! `izzie` am I talking to". This registry is that something, and it is what
//! makes BOTH addressing modes work: definition-addressed resolution walks it,
//! and instance-bypass validates against it.
//! What: [`InstanceRegistry`], keyed by instance id, holding one
//! [`LiveInstance`] per running assistant — its public [`InstanceMeta`] plus
//! the [`InboxSet`] holding one bounded buffer per attached subscription
//! (`super::inbox`, #6462). Registration mints the id; resolution is
//! fail-closed in both modes.
//! Test: `bus::tests` — `register_mints_prefixed_id`,
//! `resolve_definition_picks_most_recent`, `resolve_definition_missing_errors`,
//! `bypass_to_dead_instance_errors_not_falls_back`,
//! `deregister_makes_instance_gone`.
//!
//! ## Instance-bypass failure mode: EXPLICIT ERROR, never silent fallback
//!
//! A sender may hold an `instance_id` it learned from a prior reply, and that
//! instance may die before the next message is sent. This registry answers
//! that case by returning [`BusError::InstanceGone`] to the sender. It does
//! **not** fall back to definition-addressed delivery, even when another
//! instance of the same definition is live and the sender supplied a
//! `definition_id` alongside the instance id. Three reasons, in order of
//! weight:
//!
//! 1. **Silent redirect destroys the only thing bypass is for.** A sender
//!    bypasses definition resolution precisely because it wants the instance
//!    holding the conversation state — the thread it is mid-way through.
//!    Redirecting to a sibling instance delivers the message to a peer with no
//!    memory of that thread, which is worse than not delivering it: the sender
//!    believes continuity held when it did not, and the recipient answers a
//!    question it never saw the first half of.
//! 2. **DOC-60 §4 forbids it directly.** §4 requires an explicit
//!    `BusUnavailable`-shaped error "rather than silently dropping", and gives
//!    the reason: a silent outcome "recreates exactly the failure mode
//!    ADR-0019 was written to eliminate — no way to distinguish 'message never
//!    sent' from 'sent but recipient never polled'." A silent redirect is the
//!    same class of defect with an extra step; the sender still cannot tell
//!    what happened.
//! 3. **The recovery belongs to the sender, not the daemon.** Re-addressing by
//!    `definition_id` is one further call away and the sender knows whether
//!    thread continuity actually mattered for this message. The daemon does
//!    not, and guessing on the sender's behalf is a policy decision the bus
//!    has no basis to make.
//!
//! The error is distinguishable from "this definition never had an instance"
//! ([`BusError::NoLiveInstance`]) both by variant and by HTTP status (`410
//! Gone` vs `404 Not Found`), so a client can implement the re-address
//! recovery without parsing a message string.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use serde::{Deserialize, Serialize};

use super::BusError;
use super::envelope::BusEnvelope;
use super::inbox::{DeliveryOutcome, InboxSet, InboxSubscription};

/// Separator between the definition id and the random suffix in an instance id.
///
/// Why NOT `#`, which DOC-60 §11's illustrative `izzie#a1b2c3` uses: `#` is the
/// URI fragment delimiter, so a raw `#`-bearing id in a path segment
/// (`DELETE /api/v1/bus/instances/{instance_id}`) is truncated by every
/// conforming client before the request is even sent. Requiring each client to
/// percent-encode an id the daemon itself minted is precisely the kind of
/// easy-to-forget step that produces silent misrouting — the class of bug this
/// bus exists to eliminate. §11 self-declares as "not normative wire format",
/// so the readable `<definition>SEP<suffix>` shape is preserved with an
/// RFC 3986 *unreserved* separator that is safe in a path, a query, and a
/// fragment alike.
pub const INSTANCE_ID_SEPARATOR: char = '~';

/// How many suffixes [`InstanceRegistry::register`] mints before refusing.
///
/// Why (#4276): a bounded retry is what turns a collision into a re-mint
/// instead of an overwrite, and the bound is what stops a pathological suffix
/// source from spinning forever inside a registration. Eight is generous
/// against a 32-bit space — reaching it means the source is not random, which
/// is a condition to report, not to retry through.
const MAX_INSTANCE_ID_MINT_ATTEMPTS: usize = 8;

/// One random 8-hex-character instance-id suffix.
///
/// Why a named function rather than an inline expression: it is the production
/// argument [`InstanceRegistry::register_with_minter`] takes, so the seam a
/// test substitutes has exactly one real implementation.
fn mint_instance_suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// The publicly observable facts about one live instance.
///
/// Why: `GET /api/v1/bus/instances` and every resolution result need the
/// instance's identity without exposing its delivery channel; splitting the
/// serializable metadata from the channel keeps the API type honest and
/// derivable.
/// What: the minted instance id, the definition it runs, an optional project
/// scope, its RFC3339 registration time, and a monotonic registration
/// sequence.
/// Test: `register_mints_prefixed_id`, `list_returns_live_instances`,
/// `instance_id_is_url_path_safe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceMeta {
    /// §6b — the minted id, shaped `<definition_id>~<8 hex>` (e.g.
    /// `izzie~a1b2c3d4`). See [`INSTANCE_ID_SEPARATOR`] for why `~` and not
    /// §11's illustrative `#`.
    pub instance_id: String,
    /// §6a — the definition this instance runs.
    pub definition_id: String,
    /// The project this instance belongs to, when the registrant supplied one.
    ///
    /// Recorded but NOT used for routing: cross-project addressing is DOC-60
    /// §12 Q4, deferred to the owner. Carrying the value now means enabling it
    /// later does not change the registration wire shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// RFC3339 registration time.
    pub registered_at: String,
    /// Monotonic registration counter.
    ///
    /// Why not compare `registered_at`: RFC3339 strings tie at their
    /// resolution, and two instances of one definition can register in the
    /// same millisecond at daemon start. A counter makes "most recently
    /// registered" total and deterministic, which is what
    /// [`InstanceRegistry::resolve_definition`]'s tie-break needs to be
    /// testable.
    pub seq: u64,
}

/// One live instance: its metadata plus its delivery channel.
///
/// Why: delivery is addressed per instance rather than broadcast-then-filtered.
/// DOC-60 §2 identifies filter-after-broadcast as the core defect of the
/// existing `/sessions/{id}/events` path ("producers do not address a
/// recipient; consumers filter client-side after the fact"), so giving each
/// instance its own channel is what makes this bus genuinely addressed rather
/// than a sixth unaddressed broadcast.
/// What: [`InstanceMeta`] plus the [`InboxSet`] holding one bounded buffer per
/// attached subscription. Every clone of a `LiveInstance` shares that set, so
/// two publishers that each resolved the instance separately still fan out
/// through the same lock and agree on an order.
/// Test: `publish_reaches_only_target_instance`,
/// `concurrent_publishers_lose_nothing_without_a_record`.
#[derive(Debug, Clone)]
pub struct LiveInstance {
    /// The instance's public metadata.
    pub meta: InstanceMeta,
    /// One [`ClientInbox`](super::inbox::ClientInbox) per attached
    /// subscription.
    ///
    /// Private, and reachable only through [`LiveInstance::subscribe`] and
    /// [`LiveInstance::deliver`]: #4271's defect was a send that bypassed the
    /// delivery bookkeeping, and the "nothing is lost without a record"
    /// guarantee is only as good as the absence of such a send. Subscribing
    /// goes through [`PeerBus::subscribe`](super::PeerBus::subscribe);
    /// publishing goes through [`PeerBus::publish`](super::PeerBus::publish).
    inboxes: Arc<InboxSet>,
}

impl LiveInstance {
    /// Attach a new subscription to this instance.
    ///
    /// Why per subscription rather than per instance (#6462): the buffer IS the
    /// delivery boundary, so giving each client its own is what stops one
    /// stalled client from spending another's budget. See
    /// [`super::inbox`]'s module doc.
    /// What: mints a subscription id and returns its reader handle, which
    /// detaches on drop.
    /// Test: `publish_reaches_only_target_instance`,
    /// `a_detached_subscription_stops_costing_the_instance`.
    pub(super) fn subscribe(&self) -> InboxSubscription {
        self.inboxes.attach(&self.meta.instance_id)
    }

    /// Deliver one envelope into every attached client's inbox.
    ///
    /// Why this can no longer refuse (#6462): the shared 64-slot broadcast ring
    /// it replaced measured backlog across every attached receiver and could
    /// not send to a subset, so keeping the §9 log truthful meant refusing the
    /// publish — and one client that stopped reading refused every publish to
    /// the instance. Each client now has its own buffer, so a stalled client
    /// falls behind alone: its inbox displaces its own oldest unread envelope
    /// and the loss is returned here to be recorded, rather than charged to the
    /// publisher or to a healthy co-subscriber.
    ///
    /// The guarantee is scoped to this method and its logging caller: an
    /// envelope recorded `delivered` is readable by every attached subscription
    /// until that subscription reads it, UNLESS the §9 stream also carries an
    /// [`InboxEviction`](super::inbox::InboxEviction) naming that envelope and
    /// the subscription that lost it. Nothing else writes into an inbox.
    ///
    /// What: delegates to [`InboxSet::fan_out`], which holds the instance's one
    /// lock across the whole fan-out so concurrent publishers cannot hand two
    /// clients the same two envelopes in opposite orders.
    /// Test: `two_subscribers_one_lagging_account_exactly`,
    /// `eviction_is_recorded_per_client_in_the_durable_log`,
    /// `concurrent_publishers_lose_nothing_without_a_record`.
    pub fn deliver(&self, envelope: BusEnvelope) -> DeliveryOutcome {
        self.inboxes.fan_out(envelope)
    }

    /// How many subscriptions are attached.
    ///
    /// Test: `a_detached_subscription_stops_costing_the_instance`.
    pub fn subscriber_count(&self) -> usize {
        self.inboxes.len()
    }

    /// End every attached subscription once it has drained what it holds.
    ///
    /// Why: dropping the broadcast `Sender` used to do this implicitly. With
    /// per-client queues the end has to be stated, and it must not discard what
    /// the §9 log already recorded delivered — so the reader drains first.
    /// Test: `deregister_ends_a_subscription_after_it_drains`.
    fn close_subscribers(&self) {
        self.inboxes.close_all();
    }
}

/// The daemon's registry of live agent instances (DOC-60 §6b).
///
/// Why: see the module doc — this is the resolution table both addressing
/// modes depend on, and the authority on whether a bypass target is still
/// alive.
/// What: a `DashMap` keyed by instance id. Definition lookup is a linear scan
/// over live instances, deliberately: the live roster is tens of entries, and
/// a second index would have to be kept coherent with this one on every
/// register/deregister for no measurable gain at that size.
/// Test: `register_mints_prefixed_id`, `resolve_definition_picks_most_recent`,
/// `bypass_to_dead_instance_errors_not_falls_back`.
#[derive(Debug, Default)]
pub struct InstanceRegistry {
    /// Live instances, keyed by instance id.
    instances: DashMap<String, LiveInstance>,
    /// Monotonic source for [`InstanceMeta::seq`].
    next_seq: AtomicU64,
}

/// How a publisher addressed its target.
///
/// Why: the two modes are not interchangeable and must not silently degrade
/// into one another — see the module doc. Making them an enum forces every
/// call site to state which it meant, so a fallback can never happen by
/// accident.
/// What: [`PeerTarget::Definition`] is DOC-60 §5.3's normative
/// definition-addressed mode; [`PeerTarget::Instance`] is the owner-ratified
/// bypass (§12 Q8, decided in this change's favor).
/// Test: `bypass_to_dead_instance_errors_not_falls_back`,
/// `target_prefers_instance_when_both_supplied`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerTarget {
    /// Address a definition; the registry picks its live instance.
    Definition(String),
    /// Address one specific live instance directly.
    Instance(String),
}

impl InstanceRegistry {
    /// Register a newly-started instance and mint its id.
    ///
    /// Why: an assistant becomes addressable only by announcing itself; the
    /// daemon cannot discover in-process personas on its own. Minting the id
    /// here (rather than accepting a client-supplied one) keeps ids unique and
    /// unforgeable.
    /// What: validates `definition_id`, mints
    /// `<definition_id>~<8 hex>` ([`INSTANCE_ID_SEPARATOR`]), creates the
    /// instance's delivery channel, and inserts it.
    /// Test: `register_mints_prefixed_id`, `register_rejects_bad_definition`,
    /// `instance_id_is_url_path_safe`,
    /// `register_re_mints_on_a_suffix_collision`,
    /// `register_fails_loudly_when_every_mint_collides`.
    pub fn register(
        &self,
        definition_id: &str,
        project: Option<String>,
    ) -> Result<InstanceMeta, BusError> {
        self.register_with_minter(definition_id, project, mint_instance_suffix)
    }

    /// [`register`](Self::register) against an explicit suffix source.
    ///
    /// Why (#4276): registration used to `insert` unconditionally, and
    /// `DashMap::insert` REPLACES on a duplicate key. Two instances that drew
    /// the same 32-bit suffix would therefore collapse into one entry, and
    /// every later lookup of the first instance would resolve to the second —
    /// the one way instance-addressed delivery could reach an instance the
    /// sender did not name, defeating the no-fallback guarantee this module is
    /// built around by a route the module doc does not cover. The suffix source
    /// is a parameter because a collision cannot be provoked through
    /// `Uuid::new_v4`, so the guard would otherwise be untestable.
    /// What: mints, then claims the key through `entry` — a VACANT entry
    /// inserts and returns, an OCCUPIED one logs and re-mints, up to
    /// [`MAX_INSTANCE_ID_MINT_ATTEMPTS`]. Exhausting those returns
    /// [`BusError::InstanceIdCollision`] rather than overwriting a live
    /// instance: refusing to register is recoverable, silently stealing another
    /// instance's id is not. `seq` is drawn once per call, so a re-mint does
    /// not perturb the registration ordering
    /// [`resolve_definition`](Self::resolve_definition) tie-breaks on.
    /// Test: `register_re_mints_on_a_suffix_collision`,
    /// `register_fails_loudly_when_every_mint_collides`.
    pub(super) fn register_with_minter(
        &self,
        definition_id: &str,
        project: Option<String>,
        mut mint: impl FnMut() -> String,
    ) -> Result<InstanceMeta, BusError> {
        validate_definition_id(definition_id)?;
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        for attempt in 1..=MAX_INSTANCE_ID_MINT_ATTEMPTS {
            let instance_id = format!("{definition_id}{INSTANCE_ID_SEPARATOR}{}", mint());
            match self.instances.entry(instance_id.clone()) {
                Entry::Occupied(_) => {
                    tracing::warn!(
                        %instance_id,
                        attempt,
                        "bus: minted instance id already live — re-minting rather than \
                         overwriting the registered instance"
                    );
                }
                Entry::Vacant(vacant) => {
                    let meta = InstanceMeta {
                        instance_id,
                        definition_id: definition_id.to_string(),
                        project,
                        registered_at: chrono::Utc::now().to_rfc3339(),
                        seq,
                    };
                    vacant.insert(LiveInstance {
                        meta: meta.clone(),
                        inboxes: Arc::new(InboxSet::default()),
                    });
                    return Ok(meta);
                }
            }
        }
        tracing::error!(
            definition_id,
            attempts = MAX_INSTANCE_ID_MINT_ATTEMPTS,
            "bus: could not mint a free instance id — registration refused"
        );
        Err(BusError::InstanceIdCollision {
            definition_id: definition_id.to_string(),
            attempts: MAX_INSTANCE_ID_MINT_ATTEMPTS,
        })
    }

    /// Remove an instance, making it no longer addressable.
    ///
    /// Why: an instance that exited must stop resolving, or definition-
    /// addressed delivery would route to a dead channel and bypass would
    /// silently succeed against a corpse.
    /// What: removes the entry and ends its subscriptions once they have
    /// drained ([`LiveInstance::close_subscribers`]); returns whether one was
    /// present. The close is explicit since #6462 — a per-client inbox has no
    /// `Sender` whose drop would end the stream for it.
    /// Test: `deregister_makes_instance_gone`,
    /// `deregister_ends_a_subscription_after_it_drains`.
    pub fn deregister(&self, instance_id: &str) -> bool {
        match self.instances.remove(instance_id) {
            Some((_, live)) => {
                live.close_subscribers();
                true
            }
            None => false,
        }
    }

    /// Resolve a definition to its most-recently-registered live instance.
    ///
    /// Why: DOC-60 §5.3's normative addressing mode. When two instances of one
    /// definition are live, §6 acknowledges the ambiguity but does not settle
    /// which wins; most-recent is chosen because it is the one an interactive
    /// user most likely just started, and because it is deterministic and
    /// therefore testable.
    /// What: scans live instances for `definition_id`, returning the highest
    /// [`InstanceMeta::seq`]; fails closed with [`BusError::NoLiveInstance`]
    /// when none matches. #6462 built §7's inbox for a LIVE client's
    /// subscription; §7's other half — queueing to a definition with no running
    /// instance, without spawning one — is still deferred, so a definition that
    /// resolves to nothing still fails rather than queues.
    /// Test: `resolve_definition_picks_most_recent`,
    /// `resolve_definition_missing_errors`.
    pub fn resolve_definition(&self, definition_id: &str) -> Result<LiveInstance, BusError> {
        validate_definition_id(definition_id)?;
        self.instances
            .iter()
            .filter(|e| e.meta.definition_id == definition_id)
            .max_by_key(|e| e.meta.seq)
            .map(|e| e.clone())
            .ok_or_else(|| BusError::NoLiveInstance {
                definition_id: definition_id.to_string(),
            })
    }

    /// Resolve a specific instance id, or report that it is gone.
    ///
    /// Why: the instance-bypass path. See the module doc for why this returns
    /// an error rather than degrading to [`resolve_definition`](Self::resolve_definition).
    /// What: looks the instance up; returns [`BusError::InstanceGone`] when it
    /// is absent. Performs NO definition-level fallback under any
    /// circumstances.
    /// Test: `bypass_to_dead_instance_errors_not_falls_back`,
    /// `deregister_makes_instance_gone`.
    pub fn resolve_instance(&self, instance_id: &str) -> Result<LiveInstance, BusError> {
        self.instances
            .get(instance_id)
            .map(|e| e.clone())
            .ok_or_else(|| BusError::InstanceGone {
                instance_id: instance_id.to_string(),
            })
    }

    /// Resolve whichever addressing mode the publisher chose.
    ///
    /// Why: one entry point keeps the no-fallback rule in a single place. A
    /// caller cannot accidentally implement a fallback by chaining the two
    /// resolvers, because it never sees them chained.
    /// What: dispatches on [`PeerTarget`]; each arm's failure propagates
    /// unchanged.
    /// Test: `bypass_to_dead_instance_errors_not_falls_back`.
    pub fn resolve(&self, target: &PeerTarget) -> Result<LiveInstance, BusError> {
        match target {
            PeerTarget::Definition(d) => self.resolve_definition(d),
            PeerTarget::Instance(i) => self.resolve_instance(i),
        }
    }

    /// Every live instance's metadata, ordered by registration sequence.
    ///
    /// Why: an operator (and a sender choosing a peer) needs to see the live
    /// roster; stable ordering makes the listing diffable across calls.
    /// What: clones each [`InstanceMeta`], sorted by [`InstanceMeta::seq`].
    /// Test: `list_returns_live_instances`.
    pub fn live(&self) -> Vec<InstanceMeta> {
        let mut out: Vec<InstanceMeta> = self.instances.iter().map(|e| e.meta.clone()).collect();
        out.sort_by_key(|m| m.seq);
        out
    }

    /// Number of live instances.
    ///
    /// Test: `deregister_makes_instance_gone`.
    pub fn len(&self) -> usize {
        self.instances.len()
    }

    /// Whether no instance is live.
    ///
    /// Test: `deregister_makes_instance_gone`.
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }
}

/// Reject a definition id that cannot be part of a well-formed instance id.
///
/// Why: instance ids are `<definition_id>~<short>`, so a definition id
/// containing the separator would make the id ambiguous to split and could let
/// a caller forge one that resolves to something else. Instance ids also travel
/// in URL path segments, so a definition id carrying a URI-reserved character
/// would reintroduce the encoding hazard [`INSTANCE_ID_SEPARATOR`] exists to
/// avoid. Rejecting both at registration keeps every id in the registry
/// unambiguous and transport-safe by construction.
/// What: requires non-empty, separator-free, whitespace-free, and free of the
/// RFC 3986 reserved characters that break a path segment.
/// Test: `register_rejects_bad_definition`, `instance_id_is_url_path_safe`.
fn validate_definition_id(definition_id: &str) -> Result<(), BusError> {
    /// Characters that would either break a URL path segment or be silently
    /// rewritten in transit.
    const UNSAFE: &[char] = &[
        '#', '?', '/', '\\', '%', '&', '=', '+', ':', '@', '[', ']', '"', '\'', '<', '>',
    ];
    let reason = if definition_id.is_empty() {
        Some("must not be empty")
    } else if definition_id.contains(INSTANCE_ID_SEPARATOR) {
        Some("must not contain '~' (reserved as the instance-id separator)")
    } else if definition_id.chars().any(char::is_whitespace) {
        Some("must not contain whitespace")
    } else if definition_id.contains(UNSAFE) {
        Some("must not contain URL-reserved characters (#?/\\%&=+:@[]\"'<>)")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(BusError::InvalidDefinitionId {
            definition_id: definition_id.to_string(),
            reason: reason.to_string(),
        }),
        None => Ok(()),
    }
}

/// Shared handle type used by [`super::PeerBus`] and the HTTP routes.
pub type SharedRegistry = Arc<InstanceRegistry>;
