//! Daemon-owned session model + attach/detach protocol (#2054, vision spec
//! Axiom 4 / §4.3-4.4 / §12).
//!
//! Why: "The Daemon OWNS Sessions; CLI Attaches Over the API" — a running
//! session is a first-class object living inside `tcode serve`, not a tmux
//! pane the CLI scrapes. This module is the whole vertical slice: the
//! domain type, its in-memory registry + ring buffer + attach bookkeeping,
//! and the `session.*` JSON-RPC methods built on top of the existing
//! `crate::jsonrpc::Router` — reachable identically over STDIO and HTTP
//! (`crate::serve`), reusing the SAME method surface both transports
//! dispatch through.
//! What: [`model::Session`]/[`model::SessionStatus`] (the domain type),
//! [`registry::SessionRegistry`] (storage + ring buffer + attach/detach +,
//! since #2056, execution tracking, #2350 operator-facing goal slots),
//! [`transcript::TranscriptRecord`] (#2058's `session.get_transcript`
//! response DTO, #2350 additive `goals` field) plus its
//! [`transcript::GoalSlotRecord`] snapshot type, [`memory_sink::TurnMemorySink`]
//! (#2345's durable per-turn dual-write to trusty-memory, owned per-session
//! by `registry::SessionRegistry`), and [`protocol::register`] (wires
//! `session.create`/`list`/`status`/`send`/`attach`/`detach`/`cancel`/
//! `get_transcript`/(#2350)`set_goal`/`clear_goal`/`get_goals` onto a
//! `Router`).
//! Test: `model::tests::*`, `registry::registry_tests::*`,
//! `protocol::tests::*`; end-to-end over the real daemon in
//! `tests/session_e2e.rs` and (#2058) `tests/task_e2e.rs`.

pub mod memory_sink;
pub mod model;
pub mod protocol;
pub mod registry;
pub mod transcript;

pub use memory_sink::TurnMemorySink;
pub use model::{Session, SessionStatus};
pub use registry::SessionRegistry;
pub use transcript::{GoalSlotRecord, TranscriptRecord};
