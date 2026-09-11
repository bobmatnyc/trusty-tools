//! Chat-thread attachments, stored under the assistant home (#7370).
//!
//! Why: a chat turn could carry only text. Dropping a CSV, a log file or a
//! screenshot into the conversation meant pasting its contents inline, which
//! loses the file's identity (name, type, size) and cannot survive a reload —
//! the durable `chat_session` projection holds `{role, content}` strings and
//! nothing else. Epic #7425 item (e) names the storage location verbatim:
//! `<assistant home>/attachments/<session>/<file>`, a sibling of
//! `<assistant home>/okg`, which [`crate::assistants::AssistantHome`] has
//! already reserved as [`crate::assistants::ATTACHMENTS_DIR`].
//!
//! What: three cooperating pieces, all synchronous and filesystem-only.
//!   - [`Attachment`] is one stored file's row: id, session, original name,
//!     media type, byte size, SHA-256, and the absolute path it landed at.
//!   - [`AttachmentStore`] owns the directory. It CONFINES every path it
//!     builds inside the attachments root and returns an error rather than
//!     substituting a name — the same contract
//!     [`crate::assistants::AssistantHome::store_root`] holds, for the same
//!     reason: a silently-renamed target writes the user's file somewhere they
//!     will never look for it, and a silently-accepted `..` writes it
//!     somewhere they never agreed to.
//!   - [`model_input`] turns stored rows into the text a turn hands the model,
//!     and reads the `[[attachment:<id>]]` markers back out of a persisted
//!     turn so a reload can rebuild the cards.
//!
//! Chat messages stay `{role, content: String}`. This module deliberately does
//! NOT widen `trusty_common::ChatMessage` or
//! `trusty_agents_common::HistoryMessage`: an attached turn embeds one
//! [`model_input::marker_for`] line per attachment inside the ordinary content
//! string, so every existing reader — the persistence path, the history route,
//! the REPL — keeps working byte-for-byte on a turn that carries no
//! attachments, and a turn that does carries its references in-band.
//!
//! Scope: storage and rendering. This module performs no HTTP, no provider
//! negotiation, and no vision/multipart content arrays — a binary attachment
//! reaches the model as a one-line reference (see [`model_input`]), because the
//! prompt assembly path builds `.content(String)` and this slice does not
//! change that.
//!
//! The manifest is JSON, not redb, because the assistant home is deliberately
//! human-browsable (see [`crate::assistants::home`]'s module doc): a user who
//! opens `attachments/<session>/` must be able to read what is there without
//! this binary. See [`manifest`] for the file's shape and its locking.
//!
//! Test: `tests` — the whole module.

mod error;
pub mod manifest;
pub mod model_input;
mod store;

#[cfg(test)]
mod tests;

pub use error::AttachmentError;
pub use manifest::Attachment;
pub use store::{AttachmentStore, FALLBACK_MEDIA_TYPE, MAX_ATTACHMENT_BYTES};
