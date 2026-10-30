//! Debug-only timing trace for the `tm tui` project view (#10476).
//!
//! Why: the refresh tick's cost and the key-to-frame latency must be compared
//! before and after a change on a live daemon, and a profiler cannot see where
//! the UI loop waits.
//! What: [`TuiTrace`] appends one line per event to the file `TM_TUI_TRACE`
//! names: `start` (the wall-clock origin), `tick` (per-call and total ms of one
//! daemon poll), `draw` (one frame) and `key` (key read to the next drawn
//! frame). Unset, it holds no file and
//! every method returns at once.
//! Test: `trace_writes_tick_draw_and_key_lines`, `trace_without_a_file_writes_nothing`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

/// The environment variable naming the trace file.
pub(crate) const TRACE_ENV: &str = "TM_TUI_TRACE";

/// One poll's per-call timings, in call order.
pub(crate) type CallTimings = Vec<(&'static str, Duration)>;

/// Appends timing lines to an optional file; a no-op when it has none.
#[derive(Debug)]
pub(crate) struct TuiTrace {
    file: Option<File>,
    origin: Instant,
}

impl TuiTrace {
    /// Open the file `TM_TUI_TRACE` names, or a disabled trace when unset.
    ///
    /// An unopenable path disables the trace with a stderr note; the TUI
    /// still runs.
    pub(crate) fn from_env() -> Self {
        match std::env::var_os(TRACE_ENV) {
            Some(path) if !path.is_empty() => Self::to_file(Path::new(&path)),
            _ => Self::disabled(),
        }
    }

    /// A trace that writes nothing.
    pub(crate) fn disabled() -> Self {
        Self {
            file: None,
            origin: Instant::now(),
        }
    }

    /// A trace appending to `path`; disabled when the file cannot be opened.
    pub(crate) fn to_file(path: &Path) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .inspect_err(|e| eprintln!("{TRACE_ENV}: cannot open {}: {e}", path.display()))
            .ok();
        let mut trace = Self {
            file,
            origin: Instant::now(),
        };
        // The wall-clock origin lets a run line `at_ms` up with key-send times.
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        trace.write(&format!("start unix_ms={unix_ms}"));
        trace
    }

    /// Whether lines are written; callers skip building them otherwise.
    pub(crate) fn enabled(&self) -> bool {
        self.file.is_some()
    }

    /// Record one daemon poll: when it started, each call, and the total.
    pub(crate) fn tick(&mut self, started: Instant, calls: &CallTimings, total: Duration) {
        if !self.enabled() {
            return;
        }
        let mut line = format!("tick start_ms={}", self.since_origin(started));
        for (name, took) in calls {
            line.push_str(&format!(" {name}_ms={}", ms(*took)));
        }
        line.push_str(&format!(" total_ms={}", ms(total)));
        self.write(&line);
    }

    /// Record one drawn frame.
    pub(crate) fn draw(&mut self, took: Duration) {
        if self.enabled() {
            self.write(&format!("draw ms={}", ms(took)));
        }
    }

    /// Record the time from a key read to the end of the next drawn frame,
    /// and when that frame ended (`at_ms` from the `start` line).
    pub(crate) fn key_to_frame(&mut self, took: Duration) {
        if self.enabled() {
            let at = self.since_origin(Instant::now());
            self.write(&format!("key ms={} at_ms={at}", ms(took)));
        }
    }

    fn since_origin(&self, at: Instant) -> u128 {
        at.saturating_duration_since(self.origin).as_millis()
    }

    fn write(&mut self, line: &str) {
        if let Some(file) = &mut self.file
            && writeln!(file, "{line}").is_err()
        {
            // A full disk or a removed file ends the trace, never the TUI.
            self.file = None;
        }
    }
}

/// Milliseconds with one decimal place.
fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_writes_tick_draw_and_key_lines() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("trace.log");
        let mut trace = TuiTrace::to_file(&path);
        let calls = vec![
            ("health", Duration::from_millis(2)),
            ("fleet", Duration::from_millis(900)),
        ];
        trace.tick(Instant::now(), &calls, Duration::from_millis(902));
        trace.draw(Duration::from_micros(1500));
        trace.key_to_frame(Duration::from_millis(12));
        drop(trace);

        let text = std::fs::read_to_string(&path).expect("read trace");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[0].starts_with("start unix_ms="), "{text}");
        assert!(lines[1].starts_with("tick start_ms="), "{text}");
        assert!(
            lines[1].ends_with("health_ms=2.0 fleet_ms=900.0 total_ms=902.0"),
            "{text}"
        );
        assert_eq!(lines[2], "draw ms=1.5");
        assert!(lines[3].starts_with("key ms=12.0 at_ms="), "{text}");
    }

    #[test]
    fn trace_without_a_file_writes_nothing() {
        let mut trace = TuiTrace::disabled();
        assert!(!trace.enabled());
        trace.tick(Instant::now(), &vec![], Duration::ZERO);
        trace.draw(Duration::ZERO);
        trace.key_to_frame(Duration::ZERO);
        assert!(!trace.enabled());
    }
}
