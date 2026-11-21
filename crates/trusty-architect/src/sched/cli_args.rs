//! The scheduler binary's command line, parsed by hand (no `clap`).
//!
//! Why: the leaf crate takes no new dependency; the verb set is small.
//! What: [`parse`] reads `<verb> [flags]` into [`Args`]. Verbs: `validate`,
//! `status`, `run` (needs `--once`), `daemon`, `cutover`, `rollback`,
//! `parity`, `plist`. Flags: `--root DIR`, `--config PATH`, `--once`,
//! `--json`, `--since <N>[smhd]`, `--force`, `--min-cycles N`,
//! `--min-match-pct P`, `--exe PATH`. An unknown verb or flag, a flag
//! without its value or a value that does not parse is a usage error.
//! Test: `args_parse_every_verb_and_flag`, `args_reject_unknown_and_malformed`.

use std::path::PathBuf;

use super::SchedError;
use super::slot::parse_every;

/// The verbs.
pub const VERBS: [&str; 8] = [
    "validate", "status", "run", "daemon", "cutover", "rollback", "parity", "plist",
];

/// A parsed command line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Args {
    /// The verb.
    pub verb: String,
    /// `--root`.
    pub root: Option<PathBuf>,
    /// `--config`.
    pub config: Option<PathBuf>,
    /// `--once`.
    pub once: bool,
    /// `--json`.
    pub json: bool,
    /// `--since`, seconds.
    pub since_s: Option<i64>,
    /// `--force`.
    pub force: bool,
    /// `--min-cycles`.
    pub min_cycles: Option<u64>,
    /// `--min-match-pct`.
    pub min_match_pct: Option<f64>,
    /// `--exe`.
    pub exe: Option<PathBuf>,
}

/// Parses `argv` (without the program name).
///
/// # Errors
/// [`SchedError::Usage`] naming the problem.
pub fn parse(argv: &[String]) -> Result<Args, SchedError> {
    let usage = |m: String| SchedError::Usage(m);
    let mut it = argv.iter();
    let verb = it
        .next()
        .ok_or_else(|| usage(format!("missing verb; one of {}", VERBS.join(", "))))?;
    if !VERBS.contains(&verb.as_str()) {
        return Err(usage(format!(
            "unknown verb {verb:?}; one of {}",
            VERBS.join(", ")
        )));
    }
    let mut a = Args {
        verb: verb.clone(),
        ..Args::default()
    };
    while let Some(flag) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| usage(format!("{name} needs a value")))
        };
        match flag.as_str() {
            "--once" => a.once = true,
            "--json" => a.json = true,
            "--force" => a.force = true,
            "--root" => a.root = Some(value("--root")?.into()),
            "--config" => a.config = Some(value("--config")?.into()),
            "--exe" => a.exe = Some(value("--exe")?.into()),
            "--since" => {
                let v = value("--since")?;
                a.since_s = Some(
                    parse_every(&v)
                        .ok_or_else(|| usage(format!("--since {v:?}: want <N>[smhd]")))?,
                );
            }
            "--min-cycles" => {
                let v = value("--min-cycles")?;
                a.min_cycles = Some(
                    v.parse()
                        .map_err(|_| usage(format!("--min-cycles {v:?}: not a number")))?,
                );
            }
            "--min-match-pct" => {
                let v = value("--min-match-pct")?;
                let p: f64 = v
                    .parse()
                    .map_err(|_| usage(format!("--min-match-pct {v:?}: not a number")))?;
                if !(0.0..=100.0).contains(&p) {
                    return Err(usage(format!("--min-match-pct {v:?}: want 0 to 100")));
                }
                a.min_match_pct = Some(p);
            }
            other => return Err(usage(format!("unknown flag {other:?}"))),
        }
    }
    if a.verb == "run" && !a.once {
        return Err(usage(
            "run needs --once; use daemon for a long run".to_owned(),
        ));
    }
    Ok(a)
}
