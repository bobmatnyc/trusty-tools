//! Test stand-in for a script that prints and logs the time (#12028).
//!
//! Why: AC2 of #12028 needs time-stamped output (a soak log, say) to replay
//! without a diff, and shows the seam a real port uses: the ported code takes
//! a [`Clock`], and only a replay entry point like this one, never a shipped
//! binary, reads [`CLOCK_ENV`] to pick a fixed one. This example never ships.
//! What: prints `time.time()`, a `%Y-%m-%d %H:%M:%S %Z` local time and
//! `datetime.now().isoformat()`, then appends `<local time> soak ok` to
//! `soak.log`, as the Python script in `tests/replay_clock.rs` does. Local
//! time is UTC: the recorder pins `TZ=UTC`. `--ignore-clock` reads the live
//! clock instead, the port that forgot the seam.
//! Test: `a_clocked_script_replays_clean_against_a_stand_in_that_reads_the_seam`,
//! `a_stand_in_that_ignores_the_clock_seam_reports_a_diff`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::replay::CLOCK_ENV;
use trusty_architect::{Clock, FakeClock, SystemClock};

/// `%Y-%m-%dT%H:%M:%S` in UTC for epoch second `t`.
fn utc(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, mi, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

/// The ported logic: everything time-dependent reads `clock`.
fn run(clock: &dyn Clock) -> std::io::Result<()> {
    let now = clock.now_unix();
    let stamp = utc(now.floor() as i64);
    println!("{now:?}");
    println!("{} UTC", stamp.replace('T', " "));
    println!("{stamp}");
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("soak.log")?;
    writeln!(log, "{stamp} soak ok")
}

fn main() -> ExitCode {
    let ignore = std::env::args().any(|a| a == "--ignore-clock");
    let frozen = std::env::var(CLOCK_ENV)
        .ok()
        .and_then(|v| v.parse::<i64>().ok());
    let result = match frozen {
        Some(t) if !ignore => run(&FakeClock::new(t as f64)),
        _ => run(&SystemClock),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("replay_standin_clock: {e}");
            ExitCode::from(2)
        }
    }
}
