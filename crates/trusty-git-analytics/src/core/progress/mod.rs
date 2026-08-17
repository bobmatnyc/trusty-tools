//! Optional, non-blocking progress reporting for long-running `tga` stages.
//!
//! Why: `collect` and `classify` can run for minutes; their only progress
//! surface was `indicatif`, which is one-shot per stage and hidden off-TTY, so
//! nothing could drive a live multi-stage display (#5197). This module adds a
//! structured event stream a consumer subscribes to — today the `tga tui`
//! subcommand, tomorrow anything else.
//!
//! What: three pieces.
//!
//! - [`ProgressEvent`] / [`Stage`] / [`Outcome`] — the wire format.
//! - [`ProgressBus`] — a bounded, drop-oldest, never-blocking channel. It is
//!   OPTIONAL at every emit site: [`ProgressBus::disabled`] is what all
//!   existing CLI paths pass, and on it every emit is a no-op, so CLI behavior
//!   including the current `indicatif` bars is unchanged.
//! - [`ProgressAggregate`] — the pure fold from events to renderable rows, so
//!   the display logic is testable without a terminal.
//!
//! Scope: this lives in `tga`, not `trusty-common`, because it has exactly one
//! consumer today. Promote it only when a second crate genuinely needs it.
//!
//! Test: `tests` in this module.
//!
//! # Example
//!
//! ```
//! use tga::core::progress::{ProgressBus, ProgressEvent, Stage};
//!
//! // No subscriber: emits cost nothing and nothing is observable.
//! let off = ProgressBus::disabled();
//! off.emit(ProgressEvent::started(Stage::Collect, "api", Some(3)));
//! assert!(off.drain().is_empty());
//!
//! // With a subscriber: events queue until drained.
//! let bus = ProgressBus::new();
//! bus.emit(ProgressEvent::completed(Stage::Collect, "api", 3));
//! assert_eq!(bus.drain().len(), 1);
//! ```

mod aggregate;
mod bus;
mod event;
// #5823: the same events, written out of the process for a parent that spawned
// it — the fourth piece, and the only one whose consumer is not in-process.
pub mod relay;

pub use aggregate::{ProgressAggregate, StageSummary, TargetRow, LOG_CAPACITY};
pub use bus::{ProgressBus, DEFAULT_CAPACITY};
pub use event::{Outcome, ProgressEvent, Stage};
pub use relay::StageRelay;

#[cfg(test)]
mod tests;
