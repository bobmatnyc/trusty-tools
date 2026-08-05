//! The two extractors under comparison, behind one interface, plus the
//! process-level measurement helpers.
//!
//! `Native` deliberately calls `core::extract::extract_text` — the production
//! seam, size caps and all — rather than the per-format submodules, because
//! the spike's question is whether anydoc could replace what actually ships,
//! not whether it beats a stripped-down parser.

use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Native,
    Anydoc,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Engine::Native => "native",
            Engine::Anydoc => "anydoc",
        }
    }

    pub fn parse(s: &str) -> Option<Engine> {
        match s {
            "native" => Some(Engine::Native),
            "anydoc" => Some(Engine::Anydoc),
            _ => None,
        }
    }

    /// Extract `path`, normalising both extractors onto `Result<String, String>`.
    ///
    /// The native side folds `Extracted::warning` into the Ok value's
    /// companion flag rather than discarding it — a near-empty PDF that our
    /// extractor flags as probably-scanned is a materially different outcome
    /// from anydoc's hard `Unsupported` error on the same file, and averaging
    /// those together would hide the difference.
    pub fn extract(self, path: &Path) -> Outcome {
        match self {
            Engine::Native => match trusty_search::core::extract::extract_text(path) {
                Ok(e) => Outcome::Ok {
                    text: e.text,
                    warning: e.warning,
                },
                Err(e) => Outcome::Err(e.to_string()),
            },
            Engine::Anydoc => match anydoc::to_markdown(path) {
                Ok(text) => Outcome::Ok {
                    text,
                    warning: None,
                },
                Err(e) => Outcome::Err(e.to_string()),
            },
        }
    }
}

pub enum Outcome {
    Ok {
        text: String,
        warning: Option<String>,
    },
    Err(String),
}

impl Outcome {
    pub fn text(&self) -> &str {
        match self {
            Outcome::Ok { text, .. } => text,
            Outcome::Err(_) => "",
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Outcome::Ok {
                text,
                warning: Some(w),
            } => format!("ok+warning({} bytes): {w}", text.len()),
            Outcome::Ok {
                text,
                warning: None,
            } => format!("ok ({} bytes)", text.len()),
            Outcome::Err(e) => format!("err: {}", first_line(e)),
        }
    }
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.len() > 120 {
        format!("{}…", &line[..120])
    } else {
        line.to_string()
    }
}

/// Peak resident set size of THIS process, in bytes.
///
/// `ru_maxrss` is a process high-water mark, which is precisely why the
/// memory leg re-execs one child per (engine, file) measurement: read
/// in-process across several extractions it would report the maximum over all
/// of them and attribute it to whichever ran last.
pub fn peak_rss_bytes() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid, fully-initialised (zeroed) rusage.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if rc != 0 {
        return 0;
    }
    let raw = usage.ru_maxrss as u64;
    // macOS reports bytes; Linux reports kilobytes.
    if cfg!(target_os = "macos") {
        raw
    } else {
        raw * 1024
    }
}

/// Run `f` `reps` times, returning (min, median, max) wall-clock.
///
/// Min is reported alongside the median because it is the least noisy
/// estimator of the parse cost itself on a shared laptop; the spread between
/// them is the signal for how much to trust either.
pub fn time_reps<F: FnMut()>(reps: usize, mut f: F) -> (Duration, Duration, Duration) {
    let mut samples = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        f();
        samples.push(t.elapsed());
    }
    samples.sort();
    (
        samples[0],
        samples[samples.len() / 2],
        samples[samples.len() - 1],
    )
}

pub fn fmt_ms(d: Duration) -> String {
    format!("{:.2}", d.as_secs_f64() * 1000.0)
}

pub fn fmt_mib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}
