//! The host's 1-minute load average (#8261).
//!
//! Why: builder admission needs a SUSTAINED measure of how busy the machine is,
//! and [`host_metrics`](crate::host_metrics)' `CpuMetrics::usage_pct` is not
//! one — it is `sysinfo`'s instantaneous global utilisation at the moment of
//! the call, so a machine between two `rustc` bursts reads idle. The kernel's
//! own 1-minute load average is the exponentially-decayed run-queue length,
//! which is exactly the "is this machine already saturated" question admission
//! asks. It is a different primitive from anything `host_metrics` samples, so
//! it lives beside that module rather than inside it.
//!
//! What: [`read_load_average`] returns [`LoadAverage`] — the 1/5/15-minute
//! triple — from `getloadavg(3)` on macOS and the other BSD-shaped unixes, and
//! from `/proc/loadavg` on Linux, which is the authoritative source there.
//! Every failure is a [`LoadAverageError`] carrying the OS errno where one
//! exists; nothing here panics, and nothing here substitutes a guessed number
//! for a reading it could not take. Admission's fail-closed rule needs to tell
//! "unreadable" apart from "high", which a sentinel value would destroy.
//!
//! Test: the `#[cfg(test)]` suite below.

use std::fmt;

/// A host load-average reading, as the kernel reports it.
///
/// Why: the three windows are read together by both platform paths, and a
/// caller that wants the 5- or 15-minute figure later should not need a second
/// syscall or a second module. Admission reads [`Self::one_minute`] only.
/// What: run-queue length averaged over 1, 5 and 15 minutes. These are NOT
/// percentages and are not normalised by core count — a value of `16.0` on a
/// 16-core host means one runnable thread per core. The caller divides by its
/// own core count.
/// Test: `a_reading_from_this_host_is_finite_and_non_negative`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadAverage {
    /// Run-queue length averaged over the last minute.
    pub one_minute: f64,
    /// Run-queue length averaged over the last five minutes.
    pub five_minute: f64,
    /// Run-queue length averaged over the last fifteen minutes.
    pub fifteen_minute: f64,
}

impl fmt::Display for LoadAverage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:.2} {:.2} {:.2}",
            self.one_minute, self.five_minute, self.fifteen_minute
        )
    }
}

/// Why the load average could not be read.
///
/// Why: admission fails CLOSED to the fixed ceiling on an unreadable metric and
/// must say WHICH reading failed and why (#8261, `builder-cap-load-read-failure`).
/// A `None` return would collapse "this platform has no load average" and "the
/// syscall failed with EPERM" into one indistinguishable case, and the operator
/// surface has to tell them apart.
/// What: one variant per way the two platform paths fail. [`Self::Errno`]
/// carries the raw OS error code, because `getloadavg` reports failure as `-1`
/// and leaves the reason in `errno`.
/// Test: `an_unparsable_proc_line_is_an_error`, `a_short_proc_line_is_an_error`.
#[derive(Debug, thiserror::Error)]
pub enum LoadAverageError {
    /// The platform exposes no load average this module knows how to read.
    #[error("no load-average source on this platform ({0})")]
    Unsupported(&'static str),
    /// A syscall or file read failed; the OS error is attached.
    #[error("load-average read failed: {source} (errno {errno:?})")]
    Errno {
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
        /// The raw errno, when the OS provided one.
        errno: Option<i32>,
    },
    /// The source was readable but did not contain three parsable numbers.
    ///
    // #8261: the field is `origin`, not `source` — `thiserror` reads a field
    // literally named `source` as the error's `std::error::Error::source()`,
    // and a `&'static str` does not implement `StdError`.
    #[error("load-average source {origin} is malformed: {detail}")]
    Malformed {
        /// Where the unparsable text came from.
        origin: &'static str,
        /// What was wrong with it.
        detail: String,
    },
}

impl LoadAverageError {
    /// The OS errno behind this failure, when there is one.
    ///
    /// Why: the refusal message and the `tm doctor` row both log the errno
    /// (#8261), and reaching into the variant at each site would duplicate the
    /// match.
    /// Test: `an_errno_is_reported_for_an_io_failure`.
    #[must_use]
    pub fn errno(&self) -> Option<i32> {
        match self {
            Self::Errno { errno, .. } => *errno,
            _ => None,
        }
    }
}

/// This host's load average right now.
///
/// Why: the one entry point, so the platform split is decided in a single
/// reviewable place rather than at each caller.
/// What: `/proc/loadavg` on Linux, `getloadavg(3)` on every other unix, and
/// [`LoadAverageError::Unsupported`] elsewhere. Sub-millisecond; the caller may
/// call it per admission decision.
///
/// # Errors
///
/// [`LoadAverageError`] when the platform has no source, the read fails, or the
/// source does not parse. Never a substituted value — see the module doc.
///
/// Test: `a_reading_from_this_host_is_finite_and_non_negative`.
pub fn read_load_average() -> Result<LoadAverage, LoadAverageError> {
    #[cfg(target_os = "linux")]
    {
        read_proc_loadavg()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        read_getloadavg()
    }
    #[cfg(not(unix))]
    {
        Err(LoadAverageError::Unsupported(std::env::consts::OS))
    }
}

/// `getloadavg(3)` — the BSD/macOS path.
///
/// Why: macOS has no `/proc`, and `sysctl vm.loadavg` returns a fixed-point
/// struct that `getloadavg` already converts. This is the libc call the
/// `uptime(1)` on this host makes.
/// What: asks for three samples; a return under 3 means the platform filled
/// fewer than asked and the reading is incomplete rather than wrong.
/// Test: `a_reading_from_this_host_is_finite_and_non_negative` (this arm runs
/// on macOS CI and dev hosts).
#[cfg(all(unix, not(target_os = "linux")))]
fn read_getloadavg() -> Result<LoadAverage, LoadAverageError> {
    let mut samples = [0f64; 3];
    // SAFETY: `getloadavg` writes at most `samples.len()` doubles into the
    // pointer it is given, and the length passed is exactly that. The array is
    // fully initialised before the call, so a short write leaves valid f64s.
    let filled = unsafe { libc::getloadavg(samples.as_mut_ptr(), 3) };
    if filled < 3 {
        let source = std::io::Error::last_os_error();
        let errno = source.raw_os_error();
        return Err(LoadAverageError::Errno { source, errno });
    }
    Ok(LoadAverage {
        one_minute: samples[0],
        five_minute: samples[1],
        fifteen_minute: samples[2],
    })
}

/// `/proc/loadavg` — the Linux path.
///
/// Why: preferred over `getloadavg` on Linux because it is the kernel's own
/// interface, is readable inside a container without a libc call, and is what
/// every other Linux tool reads.
/// Test: `an_unparsable_proc_line_is_an_error`, `a_short_proc_line_is_an_error`,
/// `a_well_formed_proc_line_parses`.
#[cfg(target_os = "linux")]
fn read_proc_loadavg() -> Result<LoadAverage, LoadAverageError> {
    let raw = std::fs::read_to_string("/proc/loadavg").map_err(|source| {
        let errno = source.raw_os_error();
        LoadAverageError::Errno { source, errno }
    })?;
    parse_proc_loadavg(&raw)
}

/// Parse the first three fields of a `/proc/loadavg` line.
///
/// Why: split from the read so the malformed cases are assertable on every
/// platform, including the macOS dev host where `/proc` does not exist.
/// What: whitespace-splits and parses exactly the first three fields; the
/// running/total and last-PID fields after them are ignored.
///
/// # Errors
///
/// [`LoadAverageError::Malformed`] when fewer than three fields are present or
/// any of the three does not parse as a float.
///
/// Test: `a_well_formed_proc_line_parses`, `an_unparsable_proc_line_is_an_error`,
/// `a_short_proc_line_is_an_error`.
pub fn parse_proc_loadavg(raw: &str) -> Result<LoadAverage, LoadAverageError> {
    let mut fields = raw.split_whitespace();
    let mut next = |which: &str| -> Result<f64, LoadAverageError> {
        let field = fields
            .next()
            .ok_or_else(|| LoadAverageError::Malformed {
                origin: "/proc/loadavg",
                detail: format!("missing the {which} field"),
            })?;
        field
            .parse::<f64>()
            .map_err(|err| LoadAverageError::Malformed {
                origin: "/proc/loadavg",
                detail: format!("{which} field {field:?} does not parse: {err}"),
            })
    };
    Ok(LoadAverage {
        one_minute: next("1-minute")?,
        five_minute: next("5-minute")?,
        fifteen_minute: next("15-minute")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live reading on whatever host runs the suite. Deliberately asserts
    /// only what is true of EVERY machine — a threshold assertion here would be
    /// a flake on a loaded CI runner.
    #[test]
    fn a_reading_from_this_host_is_finite_and_non_negative() {
        let avg = read_load_average().expect("a unix host exposes a load average");
        for (label, value) in [
            ("1-minute", avg.one_minute),
            ("5-minute", avg.five_minute),
            ("15-minute", avg.fifteen_minute),
        ] {
            assert!(
                value.is_finite() && value >= 0.0,
                "{label} load {value} is not a plausible run-queue length"
            );
        }
    }

    #[test]
    fn a_well_formed_proc_line_parses() {
        let avg = parse_proc_loadavg("0.52 1.25 2.00 3/1234 56789\n")
            .expect("a canonical /proc/loadavg line parses");
        assert_eq!(avg.one_minute, 0.52);
        assert_eq!(avg.five_minute, 1.25);
        assert_eq!(avg.fifteen_minute, 2.00);
    }

    #[test]
    fn an_unparsable_proc_line_is_an_error() {
        let err = parse_proc_loadavg("0.52 not-a-number 2.00")
            .expect_err("a non-numeric field must not be read as a load");
        assert!(
            matches!(err, LoadAverageError::Malformed { .. }),
            "expected Malformed, got {err:?}"
        );
        assert!(
            format!("{err}").contains("5-minute"),
            "the message must name the bad field: {err}"
        );
    }

    #[test]
    fn a_short_proc_line_is_an_error() {
        // Fewer than three fields is incomplete, never "assume the rest is 0".
        let err = parse_proc_loadavg("0.52 1.25")
            .expect_err("a truncated line must not be padded with a guess");
        assert!(
            format!("{err}").contains("missing the 15-minute field"),
            "{err}"
        );
    }

    #[test]
    fn an_errno_is_reported_for_an_io_failure() {
        let err = LoadAverageError::Errno {
            source: std::io::Error::from_raw_os_error(libc::EPERM),
            errno: Some(libc::EPERM),
        };
        assert_eq!(err.errno(), Some(libc::EPERM));
        // A malformed reading has no errno, and the refusal message must not
        // invent one.
        let malformed = LoadAverageError::Malformed {
            origin: "/proc/loadavg",
            detail: "x".to_string(),
        };
        assert_eq!(malformed.errno(), None);
    }

    #[test]
    fn the_display_form_reads_like_uptime() {
        let avg = LoadAverage {
            one_minute: 21.4,
            five_minute: 21.42,
            fifteen_minute: 21.671,
        };
        assert_eq!(format!("{avg}"), "21.40 21.42 21.67");
    }
}
