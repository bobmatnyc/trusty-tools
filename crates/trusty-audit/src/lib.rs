//! The auditor client: a library, with a thin CLI over it.
//!
//! Why: a client company receives a package, runs it against their own
//! codebases, and returns a report. The owner's framing is that this is really
//! an installer runner — it fetches pinned `tga` / `trusty-analyze` /
//! `trusty-review` binaries and drives them (#5477). Two constraints shape the
//! crate, and both are permanent (#5502):
//!
//! 1. **Headless first.** A library plus a CLI. The Tauri shell is a later
//!    milestone and is a view over this API, never a second implementation.
//! 2. **Every feature is CLI-testable, forever.** For any capability there must
//!    be a CLI invocation that exercises it end to end with no window. That is
//!    not a review convention here: [`session::Command`] is the capability set,
//!    [`session::Session::execute`] is the only way to run one, and
//!    [`cli`]'s exhaustive match over `Command` fails to compile if a capability
//!    has no CLI path.
//!
//! What: five modules, each with one job.
//!
//! | Module | Owns |
//! |---|---|
//! | [`session`] | the capability enum, the dispatcher, the structured results |
//! | [`cli`] | argv → capability, and result → text |
//! | [`workdir`] | the directory tree this crate owns on the recipient's machine |
//! | [`config`] | the engagement config that ships TO the recipient — and the key it carries |
//! | [`manifest`] | reading tga's `manifest.toml` rather than duplicating it |
//! | [`tools`] | which pinned tools are needed, and the seam where #5491 installs them |
//!
//! ## What this milestone is not
//!
//! Scaffold only. The audit sweep, package assembly, signing and the GUI are
//! later milestones on #5473/#5477's tree. [`tools::install`] is a seam that
//! fails closed rather than a download implementation — downloads belong to
//! `trusty-installer` (#5491), and a second implementation here would be a
//! defect under CLAUDE.md's common-entry-point rule.
//!
//! ## Two properties worth checking before trusting this crate with a codebase
//!
//! **Everything it writes is under one root.** `rm -rf <work-dir>` is a complete
//! uninstall — see [`workdir`], where the containment property is a test.
//!
//! **A credential never reaches an output artifact.** The engagement config
//! carries a spend-capped OpenRouter key; the deliverable that goes back carries
//! none. [`config::SecretKey`] implements `Deserialize` and not `Serialize`, so
//! that is a compile error rather than a review catch.
//!
//! Test: each module carries its own `#[cfg(test)]` tests.

pub mod cli;
pub mod config;
pub mod error;
pub mod manifest;
pub mod session;
pub mod tools;
pub mod workdir;

pub use error::AuditError;
pub use session::{Command, Outcome, Session};
pub use workdir::WorkDir;
