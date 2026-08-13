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
//! | [`tools`] | which pinned tools are needed, and the call that installs them |
//! | [`discover`] | which repositories the recipient's `gh` credential can reach (#5487) |
//! | [`clone`] | getting the selected repositories onto the recipient's disk (#5215) |
//! | [`run`] | driving the pinned `tga audit` over the selected repositories |
//!
//! ## What this milestone is not
//!
//! Repository selection and cloning (#5487, #5215), package assembly, signing
//! and the GUI are later milestones on #5473/#5477's tree. [`run::sweep`] reads
//! the selection those will write — see [`run::SELECTION_FILE`] for the exact
//! file and shape. [`tools::install`] fetches the pinned triple, but it
//! contains no download implementation — it calls `trusty-installer`'s pinned,
//! fail-closed entry point (#5491, #5495), because a second implementation here
//! would be a defect under CLAUDE.md's common-entry-point rule.
//!
//! ## Three properties worth checking before trusting this crate with a codebase
//!
//! **Everything it writes is under one root.** `rm -rf <work-dir>` is a complete
//! uninstall — see [`workdir`], where the containment property is a test.
//!
//! **A credential never reaches an output artifact.** The engagement config
//! carries a spend-capped OpenRouter key; the deliverable that goes back carries
//! none. [`config::SecretKey`] implements `Deserialize` and not `Serialize`, so
//! that is a compile error rather than a review catch.
//!
//! **Nothing runs at a version the engagement did not pin.** The tool triple is
//! required config with no default, all three install or none do, and the
//! versions that were verified are recorded — see [`tools`].
//!
//! Test: each module carries its own `#[cfg(test)]` tests.

pub mod cli;
pub mod clone;
pub mod config;
pub mod discover;
pub mod error;
pub mod manifest;
pub mod run;
pub mod session;
pub mod tools;
pub mod workdir;

pub use error::AuditError;
pub use session::{Command, Outcome, Session};
pub use workdir::WorkDir;
