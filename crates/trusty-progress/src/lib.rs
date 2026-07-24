//! # trusty-progress
//!
//! Shared rustup/cargo-style progress + status display for the trusty-* tooling
//! ecosystem. Today `trusty-search` and `tga` each pin `indicatif` locally and
//! hand-roll their own bars; this crate (issue #1315) consolidates the *style*
//! behind one small, dependency-light API so every tool renders progress the
//! same way.
//!
//! ## Why this crate exists
//!
//! - **One look.** A single right-aligned component table and `info:` narrator
//!   format, matching rustup, used everywhere.
//! - **Centralised terminal handling.** TTY detection, the quiet/non-TTY/`--json`
//!   fallback, and indicatif draw-target selection live in [`Output`] alone, so
//!   call sites never re-derive them.
//! - **Library-grade errors.** Fallible writes surface as [`ProgressError`]
//!   (via `thiserror`); there is no `unwrap()` in the library path.
//!
//! ## Building blocks
//!
//! - [`Output`] / [`Mode`] — pick where text goes and how animated it should be.
//! - [`Narrator`] — `info:` / `warning:` / `error:` status lines.
//! - [`ComponentTracker`] / [`Component`] — the right-aligned "name installed
//!   size" table.
//! - [`ProgressHandle`] — a spinner (indeterminate) or bar (determinate) over
//!   `indicatif`.
//! - [`format_mib`] / [`format_bytes`] — human-readable size columns.
//!
//! ## Rustup-style component table
//!
//! Render the exact layout from the issue — `info:` lines bracketing a
//! right-aligned component table. Here we capture the output to a buffer (as the
//! tests do) so the example is deterministic; in a real CLI you would use
//! [`Output::to_stderr`] and let terminal detection choose the mode.
//!
//! ```rust
//! use trusty_progress::{Component, ComponentTracker, Mode, Narrator, Output};
//!
//! // In production: `Output::to_stderr()` autodetects the terminal.
//! let (output, capture) = Output::for_capture(Mode::Plain);
//! let narrator = Narrator::new(output.clone());
//!
//! narrator.info("downloading 6 components")?;
//!
//! let mut table = ComponentTracker::new(output.clone());
//! table
//!     .add(Component::new("cargo", 8_870_953))
//!     .add(Component::new("clippy", 3_030_384))
//!     .add(Component::new("rust-std", 27_472_691));
//! table.print()?;
//!
//! narrator.info("default toolchain set to stable-aarch64-apple-darwin")?;
//!
//! let rendered = capture.contents();
//! // The narrator lines bracket the right-aligned table:
//! assert!(rendered.starts_with("info: downloading 6 components\n"));
//! assert!(rendered.contains("cargo installed"));
//! assert!(rendered.contains("8.46 MiB"));
//! assert!(rendered.trim_end().ends_with(
//!     "info: default toolchain set to stable-aarch64-apple-darwin"
//! ));
//! // Plain/non-TTY output never contains ANSI escape sequences.
//! assert!(!rendered.contains('\u{1b}'));
//! # Ok::<(), trusty_progress::ProgressError>(())
//! ```

mod components;
mod error;
mod live;
mod narrator;
mod output;
mod progress;
mod size;

pub use components::{Component, ComponentTracker};
pub use error::{ProgressError, Result};
pub use live::{ComponentState, LiveChecklist};
pub use narrator::Narrator;
pub use output::{Capture, Mode, Output};
pub use progress::ProgressHandle;
pub use size::{format_bytes, format_mib};
