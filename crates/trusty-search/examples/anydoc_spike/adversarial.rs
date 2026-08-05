//! Adversarial-parity leg.
//!
//! Every probe runs in a re-exec'd child under a wall-clock bound, because
//! the outcomes worth distinguishing include ones that end the process. A
//! stack-overflow abort (`SIGABRT`) is not a panic: `catch_unwind` does not
//! see it, and neither does `spawn_blocking`'s `JoinError` containment, so
//! measuring it in-process would take the harness down with it and report
//! nothing.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::bench::parse_rss;
use crate::engines::{fmt_mib, fmt_ms, Engine};
use crate::fixtures::Corpus;

/// How long a child gets before it is judged hung and killed.
///
/// Set above the production `EXTRACT_TIMEOUT` (30s) so "our timeout would
/// have fired" and "the parser never terminates" stay distinguishable.
const CHILD_BUDGET: Duration = Duration::from_secs(45);

/// What a probe did, in the vocabulary that matters for a daemon.
#[derive(Debug)]
enum Verdict {
    /// Returned a value. Contains the one-line summary.
    Returned(String),
    /// Unwinding panic — contained by `spawn_blocking`'s JoinError today.
    Panicked,
    /// Killed by a signal. Contains the signal number. Signal 6 (SIGABRT) is
    /// the uncatchable stack-overflow abort; the process dies regardless of
    /// any containment the caller wrote.
    Signalled(i32),
    /// Still running when the budget expired.
    Hung,
}

impl Verdict {
    fn cell(&self) -> String {
        match self {
            Verdict::Returned(s) => s.clone(),
            Verdict::Panicked => "**PANIC** (unwinding — contained)".to_string(),
            Verdict::Signalled(6) => "**SIGABRT** (uncatchable — process dies)".to_string(),
            Verdict::Signalled(11) => "**SIGSEGV** (uncatchable — process dies)".to_string(),
            Verdict::Signalled(n) => format!("**signal {n}** (uncatchable)"),
            Verdict::Hung => format!("**HUNG** (> {}s, killed)", CHILD_BUDGET.as_secs()),
        }
    }
}

struct Probe {
    verdict: Verdict,
    elapsed: Duration,
    peak_rss: u64,
}

pub fn run(corpus: &Corpus) {
    let fixtures = corpus.adversarial();

    println!("## Adversarial parity\n");
    println!(
        "Each cell is one isolated child process, budget {}s. Both extractors sit behind the same \
         10 MiB `MAX_OFFICE_FILE_BYTES` gate in production; every fixture here is under it.\n",
        CHILD_BUDGET.as_secs()
    );
    println!("| fixture | bytes | native outcome | native ms | native peak MiB | anydoc outcome | anydoc ms | anydoc peak MiB |");
    println!("|---|---:|---|---:|---:|---|---:|---:|");

    for f in &fixtures {
        let n = probe(Engine::Native, &f.path);
        let a = probe(Engine::Anydoc, &f.path);
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            f.name,
            f.size(),
            n.verdict.cell(),
            fmt_ms(n.elapsed),
            fmt_mib(n.peak_rss),
            a.verdict.cell(),
            fmt_ms(a.elapsed),
            fmt_mib(a.peak_rss),
        );
    }

    println!("\n### What each fixture probes\n");
    for f in &fixtures {
        println!("- `{}` — {}", f.name, f.note);
    }
}

fn probe(engine: Engine, path: &Path) -> Probe {
    let exe = std::env::current_exe().expect("current_exe");
    let start = Instant::now();
    let mut child = Command::new(exe)
        .arg("run-one")
        .arg(engine.label())
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn probe child");

    // Poll rather than `wait()` so a non-terminating parse is reported as
    // HUNG instead of stalling the harness indefinitely.
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(s) => break Some(s),
            None if start.elapsed() > CHILD_BUDGET => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    };
    let elapsed = start.elapsed();

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut o) = child.stdout.take() {
        use std::io::Read;
        let _ = o.read_to_string(&mut stdout);
    }
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut stderr);
    }

    let verdict = match status {
        None => Verdict::Hung,
        Some(s) => classify(s, &stdout, &stderr),
    };
    Probe {
        verdict,
        elapsed,
        peak_rss: parse_rss(&stdout),
    }
}

fn classify(status: std::process::ExitStatus, stdout: &str, stderr: &str) -> Verdict {
    use std::os::unix::process::ExitStatusExt;
    if let Some(sig) = status.signal() {
        return Verdict::Signalled(sig);
    }
    if let Some(line) = stdout
        .lines()
        .find_map(|l| l.strip_prefix("RESULT="))
        .map(str::to_string)
    {
        return Verdict::Returned(line);
    }
    // No RESULT line and no signal: the child aborted through the panic
    // handler (which prints to stderr and exits non-zero) or died some other
    // way we should not silently score as success.
    if stderr.contains("panicked at") {
        Verdict::Panicked
    } else {
        Verdict::Returned(format!("no result, exit {:?}", status.code()))
    }
}
