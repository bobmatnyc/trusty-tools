//! The command-line verbs behind the `trusty-okf` binary (SPEC-OKF-01
//! §4.10, 3g B).
//!
//! Why: the CLI must refuse bad input before any write (exit 2), report
//! each state axis apart, and exit with the §4.10 code. Keeping it in the
//! library lets the criteria run it against a scripted fake daemon.
//! What: [`run`] parses `import plan`, `import run`, `import status` and
//! `feed`, builds the document-tree store, runs the library calls and
//! prints a text or `--json` report. Every path it uses comes from its
//! arguments: the bundle root, `--from dir:<path>`, and for a feed pass
//! `--index`, `--state-dir` (machine-local, §4.9.1) and `--socket`. It
//! reads no environment variable (B3). `--feed` and `feed` need the
//! `search-feed` feature; without it they are refused with exit 2 before
//! any write. Hidden from the docs: it is the binary's, not an API.
//! Slice S8 of #10746.
//! Test: `crates/trusty-okf/tests/cli_criteria.rs`,
//! `c85_failed_feed_reports_not_fully_searchable_and_exits_4`,
//! `c135_identity_and_no_d1_paths_never_read_searchable`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;

mod adapter_cmd;
mod import_cmd;
mod output;

#[cfg(feature = "search-feed")]
mod feed_cmd;

/// The usage text printed on a bad call.
pub const USAGE: &str = "usage:
  trusty-okf import plan   <bundle-root> --from dir:<path> [--store ID] [--include G]... [--exclude G]...
                           [--worker PATH] [--json]
  trusty-okf import run    <bundle-root> --from dir:<path> [--store ID] [--include G]... [--exclude G]...
                           [--dry-run] [--feed --index ID --state-dir DIR --socket PATH] [--worker PATH] [--json]
  trusty-okf import plan|run <bundle-root> --from adapter:<gmail|drive|gdocs> --adapter PATH
                           --select account=LABEL --select KEY=VALUE... [--adapter-arg A]...
                           [--adapter-var NAME=VALUE]... [--operator-initiated] [--store ID] ...
  trusty-okf import status <bundle-root> [--store ID] [--index ID --state-dir DIR [--socket PATH]] [--json]
  trusty-okf feed          <bundle-root> --index ID --state-dir DIR --socket PATH [--json]
";

/// Options that take a value; `--include` and `--exclude` may repeat.
const VALUED: &[&str] = &[
    "--from",
    "--store",
    "--include",
    "--exclude",
    "--index",
    "--state-dir",
    "--socket",
    // #10746 S10: the extraction worker binary (SPEC-OKF-01 §4.10).
    "--worker",
    // #10746 S8: test-only. Fixes the run's `now` so a test can compare two
    // runs byte for byte; left out of `USAGE` on purpose (Architect ruling).
    // Without it the CLI takes the system clock.
    "--now",
    // #10746 S9: the adapter process of an authenticated source (3m A).
    "--adapter",
    "--adapter-arg",
    "--adapter-var",
    "--select",
];
/// Options that may repeat.
const REPEATED: &[&str] = &[
    "--include",
    "--exclude",
    "--adapter-arg",
    "--adapter-var",
    "--select",
];
const FLAGS: &[&str] = &["--json", "--feed", "--dry-run", "--operator-initiated"];

/// Parsed arguments: positionals in order, and options.
#[derive(Debug, Default)]
pub(crate) struct Args {
    pub pos: Vec<String>,
    pub opts: BTreeMap<String, Vec<String>>,
    pub flags: BTreeSet<String>,
}

impl Args {
    pub(crate) fn one(&self, k: &str) -> Option<&str> {
        self.opts.get(k).and_then(|v| v.first()).map(String::as_str)
    }

    pub(crate) fn all(&self, k: &str) -> Vec<String> {
        self.opts.get(k).cloned().unwrap_or_default()
    }

    pub(crate) fn has(&self, k: &str) -> bool {
        self.flags.contains(k)
    }

    /// Refuse any option outside `allowed`.
    pub(crate) fn only(&self, allowed: &[&str]) -> Result<(), String> {
        for k in self.opts.keys().chain(self.flags.iter()) {
            if !allowed.contains(&k.as_str()) {
                return Err(format!("{k} is not an option of this command"));
            }
        }
        Ok(())
    }
}

fn parse(args: &[OsString]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let a = a.to_str().ok_or("an argument is not UTF-8")?.to_string();
        if VALUED.contains(&a.as_str()) {
            let v = it.next().ok_or(format!("{a} needs a value"))?;
            let v = v.to_str().ok_or("an argument is not UTF-8")?.to_string();
            let slot = out.opts.entry(a.clone()).or_default();
            if !slot.is_empty() && !REPEATED.contains(&a.as_str()) {
                return Err(format!("{a} given twice"));
            }
            slot.push(v);
        } else if FLAGS.contains(&a.as_str()) {
            out.flags.insert(a);
        } else if a.starts_with("--") {
            return Err(format!("unknown option {a}"));
        } else {
            out.pos.push(a);
        }
    }
    Ok(out)
}

/// Where a feed pass sends its calls: the daemon socket, or a test's fake.
pub(crate) struct Ctx<'a> {
    #[cfg(feature = "search-feed")]
    pub sink: Option<&'a dyn crate::feed::IndexFeed>,
    /// #10746 S10 fix: the process is the `trusty-okf` binary, so SIGINT
    /// may be taken over to cancel a worker run (never in a library call).
    pub catch_interrupt: bool,
    _p: std::marker::PhantomData<&'a ()>,
}

/// Run one CLI call; returns the §4.10 exit code.
///
/// What: see the module doc. `out` gets the report, `err` the errors and
/// the usage. A bad call is 2 and writes nothing.
pub fn run(args: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let ctx = Ctx {
        #[cfg(feature = "search-feed")]
        sink: None,
        catch_interrupt: false,
        _p: std::marker::PhantomData,
    };
    run_ctx(args, out, err, &ctx)
}

/// The binary's entry: [`run`], plus (#10746 S10 fix) Ctrl-C handling. In
/// an `import run --worker` call, SIGINT cancels the run: the in-flight
/// worker item fails `cancelled`, the worker's process group is killed and
/// reaped, later items are skipped `cancelled`, and the report is printed
/// (exit 3). Only `src/main.rs` calls this.
/// Test: `c103_sigint_cancels_the_cli_and_reaps_the_worker`.
pub fn run_binary(args: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let ctx = Ctx {
        #[cfg(feature = "search-feed")]
        sink: None,
        catch_interrupt: true,
        _p: std::marker::PhantomData,
    };
    run_ctx(args, out, err, &ctx)
}

/// [`run`] with every daemon call sent to `sink` instead of `--socket`.
#[cfg(feature = "search-feed")]
#[doc(hidden)]
pub fn run_with_feed(
    args: &[OsString],
    out: &mut dyn Write,
    err: &mut dyn Write,
    sink: &dyn crate::feed::IndexFeed,
) -> i32 {
    let ctx = Ctx {
        sink: Some(sink),
        catch_interrupt: false,
        _p: std::marker::PhantomData,
    };
    run_ctx(args, out, err, &ctx)
}

fn run_ctx(args: &[OsString], out: &mut dyn Write, err: &mut dyn Write, ctx: &Ctx<'_>) -> i32 {
    let parsed = match parse(args) {
        Ok(a) => a,
        Err(e) => return usage(err, &e),
    };
    let verb: Vec<&str> = parsed.pos.iter().map(String::as_str).take(2).collect();
    let result = match verb.as_slice() {
        ["import", "plan", ..] => import_cmd::plan(&parsed, out),
        ["import", "run", ..] => import_cmd::run(&parsed, out, ctx),
        ["import", "status", ..] => import_cmd::status(&parsed, out, ctx),
        ["feed", ..] => feed_verb(&parsed, out, ctx),
        _ => Err(Fail::Usage(
            "name a verb: import plan|run|status, or feed".into(),
        )),
    };
    match result {
        Ok(code) => code,
        Err(Fail::Usage(e)) => usage(err, &e),
        Err(Fail::Code(code, e)) => {
            let _ = writeln!(err, "trusty-okf: {e}");
            code
        }
    }
}

#[cfg(feature = "search-feed")]
fn feed_verb(a: &Args, out: &mut dyn Write, ctx: &Ctx<'_>) -> Result<i32, Fail> {
    feed_cmd::feed(a, out, ctx)
}

#[cfg(not(feature = "search-feed"))]
fn feed_verb(_a: &Args, _out: &mut dyn Write, _ctx: &Ctx<'_>) -> Result<i32, Fail> {
    Err(no_feed())
}

/// The refusal of a feed call in a build without `search-feed`.
#[cfg(not(feature = "search-feed"))]
pub(crate) fn no_feed() -> Fail {
    Fail::Code(
        2,
        "this build has no search-feed feature; feed passes are not available".into(),
    )
}

/// A CLI failure: a bad call (2, with usage) or a coded error.
pub(crate) enum Fail {
    Usage(String),
    Code(i32, String),
}

fn usage(err: &mut dyn Write, e: &str) -> i32 {
    let _ = writeln!(err, "trusty-okf: {e}\n{USAGE}");
    2
}
