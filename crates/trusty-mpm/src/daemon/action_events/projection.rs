//! Hook record to bus event projection, with `parent_id` threading (#6849).
//!
//! Why: DOC-73 §3.4. tm's hooks arrive as Claude Code records — PascalCase
//! names and an untyped payload — and the dashboard tree (§5.2) needs typed
//! actions joined by a causal edge. This module is the adapter between the
//! two: it is the only place that reads Claude Code's payload field names on
//! the way to the bus, so the `ActionEvent` contract in `trusty-common` stays
//! harness-neutral.
//! What: [`Threader::project`] turns one [`HookEventRecord`] into the events
//! it implies and keeps the per-session context that threads `parent_id`:
//! each session's root, its open tool calls and its open worktrees, all
//! bounded. It reads identifiers only (tool name, `tool_use_id`, `agent_id`,
//! `agent_type`, a path); a prompt, a tool input or response, a transcript
//! path or a message never leaves this module.
//! Test: `projection_tests.rs`.
//!
//! # Spec References
//! - [`SPEC-UNIDASH-03~draft`](docs/specs/DOC-73-unified-mpm-code-agents-dashboard.md#SPEC-UNIDASH-03~draft)
//! - [`SPEC-UNIDASH-05~draft`](docs/specs/DOC-73-unified-mpm-code-agents-dashboard.md#SPEC-UNIDASH-05~draft)

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use trusty_common::control_bus::{
    ActionEvent, ActionMeta, Actor, CallPhase, EventId, FilePhase, HarnessEvent, HarnessPayload,
    HarnessSource, ObjectRef, ObjectType, PathRef, SessionPhase, WorkflowPhase,
};

use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::SessionId;
use crate::daemon::services::hook_service::is_coding_file;

/// Sessions tracked at once; the least recently touched is evicted.
pub const MAX_SESSIONS: usize = 256;

/// Open tool calls (and open worktrees) tracked per session.
pub const MAX_OPEN_PER_SESSION: usize = 256;

/// Longest string copied from a payload, in characters.
const MAX_FIELD_CHARS: usize = 1024;

/// Payload fields a `Hook`-domain event keeps: identifiers, never content.
pub const HOOK_DATA_FIELDS: &[&str] = &["agent_id", "agent_type"];

/// Open calls or worktrees by key, oldest evicted first.
#[derive(Default)]
struct Open(VecDeque<(String, EventId)>);

impl Open {
    fn insert(&mut self, key: String, id: EventId) {
        if self.0.len() >= MAX_OPEN_PER_SESSION {
            self.0.pop_front();
        }
        self.0.push_back((key, id));
    }

    fn take(&mut self, key: &str) -> Option<EventId> {
        let at = self.0.iter().position(|(k, _)| k == key)?;
        self.0.remove(at).map(|(_, id)| id)
    }
}

/// One session's causal context.
struct Thread {
    root: EventId,
    cwd: Option<PathBuf>,
    calls: Open,
    worktrees: Open,
    touched: u64,
}

/// What one record becomes, before the envelope is stamped.
enum Body {
    Session(SessionPhase, BTreeMap<String, Value>),
    Tool(CallPhase, String, String),
    File(PathRef),
    Workflow(WorkflowPhase, ObjectRef),
    Hook(String, Value),
}

/// Per-session causal context for the projection.
///
/// Why: DOC-73 §3.1 — `parent_id` is the event that caused this one, and
/// only the producer knows that. A hook record names no parent, so the
/// projection remembers what is open in each session.
/// What: maps a session to its root and its open calls and worktrees. Both
/// maps are bounded ([`MAX_SESSIONS`], [`MAX_OPEN_PER_SESSION`]); an evicted
/// entry costs a less precise parent (the session root, or a fresh inferred
/// root), never a cycle or a dangling edge. Not thread-safe; the publisher
/// holds it behind a lock.
/// Test: `a_recorded_session_projects_to_a_forest_6849`,
/// `threading_state_stays_bounded_6849`.
pub struct Threader {
    sessions: HashMap<SessionId, Thread>,
    tick: u64,
    home: Vec<PathBuf>,
}

impl Threader {
    /// A threader that shows paths outside a session's `cwd` relative to
    /// `home` (raw and canonical spellings), as `~/...`.
    pub fn new(home: Option<PathBuf>) -> Self {
        let mut prefixes = Vec::new();
        if let Some(home) = home {
            if let Ok(canonical) = std::fs::canonicalize(&home)
                && canonical != home
            {
                prefixes.push(canonical);
            }
            prefixes.push(home);
        }
        Self {
            sessions: HashMap::new(),
            tick: 0,
            home: prefixes,
        }
    }

    /// Carry the credential-scanned body on `message` events.
    pub fn with_message_bodies(self, _on: bool) -> Self {
        self
    }

    /// Project one hook record onto the bus events it implies.
    ///
    /// Why: DOC-73 §3.4 maps the hooks with an obvious kind; everything else
    /// stays on ADR-0005's untyped `Hook` arm, so nothing is lost.
    /// What:
    ///
    /// | Hook | Event | Parent |
    /// |---|---|---|
    /// | `SessionStart`, new session | `Session::Started` | none: the root |
    /// | `SessionStart`, known session | `Session::StatusChanged` | root |
    /// | `SessionEnd` | `Session::Done` | root; the thread closes |
    /// | `PreToolUse` | `Tool::Started` | root |
    /// | `PostToolUse`, `PostToolUseFailure` | `Tool::Finished`, `Tool::Errored` | its `Started`, else root |
    /// | `FileChanged` | `File::Written` | root |
    /// | `WorktreeCreate`, `WorktreeRemove` | `Workflow::Start`, `Workflow::Stop` | root, its `Start` |
    /// | any other | `Hook { kind, data }` | root |
    ///
    /// A `FileChanged` whose path fails `is_coding_file` projects to nothing.
    /// A record for a session with no root first mints a `Session::Started`
    /// with `attrs.inferred = true`, so a daemon that started mid-session
    /// still emits a forest. `seq` is `0`; the publisher stamps it.
    /// Test: `every_hook_event_variant_projects_to_its_kind_6849`,
    /// `terminals_parent_their_opening_event_6849`.
    pub fn project(&mut self, record: &HookEventRecord) -> Vec<HarnessEvent> {
        // #6849: the watcher skips `hook_service`'s noise filter; build,
        // dependency and VCS writes would flood the bounded buffer.
        if record.event == HookEvent::FileChanged
            && !is_coding_file(&str_field(&record.payload, "path").unwrap_or_default())
        {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(2);
        if !self.sessions.contains_key(&record.session) {
            let root = self.open_thread(record);
            let fresh = record.event == HookEvent::SessionStart;
            let mut attrs = BTreeMap::new();
            if !fresh {
                attrs.insert("inferred".to_string(), json!(true));
            }
            let started = Body::Session(SessionPhase::Started, attrs);
            out.push(envelope(record, root, None, started));
            if fresh {
                return out;
            }
        }
        self.tick += 1;
        let Some(thread) = self.sessions.get_mut(&record.session) else {
            return out;
        };
        thread.touched = self.tick;
        if let Some(cwd) = str_field(&record.payload, "cwd") {
            thread.cwd = Some(PathBuf::from(cwd));
        }
        let id = EventId::new();
        let (body, parent) = step(thread, record, id, &self.home);
        out.push(envelope(record, id, Some(parent), body));
        if record.event == HookEvent::SessionEnd {
            self.sessions.remove(&record.session);
        }
        out
    }

    /// Start a thread for `record`'s session, evicting the least recently
    /// touched one at capacity, and return the new root's id.
    fn open_thread(&mut self, record: &HookEventRecord) -> EventId {
        if self.sessions.len() >= MAX_SESSIONS
            && let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, t)| t.touched)
                .map(|(s, _)| *s)
        {
            self.sessions.remove(&oldest);
        }
        self.tick += 1;
        let root = EventId::new();
        self.sessions.insert(
            record.session,
            Thread {
                root,
                cwd: str_field(&record.payload, "cwd").map(PathBuf::from),
                calls: Open::default(),
                worktrees: Open::default(),
                touched: self.tick,
            },
        );
        root
    }
}

/// The body and parent for a record on a session that has a root.
fn step(
    thread: &mut Thread,
    record: &HookEventRecord,
    id: EventId,
    home: &[PathBuf],
) -> (Body, EventId) {
    let payload = &record.payload;
    let root = thread.root;
    match record.event {
        HookEvent::SessionStart => (
            Body::Session(SessionPhase::StatusChanged, BTreeMap::new()),
            root,
        ),
        HookEvent::SessionEnd => (Body::Session(SessionPhase::Done, BTreeMap::new()), root),
        HookEvent::PreToolUse => {
            let call = call_id(payload);
            thread.calls.insert(call.clone(), id);
            (Body::Tool(CallPhase::Started, tool(payload), call), root)
        }
        HookEvent::PostToolUse | HookEvent::PostToolUseFailure => {
            let phase = if record.event == HookEvent::PostToolUse {
                CallPhase::Finished
            } else {
                CallPhase::Errored
            };
            let call = call_id(payload);
            let parent = thread.calls.take(&call).unwrap_or(root);
            (Body::Tool(phase, tool(payload), call), parent)
        }
        HookEvent::FileChanged => {
            let raw = str_field(payload, "path").unwrap_or_default();
            let path = shown(Path::new(&raw), thread.cwd.as_deref(), home);
            (
                Body::File(PathRef {
                    path,
                    diff_ref: None,
                }),
                root,
            )
        }
        HookEvent::WorktreeCreate => {
            let key = worktree_key(payload);
            let object = worktree_ref(&key, thread.cwd.as_deref(), home);
            thread.worktrees.insert(key, id);
            (Body::Workflow(WorkflowPhase::Start, object), root)
        }
        HookEvent::WorktreeRemove => {
            let key = worktree_key(payload);
            let object = worktree_ref(&key, thread.cwd.as_deref(), home);
            let parent = thread.worktrees.take(&key).unwrap_or(root);
            (Body::Workflow(WorkflowPhase::Stop, object), parent)
        }
        other => (
            Body::Hook(other.wire_name().to_string(), hook_data(payload)),
            root,
        ),
    }
}

/// Stamp the envelope and, for an action, its [`ActionMeta`].
fn envelope(
    record: &HookEventRecord,
    id: EventId,
    parent_id: Option<EventId>,
    body: Body,
) -> HarnessEvent {
    let session = record.session.0.to_string();
    let meta = |actor: Actor, objects: Vec<ObjectRef>, attrs: BTreeMap<String, Value>| ActionMeta {
        id,
        at: record.at,
        source: HarnessSource::Mpm,
        session: Some(session.clone()),
        parent_id,
        actor,
        objects,
        attrs,
        schema_version: 1,
    };
    let payload = match body {
        Body::Session(phase, attrs) => {
            let object = ObjectRef {
                object_type: ObjectType::Session,
                id: session.clone(),
                label: session.chars().take(8).collect(),
            };
            HarnessPayload::Action(ActionEvent::Session {
                meta: meta(Actor::System, vec![object], attrs),
                phase,
            })
        }
        Body::Tool(phase, tool, call_id) => {
            let object = ObjectRef {
                object_type: ObjectType::ToolCall,
                id: call_id.clone(),
                label: tool.clone(),
            };
            HarnessPayload::Action(ActionEvent::Tool {
                meta: meta(
                    actor(&record.payload, &session),
                    vec![object],
                    BTreeMap::new(),
                ),
                phase,
                tool,
                call_id,
            })
        }
        Body::File(path) => HarnessPayload::Action(ActionEvent::File {
            meta: meta(Actor::System, vec![path_ref(&path.path)], BTreeMap::new()),
            phase: FilePhase::Written,
            path,
        }),
        Body::Workflow(phase, object) => HarnessPayload::Action(ActionEvent::Workflow {
            meta: meta(
                actor(&record.payload, &session),
                vec![object.clone()],
                BTreeMap::new(),
            ),
            phase,
            object,
        }),
        Body::Hook(kind, data) => HarnessPayload::Hook { kind, data },
    };
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: Some(session),
        seq: 0,
        at: record.at,
        payload,
        id,
        parent_id,
    }
}

/// A non-empty string field, cut to [`MAX_FIELD_CHARS`].
fn str_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(MAX_FIELD_CHARS).collect())
}

/// The call's correlation key; a fresh id when the hook carries none.
fn call_id(payload: &Value) -> String {
    str_field(payload, "tool_use_id").unwrap_or_else(|| EventId::new().to_string())
}

fn tool(payload: &Value) -> String {
    str_field(payload, "tool").unwrap_or_else(|| "unknown".to_string())
}

/// The subagent named by `agent_id`, else the session's main loop.
///
/// Why: Claude Code sets `agent_id` only on a hook fired inside a subagent
/// (see `payload_is_subagent_dispatch`); without it the call is the main
/// loop's, which has no id of its own, so the session id stands in.
fn actor(payload: &Value, session: &str) -> Actor {
    let name = str_field(payload, "agent_type");
    match str_field(payload, "agent_id") {
        Some(agent_id) => Actor::Agent {
            name: name.unwrap_or_else(|| "subagent".to_string()),
            agent_id,
        },
        None => Actor::Agent {
            name: name.unwrap_or_else(|| "main".to_string()),
            agent_id: session.to_string(),
        },
    }
}

/// `path` relative to `cwd` when under it, `~/...` under home, else as given.
fn shown(path: &Path, cwd: Option<&Path>, home: &[PathBuf]) -> String {
    if let Some(rest) = cwd.and_then(|c| path.strip_prefix(c).ok()) {
        return if rest.as_os_str().is_empty() {
            ".".to_string()
        } else {
            rest.display().to_string()
        };
    }
    for prefix in home {
        if let Ok(rest) = path.strip_prefix(prefix) {
            return if rest.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", rest.display())
            };
        }
    }
    path.display().to_string()
}

/// A file object: the shown path, labelled by its last component.
fn path_ref(shown: &str) -> ObjectRef {
    let label = Path::new(shown)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| shown.to_string());
    ObjectRef {
        object_type: ObjectType::File,
        id: shown.to_string(),
        label,
    }
}

/// The worktree's path, else its name, else empty.
fn worktree_key(payload: &Value) -> String {
    str_field(payload, "worktree_path")
        .or_else(|| str_field(payload, "name"))
        .unwrap_or_default()
}

/// A worktree is shown as the directory it is (`ObjectType` has no
/// worktree; DOC-73 §6's `file` route shows the path).
fn worktree_ref(key: &str, cwd: Option<&Path>, home: &[PathBuf]) -> ObjectRef {
    if key.is_empty() {
        return ObjectRef {
            object_type: ObjectType::File,
            id: String::new(),
            label: "worktree".to_string(),
        };
    }
    path_ref(&shown(Path::new(key), cwd, home))
}

/// The [`HOOK_DATA_FIELDS`] present on `payload`, as an object.
fn hook_data(payload: &Value) -> Value {
    let mut out = Map::new();
    for key in HOOK_DATA_FIELDS {
        if let Some(v) = str_field(payload, key) {
            out.insert((*key).to_string(), Value::String(v));
        }
    }
    Value::Object(out)
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
