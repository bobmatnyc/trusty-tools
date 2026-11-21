//! A stderr log sink for the scheduler binary.
//!
//! Why: the contract says a rejected config is logged to stderr, and the
//! engine reports through `tracing`, which prints nothing without a
//! subscriber. The leaf crate takes no `tracing-subscriber`.
//! What: [`init`] installs a global subscriber that writes each INFO-or-above
//! event as `<UTC time> <LEVEL> <message> key=value ...` on stderr. Spans are
//! ignored. Calling it twice is harmless (the second install is refused and
//! reported to the caller).
//! Test: `logger_installs_once` (tests/sched_cli.rs).

use std::fmt::{Debug, Write};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

use crate::classify::alerts::iso;

struct Stderr;

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        // Writing to a String cannot fail.
        let _ = if field.name() == "message" {
            write!(self.0, "{value:?}")
        } else {
            write!(self.0, " {}={value:?}", field.name())
        };
    }
}

impl Subscriber for Stderr {
    fn enabled(&self, m: &Metadata<'_>) -> bool {
        *m.level() <= Level::INFO
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, e: &Event<'_>) {
        let mut f = Fields(String::new());
        e.record(&mut f);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        eprintln!("{} {} {}", iso(now), e.metadata().level(), f.0);
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// Installs the stderr subscriber.
///
/// # Errors
/// A subscriber is already installed.
pub fn init() -> Result<(), String> {
    tracing::subscriber::set_global_default(Stderr).map_err(|e| e.to_string())
}
