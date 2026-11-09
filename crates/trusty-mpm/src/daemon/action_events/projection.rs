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
//! path or a message never leaves this module. #9167: a relay's text leaves
//! only as `super::message` digests it, never as text unless bodies are on.
//! #10906: each session's registry `project_key` is resolved once, from its
//! first `cwd`, and stamped on every event the session projects after the
//! lookup lands. The lookup reads the disk, so this module only queues it
//! ([`Lookup`]); the publisher runs it off the hook path and off the lock.
//! Test: `projection_tests.rs`.
//!
//! # Spec References
//! - [`SPEC-UNIDASH-03~draft`](docs/specs/DOC-73-unified-mpm-code-agents-dashboard.md#SPEC-UNIDASH-03~draft)
//! - [`SPEC-UNIDASH-05~draft`](docs/specs/DOC-73-unified-mpm-code-agents-dashboard.md#SPEC-UNIDASH-05~draft)

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use trusty_common::control_bus::{
    ActionEvent, ActionMeta, Actor, CallPhase, EventId, FilePhase, HarnessEvent, HarnessPayload,
    HarnessSource, ObjectRef, ObjectType, PathRef, SessionPhase, WorkflowPhase,
};

use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::SessionId;
use crate::daemon::services::hook_service::keep_file_changed;

/// Sessions tracked at once; the least recently touched is evicted.
pub const MAX_SESSIONS: usize = 256;

/// Open tool calls (and open worktrees) tracked per session.
pub const MAX_OPEN_PER_SESSION: usize = 256;

/// Longest string copied from a payload, in characters.
const MAX_FIELD_CHARS: usize = 1024;

/// Names the registry `project_key` for a session's `cwd`; `None` when the
/// cwd belongs to no registered project or the lookup fails (#10906).
pub type ProjectResolver = Arc<dyn Fn(&Path) -> Option<String> + Send + Sync>;

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
    // #10906: the session's registry `project_key`, once its lookup lands.
    project: Option<String>,
    /// Whether the lookup was queued; it is, from the first known `cwd`.
    resolved: bool,
}

/// One session's queued project lookup (#10906).
///
/// Why: the resolver walks the filesystem and reads the registry, and a
/// stale network mount can block that for minutes. The threader runs under
/// the publisher's one lock on the synchronous hook path, so it must never
/// call the resolver itself.
/// What: the session, the root that identifies its thread, the first `cwd`
/// and the resolver. [`Self::resolve`] does the I/O; [`Threader::settle`]
/// stores the answer only if the same thread is still tracked.
/// Test: `a_session_is_resolved_once_from_its_first_cwd_10906`,
/// `a_slow_project_lookup_never_holds_the_hook_path_10906`.
pub struct Lookup {
    session: SessionId,
    root: EventId,
    cwd: PathBuf,
    resolver: ProjectResolver,
}

impl Lookup {
    /// The registry `project_key` for this session's first `cwd`. Reads the
    /// disk: call it on a blocking thread, never under the threader lock.
    pub fn resolve(&self) -> Option<String> {
        (self.resolver)(&self.cwd)
    }
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
    /// Whether `message` events carry their body (#9167); off by default.
    bodies: bool,
    /// #10906: names a session's project; none in a pure threader.
    resolver: Option<ProjectResolver>,
    /// #10906: lookups queued for the publisher, at most [`MAX_SESSIONS`].
    lookups: VecDeque<Lookup>,
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
            bodies: false,
            resolver: None,
            lookups: VecDeque::new(),
        }
    }

    /// Hand over the project lookups queued since the last call (#10906).
    pub fn take_lookups(&mut self) -> Vec<Lookup> {
        self.lookups.drain(..).collect()
    }

    /// Store `lookup`'s answer, unless its session ended or was evicted
    /// meanwhile (#10906: a stale answer never lands on a newer thread).
    pub fn settle(&mut self, lookup: &Lookup, project: Option<String>) {
        if let Some(thread) = self.sessions.get_mut(&lookup.session)
            && thread.root == lookup.root
        {
            thread.project = project;
        }
    }

    /// Queue `lookup` again on its session's next record with a cwd: the
    /// publisher had no room to run it (#10906).
    pub fn requeue(&mut self, lookup: &Lookup) {
        if let Some(thread) = self.sessions.get_mut(&lookup.session)
            && thread.root == lookup.root
        {
            thread.resolved = false;
        }
    }

    /// #10906: queue the one lookup for `session`'s first cwd. No I/O.
    fn request(&mut self, session: SessionId, root: EventId, cwd: &Path) {
        let Some(resolver) = self.resolver.clone() else {
            return;
        };
        if self.lookups.len() >= MAX_SESSIONS {
            self.lookups.pop_front();
        }
        self.lookups.push_back(Lookup {
            session,
            root,
            cwd: cwd.to_path_buf(),
            resolver,
        });
    }

    /// Name each session's project with `resolver` (#10906).
    pub fn set_project_resolver(&mut self, resolver: Option<ProjectResolver>) {
        self.resolver = resolver;
    }

    /// The resolver [`Self::set_project_resolver`] installed, if any.
    pub fn project_resolver(&self) -> Option<ProjectResolver> {
        self.resolver.clone()
    }

    /// `session`'s registry `project_key`, while it is tracked (#10906: the
    /// project of an inject or a received message). Reads memory only.
    pub fn project_of(&self, session: &SessionId) -> Option<String> {
        self.sessions.get(session).and_then(|t| t.project.clone())
    }

    /// Carry the credential-scanned body on `message` events (#9167).
    pub fn set_message_bodies(&mut self, on: bool) {
        self.bodies = on;
    }

    /// Whether `message` events carry their body.
    pub fn message_bodies(&self) -> bool {
        self.bodies
    }

    /// `session`'s root event, while it is tracked (#9167: the parent of a
    /// received message).
    pub fn root_of(&self, session: &SessionId) -> Option<EventId> {
        self.sessions.get(session).map(|t| t.root)
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
    /// | a relay (`super::message`) | `Message`, after the above | the above |
    ///
    /// A `FileChanged` whose path fails `is_coding_file` projects to nothing.
    /// A record for a session with no root first mints a `Session::Started`
    /// with `attrs.inferred = true`, so a daemon that started mid-session
    /// still emits a forest. `seq` is `0`; the publisher stamps it.
    /// #10906: the first record with a `cwd` queues the session's one
    /// [`Lookup`]; this call never runs it. Every event carries the
    /// session's `project_key` once [`Self::settle`] has stored it, and
    /// `None` before that — the record that queued the lookup included — and
    /// when the cwd is unregistered. Events are never held for the lookup:
    /// hook ingest neither waits nor drops, at the cost of the session's
    /// first events going out unattributed.
    /// Test: `every_hook_event_variant_projects_to_its_kind_6849`,
    /// `terminals_parent_their_opening_event_6849`,
    /// `a_session_is_resolved_once_from_its_first_cwd_10906`.
    pub fn project(&mut self, record: &HookEventRecord) -> Vec<HarnessEvent> {
        // #6849: the watcher skips `hook_service`'s noise filter; build,
        // dependency and VCS writes would flood the bounded buffer.
        if record.event == HookEvent::FileChanged
            && !keep_file_changed(&str_field(&record.payload, "path").unwrap_or_default())
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
                let project = self.project_of(&record.session);
                return stamped(out, project);
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
        // #10906: a session whose first records named no cwd queues now.
        let lookup = match thread.cwd.clone() {
            Some(cwd) if !thread.resolved => {
                thread.resolved = true;
                Some((thread.root, cwd))
            }
            _ => None,
        };
        let project = thread.project.clone();
        let id = EventId::new();
        let (body, parent) = step(thread, record, id, &self.home);
        out.push(envelope(record, id, Some(parent), body));
        // #9167: a relay on this record is a `message` event, child of `id`.
        out.extend(
            super::message::messages(record, self.bodies)
                .into_iter()
                .map(|m| super::message::envelope(record, m, id)),
        );
        if record.event == HookEvent::SessionEnd {
            self.sessions.remove(&record.session);
        } else if let Some((root, cwd)) = lookup {
            self.request(record.session, root, &cwd);
        }
        stamped(out, project)
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
        let cwd = str_field(&record.payload, "cwd").map(PathBuf::from);
        // #10906: queue the one lookup when this first record names a cwd.
        if let Some(c) = cwd.as_deref() {
            self.request(record.session, root, c);
        }
        self.sessions.insert(
            record.session,
            Thread {
                root,
                resolved: cwd.is_some(),
                cwd,
                calls: Open::default(),
                worktrees: Open::default(),
                touched: self.tick,
                project: None,
            },
        );
        root
    }
}

/// `events`, each carrying `project` (#10906).
fn stamped(mut events: Vec<HarnessEvent>, project: Option<String>) -> Vec<HarnessEvent> {
    for event in &mut events {
        event.project.clone_from(&project);
    }
    events
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
        project: None,
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
