//! Palace recall/remember/note over the daemon socket, with no MCP (#8352).
//!
//! Why: `tm memory` exposed only `import` and `import-auto-memory`, so a PM
//! whose `mcp__trusty-memory__*` connection is dead had no way to reach the
//! palace at all. The parity target is trusty-search, which is reachable from
//! its own CLI without an MCP client in the middle. Owner requirement, verbatim:
//! "memory should \[be\] an in process mpm command as well as mcp".
//!
//! What: [`run_verb`] resolves the palace and the socket, then issues ONE
//! direct-method JSON-RPC call — `memory_recall`, `memory_remember` or
//! `memory_note`, the three names trusty-memory's `TOOL_METHODS` allowlist
//! already forwards to the same handlers the MCP tools reach — through
//! `trusty_common::memory_rpc::call_memory_tool_at`. The arguments this module
//! builds are the MCP tools' own schema keys; nothing here invents a parameter.
//!
//! **Every write goes over the socket.** This module opens no palace store of
//! its own, so it cannot become the second in-process writer of a palace the
//! daemon already serves — the redb exclusive-lock hazard #1078 fixed and
//! `trusty_common::memory_core::registry` documents. It therefore adds no
//! `memory-core` edge: the default `trusty-mpm` build pays no HNSW/ONNX cost
//! for these verbs, and there is deliberately NO no-daemon read path (a
//! read-only snapshot fallback would need exactly that edge, behind a
//! non-default feature, to answer a question a running daemon already answers).
//!
//! #9340: `memory_forget` rides the same path; its own checks live in
//! `core::memory_forget`.
//!
//! Test: `memory_verbs_tests.rs`, plus `tests/memory_verbs_socket.rs` for the
//! end-to-end binary path.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Map, Value, json};

use trusty_common::memory_rpc::{call_memory_tool_at, resolve_memory_socket};
use trusty_common::palace_resolve::resolve_palace;

/// The trusty-memory method `tm memory recall` calls.
pub const RECALL_METHOD: &str = "memory_recall";
/// The trusty-memory method `tm memory remember` calls.
pub const REMEMBER_METHOD: &str = "memory_remember";
/// The trusty-memory method `tm memory note` calls.
pub const NOTE_METHOD: &str = "memory_note";

/// Why a verb could not run, or could not be answered.
///
/// Why a typed error (#8352): the binary layer turns each of these into a
/// distinct non-zero exit message, and the socket-down arm must NAME the socket
/// rather than report a generic transport failure — that path is the whole
/// point of the verbs, so an operator has to be able to see which socket was
/// dialled. No arm degrades to an empty success.
/// What: [`Self::Palace`] when no palace could be resolved for a write,
/// [`Self::Socket`] when the socket path itself could not be derived, and
/// [`Self::Call`] when the daemon did not answer — or answered with an error.
/// Test: `a_dead_socket_is_an_error_naming_it`,
/// `a_write_without_a_resolvable_palace_is_refused`.
// #8372: non_exhaustive from its first release, so a new failure is not an API break.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryVerbError {
    /// A write verb, and no palace could be resolved for the working directory.
    #[error("no palace resolved for {cwd}: {detail} — pass --palace <slug>")]
    Palace {
        /// The directory resolution started from.
        cwd: String,
        /// What resolution reported.
        detail: String,
    },
    /// The trusty-memory socket path could not be derived.
    #[error("could not resolve the trusty-memory socket: {detail}")]
    Socket {
        /// What resolution reported.
        detail: String,
    },
    /// Nothing answered the socket, or the daemon answered with an error.
    #[error("trusty-memory did not answer `{method}` at {socket}: {detail}")]
    Call {
        /// The JSON-RPC method that was called.
        method: &'static str,
        /// The socket that was dialled — the operator-actionable fact.
        socket: String,
        /// What the transport or the daemon reported.
        detail: String,
    },
    /// `--expires-at` is not an RFC 3339 timestamp; nothing was sent (#9142).
    #[error(
        "--expires-at must be an RFC 3339 timestamp (e.g. 2026-10-06T12:00:00Z), got {value:?}: {detail}"
    )]
    ExpiresAt {
        /// The value the caller passed.
        value: String,
        /// What the parser reported.
        detail: String,
    },
    /// `--expires-at` is not in the future; nothing was sent (#9142).
    #[error("--expires-at {value} is already in the past; a slot needs a future expiry")]
    ExpiresAtPast {
        /// The value the caller passed.
        value: String,
    },
    /// #9340: the drawer id to forget is not a UUID; nothing was sent.
    #[error("drawer id must be a UUID, got {value:?}: {detail}")]
    DrawerId {
        /// The value the caller passed.
        value: String,
        /// What the parser reported.
        detail: String,
    },
}

/// Process exit code when a write was stored but its requested slot was refused.
///
/// Why (#9142): the daemon stores a refused slot's drawer as an ordinary
/// (Tier E) drawer and answers `status: "stored"`; exiting 0 would hide that the
/// fact will not supersede anything. `2` is clap's usage-error code and `1` a
/// failed call, so a refusal gets its own value.
/// Test: `a_refused_slot_warns_and_exits_with_the_refusal_code`.
pub const EXIT_SLOT_REFUSED: i32 = 3;

/// Why the daemon refused the slot a stored write asked for, if it did.
///
/// Why (#9142): a refused slot still stores the drawer, so the only signal is
/// the envelope's `tier` / `tier_c_refused`.
/// What: `Some(reason)` when the verb sent a `fact_key`, the daemon reports the
/// drawer stored, and `tier` is not `"C"`. The reason is `tier_c_refused`, or a
/// note naming the tier the daemon reported. `None` otherwise.
/// Test: `a_refused_slot_warns_and_exits_with_the_refusal_code`.
pub fn slot_refusal(verb: &MemoryVerb, result: &Value) -> Option<String> {
    verb.slot()?.fact_key.as_ref()?;
    let status = result.get("status").and_then(Value::as_str);
    if !matches!(status, None | Some("stored")) {
        return None;
    }
    match result.get("tier").and_then(Value::as_str) {
        Some("C") => None,
        tier => Some(
            result
                .get("tier_c_refused")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("daemon reported tier {}", tier.unwrap_or("(none)"))),
        ),
    }
}

/// The ADR-0028 Tier C slot a write occupies (#9142).
///
/// Why: a status or resume fact that a later write cannot retire outranks the
/// current one in recall for weeks. Writing it under a stable `fact_key`
/// (`ws:<session>/resume`, `pr:<n>/state`) makes the next write to that key
/// supersede it.
/// What: `fact_key` and `expires_at`, sent verbatim under trusty-memory's own
/// `memory_remember`/`memory_note` schema keys of the same names
/// (`crates/trusty-memory/src/tools/definitions.rs`). Both `None` sends neither
/// key, so the request is the pre-#9142 one. The daemon owns the slot grammar
/// and the 24-hour default expiry; this side only rejects an unparseable or
/// past timestamp before anything is sent.
/// Test: `a_fact_key_and_expiry_reach_the_request`,
/// `a_write_without_slot_flags_sends_the_old_request`,
/// `a_bad_expires_at_is_refused_before_any_rpc`,
/// `a_past_expires_at_is_refused_before_any_rpc`.
#[derive(Debug, Clone, Default)]
pub struct FactSlot {
    /// `--fact-key`: the slot, `<domain>:<id>/<aspect>`.
    pub fact_key: Option<String>,
    /// `--expires-at`: RFC 3339 timestamp after which the fact stops being current.
    pub expires_at: Option<String>,
}

impl FactSlot {
    /// Reject an `expires_at` that is not RFC 3339, or is not in the future.
    ///
    /// Why: trusty-memory rejects an unparseable timestamp, and stores a past one
    /// unslotted, only after the call. A caller mistake must fail here instead.
    /// Test: `a_bad_expires_at_is_refused_before_any_rpc`,
    /// `a_past_expires_at_is_refused_before_any_rpc`.
    fn validate(&self) -> Result<(), MemoryVerbError> {
        if let Some(raw) = self.expires_at.as_deref() {
            let at = chrono::DateTime::parse_from_rfc3339(raw).map_err(|e| {
                MemoryVerbError::ExpiresAt {
                    value: raw.to_string(),
                    detail: e.to_string(),
                }
            })?;
            // #9142: the daemon refuses `expires_at <= now` the same way.
            if at <= chrono::Utc::now() {
                return Err(MemoryVerbError::ExpiresAtPast {
                    value: raw.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Insert the slot keys the caller supplied.
    fn insert_into(&self, args: &mut Map<String, Value>) {
        insert_opt(args, "fact_key", self.fact_key.clone().map(Value::from));
        insert_opt(args, "expires_at", self.expires_at.clone().map(Value::from));
    }
}

/// One `tm memory` verb and the arguments it carries.
///
/// Why: the three verbs differ only in method name and argument keys, so
/// modelling them as data keeps [`run_verb`] a single path and keeps the
/// argument keys — which are trusty-memory's own MCP schema keys — in one
/// place to audit against `crates/trusty-memory/src/tools/definitions.rs`.
/// What: [`Self::Recall`] mirrors `memory_recall`, [`Self::Remember`]
/// `memory_remember`, [`Self::Note`] `memory_note`, [`Self::Forget`]
/// `memory_forget` (#9340).
/// Test: `arguments_carry_only_the_schema_keys_supplied`.
#[derive(Debug, Clone)]
pub enum MemoryVerb {
    /// Progressive L0+L1+L2 recall.
    Recall {
        /// The search text.
        query: String,
        /// `top_k`; omitted lets the daemon's own default (10) stand.
        top_k: Option<u64>,
        /// Restrict the semantic layer to one room.
        room: Option<String>,
        /// Restrict the L2 search to one wing's rooms.
        wing: Option<String>,
        /// Relevance floor applied to query-scored hits.
        min_score: Option<f64>,
    },
    /// Store a memory, subject to the daemon's content gates.
    Remember {
        /// The memory text.
        text: String,
        /// Room to file it in.
        room: Option<String>,
        /// Tags to store alongside it.
        tags: Vec<String>,
        /// #9142: the Tier C slot, if any.
        slot: FactSlot,
    },
    /// Store a short curated fact (`DrawerType::UserFact`, importance 1.0).
    Note {
        /// The fact.
        content: String,
        /// Room to file it in.
        room: Option<String>,
        /// Tags to store alongside it.
        tags: Vec<String>,
        /// #9142: the Tier C slot, if any.
        slot: FactSlot,
    },
    /// #9340: delete one drawer by id; see `core::memory_forget`.
    Forget {
        /// UUID of the drawer to delete.
        drawer_id: String,
    },
}

impl MemoryVerb {
    /// The JSON-RPC method this verb calls.
    pub fn method(&self) -> &'static str {
        match self {
            Self::Recall { .. } => RECALL_METHOD,
            Self::Remember { .. } => REMEMBER_METHOD,
            Self::Note { .. } => NOTE_METHOD,
            Self::Forget { .. } => super::memory_forget::FORGET_METHOD,
        }
    }

    /// Does this verb write to the palace?
    ///
    /// Why it matters here: a write needs a palace NAMED, while a read with no
    /// palace is answered by the daemon with a palace index (#6318) that tells
    /// the caller which palace to name next.
    pub fn is_write(&self) -> bool {
        !matches!(self, Self::Recall { .. })
    }

    /// The Tier C slot a write carries; `None` for a recall.
    fn slot(&self) -> Option<&FactSlot> {
        match self {
            Self::Recall { .. } | Self::Forget { .. } => None,
            Self::Remember { slot, .. } | Self::Note { slot, .. } => Some(slot),
        }
    }

    /// The tool arguments, without `palace`.
    ///
    /// What: only the keys the caller actually supplied are present, so an
    /// omitted flag leaves the daemon's own default in force rather than
    /// pinning it from here.
    /// Test: `arguments_carry_only_the_schema_keys_supplied`.
    fn arguments(&self) -> Map<String, Value> {
        let mut args = Map::new();
        match self {
            Self::Recall {
                query,
                top_k,
                room,
                wing,
                min_score,
            } => {
                args.insert("query".into(), json!(query));
                insert_opt(&mut args, "top_k", top_k.map(Value::from));
                insert_opt(&mut args, "room", room.clone().map(Value::from));
                insert_opt(&mut args, "wing", wing.clone().map(Value::from));
                insert_opt(&mut args, "min_score", min_score.map(Value::from));
            }
            Self::Remember {
                text,
                room,
                tags,
                slot,
            } => {
                args.insert("text".into(), json!(text));
                insert_opt(&mut args, "room", room.clone().map(Value::from));
                insert_tags(&mut args, tags);
                slot.insert_into(&mut args); // #9142
            }
            Self::Note {
                content,
                room,
                tags,
                slot,
            } => {
                args.insert("content".into(), json!(content));
                insert_opt(&mut args, "room", room.clone().map(Value::from));
                insert_tags(&mut args, tags);
                slot.insert_into(&mut args); // #9142
            }
            Self::Forget { drawer_id } => {
                args.insert("drawer_id".into(), json!(drawer_id.trim())); // #9340
            }
        }
        args
    }
}

/// Insert `value` under `key` only when it is present.
fn insert_opt(args: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        args.insert(key.to_string(), value);
    }
}

/// Insert a non-empty `tags` array.
fn insert_tags(args: &mut Map<String, Value>, tags: &[String]) {
    if !tags.is_empty() {
        args.insert("tags".to_string(), json!(tags));
    }
}

/// Where a verb runs: which palace, which socket, from which directory.
#[derive(Debug, Clone, Default)]
pub struct MemoryVerbOptions {
    /// `--palace`; outranks every derived level, including the env override.
    pub palace: Option<String>,
    /// `--memory-socket`; outranks `TRUSTY_MEMORY_SOCKET` and the derived path.
    pub socket: Option<PathBuf>,
    /// Directory palace resolution starts from. `None` means the process cwd.
    pub cwd: Option<PathBuf>,
}

/// What one verb produced, in the shape `--json` prints.
///
/// Why a fixed envelope: an agent parsing this must not have to tell three
/// response shapes apart, and must be able to see which palace and which socket
/// answered — the two facts that decide whether an empty recall is a wrong
/// palace or an empty one. Every key is always present, `null` where it does
/// not apply, so a parser never branches on key existence.
/// What: `verb`, `palace`, `socket`, `count` (recall hits, else `null`) and
/// `result` — the daemon's own tool body, verbatim.
/// Test: `json_envelope_keys_are_always_present`.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryVerbOutcome {
    /// `recall` / `remember` / `note` / `forget` (#9340).
    pub verb: String,
    /// The palace the call named, when one was resolved.
    pub palace: Option<String>,
    /// The socket that answered.
    pub socket: String,
    /// Number of recall hits; `null` for a write.
    pub count: Option<usize>,
    /// The daemon's own tool result, unmodified.
    pub result: Value,
}

/// Resolve the palace this verb addresses.
///
/// Why: the verbs have to land in the SAME palace the MCP tools would, so this
/// goes through `trusty_common::palace_resolve::resolve_palace` — the single
/// entry point whose level 1 is `TRUSTY_MEMORY_PALACE`, the variable a managed
/// session's environment injects (`core::mcp_session_env`). `--palace` sits
/// above all four levels because it is the caller's explicit instruction.
/// What: `Ok(Some(id))` when a palace was named or derived; `Ok(None)` when
/// nothing could be derived and the verb is a READ, which the daemon answers
/// with a palace index rather than an error (#6318); `Err` when nothing could be
/// derived and the verb WRITES, because a write has nowhere to land.
/// Test: `explicit_palace_outranks_the_derived_levels`,
/// `a_write_without_a_resolvable_palace_is_refused`.
pub fn resolve_verb_palace(
    verb: &MemoryVerb,
    opts: &MemoryVerbOptions,
) -> Result<Option<String>, MemoryVerbError> {
    if let Some(explicit) = opts.palace.as_ref().map(|p| p.trim())
        && !explicit.is_empty()
    {
        return Ok(Some(explicit.to_string()));
    }
    let cwd = match opts.cwd.clone() {
        Some(cwd) => cwd,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    match resolve_palace(&cwd) {
        Ok(resolution) => Ok(Some(resolution.id)),
        Err(e) if verb.is_write() => Err(MemoryVerbError::Palace {
            cwd: cwd.display().to_string(),
            detail: e.to_string(),
        }),
        Err(e) => {
            tracing::debug!("no palace resolved for {}: {e}", cwd.display());
            Ok(None)
        }
    }
}

/// Run one verb against the running trusty-memory daemon.
///
/// Why: this is the whole no-MCP path — resolve, call, hand back the daemon's
/// own body. It performs no local storage access of any kind, so a palace the
/// daemon holds open is never opened a second time (#1078).
///
/// # Errors
///
/// [`MemoryVerbError::ExpiresAt`] / [`MemoryVerbError::ExpiresAtPast`] when
/// `--expires-at` is not RFC 3339 or not in the future — raised
/// before the palace or socket is resolved, so nothing is sent (#9142);
/// [`MemoryVerbError::DrawerId`] when a forget's id is not a UUID, likewise
/// before anything is sent (#9340);
/// [`MemoryVerbError::Palace`] when a write has no palace,
/// [`MemoryVerbError::Socket`] when the socket path cannot be derived, and
/// [`MemoryVerbError::Call`] — naming the socket — when nothing answers it or
/// the daemon refuses. The wait is bounded by
/// `trusty_common::memory_rpc::DEFAULT_TIMEOUT`, so a dead daemon fails fast
/// rather than hanging.
///
/// Test: `recall_sends_the_resolved_palace`, `a_write_sends_a_write_method_over_the_socket`,
/// `a_dead_socket_is_an_error_naming_it`, `a_bad_expires_at_is_refused_before_any_rpc`.
pub async fn run_verb(
    verb: &MemoryVerb,
    opts: &MemoryVerbOptions,
) -> Result<MemoryVerbOutcome, MemoryVerbError> {
    // #9142: an unparseable expiry fails here, before any RPC.
    if let Some(slot) = verb.slot() {
        slot.validate()?;
    }
    // #9340: a malformed drawer id fails here too, before any RPC.
    if let MemoryVerb::Forget { drawer_id } = verb {
        super::memory_forget::validate_drawer_id(drawer_id)?;
    }
    let palace = resolve_verb_palace(verb, opts)?;
    let socket = match opts.socket.clone() {
        Some(socket) => socket,
        None => resolve_memory_socket().map_err(|e| MemoryVerbError::Socket {
            detail: format!("{e:#}"),
        })?,
    };

    let mut args = verb.arguments();
    if let Some(palace) = palace.clone() {
        args.insert("palace".to_string(), Value::String(palace));
    }

    let method = verb.method();
    let result = call_memory_tool_at(&socket, method, Value::Object(args))
        .await
        .map_err(|e| MemoryVerbError::Call {
            method,
            socket: socket.display().to_string(),
            detail: format!("{e:#}"),
        })?;

    let count = if verb.is_write() {
        None
    } else {
        Some(
            result
                .get("results")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0),
        )
    };

    Ok(MemoryVerbOutcome {
        verb: verb_label(verb).to_string(),
        palace,
        socket: socket.display().to_string(),
        count,
        result,
    })
}

/// The CLI name of a verb, as `--json` reports it.
fn verb_label(verb: &MemoryVerb) -> &'static str {
    match verb {
        MemoryVerb::Recall { .. } => "recall",
        MemoryVerb::Remember { .. } => "remember",
        MemoryVerb::Note { .. } => "note",
        MemoryVerb::Forget { .. } => "forget",
    }
}

#[cfg(test)]
#[path = "memory_verbs_tests.rs"]
mod tests;
