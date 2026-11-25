//! Catch a `cargo test` target that ran nothing although no filter asked it
//! to skip anything (#11661).
//!
//! Why: libtest counts a test as "filtered out" only when a filter removed
//! it. A target reporting `0 passed; … N filtered out` (N > 0) for a run whose
//! command carried no filter ran a binary that did not match the command — a
//! stale binary from another worktree, or a filter injected somewhere the
//! caller never saw — and cargo still exits 0. A gate that tested nothing must
//! not read as green.
//! What: [`wants_empty_target_check`] decides from the leased argv whether the
//! run is a `cargo test` with no filter (a positional `TESTNAME`, a libtest
//! positional after `--`, `--skip` or `--ignored`). [`EmptyTargetScan`] reads
//! the run's output line by line: cargo's `Running <target>` / `Doc-tests
//! <crate>` lines name the target, libtest's `test result:` line carries the
//! counts, and every target with 0 run and some filtered out is recorded.
//! Test: the `#[cfg(test)]` suite below, and
//! `a_cargo_test_target_that_filters_out_every_test_unasked_fails_11661` in
//! `tests/tm_build_lease.rs`.

/// `cargo test` options that take a separate value (`--opt value`).
///
/// What: a word after one of these is the option's value, never a
/// `TESTNAME`. An option missing from this list makes its value read as a
/// filter, which only skips the check — never a false alarm.
const CARGO_VALUE_OPTS: &[&str] = &[
    "-p",
    "--package",
    "--exclude",
    "--test",
    "--bench",
    "--bin",
    "--example",
    "-F",
    "--features",
    "--target",
    "--target-dir",
    "--manifest-path",
    "-j",
    "--jobs",
    "--profile",
    "--message-format",
    "--color",
    "--config",
    "-Z",
    "-C",
    "--lockfile-path",
];

/// libtest options after `--` that take a separate value.
const LIBTEST_VALUE_OPTS: &[&str] = &[
    "--test-threads",
    "--logfile",
    "--color",
    "--format",
    "-Z",
    "--shuffle-seed",
];

/// Whether the leased `argv` is a `cargo test` that requested no filter.
///
/// What: the program word is `cargo` (any path), optionally after `env` and
/// `KEY=value` words; a `+toolchain` and global options may precede the `test`
/// (or `t`) subcommand. `false` for anything else — the check never runs on a
/// command it cannot read.
/// Test: `only_an_unfiltered_cargo_test_is_checked`.
#[must_use]
pub fn wants_empty_target_check(argv: &[String]) -> bool {
    let mut words = argv.iter().map(String::as_str).peekable();
    while let Some(&w) = words.peek() {
        if w == "env" || (w.contains('=') && !w.starts_with('-')) {
            words.next();
        } else {
            break;
        }
    }
    let is_cargo = words
        .next()
        .is_some_and(|w| std::path::Path::new(w).file_name() == Some("cargo".as_ref()));
    if !is_cargo {
        return false;
    }
    // A toolchain and cargo's global options come before the subcommand.
    let mut found_test = false;
    while let Some(w) = words.next() {
        if w.starts_with('+') {
            continue;
        }
        if w == "test" || w == "t" {
            found_test = true;
            break;
        }
        if !w.starts_with('-') {
            return false;
        }
        if CARGO_VALUE_OPTS.contains(&w) {
            words.next();
        }
    }
    found_test && !requests_filter(&words.collect::<Vec<_>>())
}

/// Whether the words after `cargo test` carry a test filter.
fn requests_filter(rest: &[&str]) -> bool {
    let mut words = rest.iter();
    // Before `--`: cargo's own options, and at most one `TESTNAME`.
    while let Some(&w) = words.next() {
        if w == "--" {
            break;
        }
        if !w.starts_with('-') {
            return true;
        }
        if CARGO_VALUE_OPTS.contains(&w) {
            words.next();
        }
    }
    // After `--`: libtest's arguments; a positional is a filter.
    while let Some(&w) = words.next() {
        if w == "--ignored" || w.starts_with("--skip") || !w.starts_with('-') {
            return true;
        }
        if LIBTEST_VALUE_OPTS.contains(&w) {
            words.next();
        }
    }
    false
}

/// The counts on one libtest `test result:` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    passed: u64,
    failed: u64,
    filtered: u64,
}

/// Parse `test result: ok. 0 passed; 0 failed; …; 7 filtered out; …`.
fn parse_summary(line: &str) -> Option<Counts> {
    let body = line.trim().strip_prefix("test result: ")?;
    let mut counts = Counts {
        passed: 0,
        failed: 0,
        filtered: 0,
    };
    let mut seen = false;
    for part in body.split([';', '.']) {
        let part = part.trim();
        let Some((n, label)) = part.split_once(' ') else {
            continue;
        };
        let Ok(n) = n.parse::<u64>() else {
            continue;
        };
        match label {
            "passed" => (counts.passed, seen) = (n, true),
            "failed" => counts.failed = n,
            "filtered out" => counts.filtered = n,
            _ => {}
        }
    }
    seen.then_some(counts)
}

/// Reads a `cargo test` run's output and records every target that ran no
/// test because all of its tests were filtered out.
///
/// Test: `an_all_filtered_target_is_recorded_with_its_name`,
/// `a_target_that_ran_tests_or_filtered_nothing_is_not_recorded`.
#[derive(Debug, Default)]
pub struct EmptyTargetScan {
    /// The last target cargo announced.
    current: Option<String>,
    /// One description per empty target.
    empty: Vec<String>,
}

impl EmptyTargetScan {
    /// An empty scan.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one output line, from stdout or stderr, without its newline.
    pub fn line(&mut self, line: &str) {
        let trimmed = line.trim();
        if let Some(target) = trimmed
            .strip_prefix("Running ")
            .or_else(|| trimmed.strip_prefix("Doc-tests "))
        {
            self.current = Some(target.to_string());
            return;
        }
        let Some(counts) = parse_summary(trimmed) else {
            return;
        };
        if counts.passed == 0 && counts.failed == 0 && counts.filtered > 0 {
            self.empty.push(format!(
                "{} — 0 tests ran, {} filtered out",
                self.current.as_deref().unwrap_or("a test target"),
                counts.filtered
            ));
        }
    }

    /// Every empty target seen so far.
    #[must_use]
    pub fn empty_targets(&self) -> &[String] {
        &self.empty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn only_an_unfiltered_cargo_test_is_checked() {
        for (cmd, want) in [
            ("cargo test -p trusty-mpm --no-fail-fast", true),
            ("cargo t", true),
            ("/usr/bin/cargo +stable test --features cli -p x", true),
            ("env CARGO_BUILD_JOBS=4 cargo test -p x", true),
            (
                "cargo test -p x -- --include-ignored --test-threads 2",
                true,
            ),
            ("cargo --config a=b test -p x", true),
            ("cargo test -p x some_name", false),
            ("cargo test -p x -- some_name --exact", false),
            ("cargo test -p x -- --skip slow", false),
            ("cargo test -p x -- --ignored", false),
            ("cargo build -p x", false),
            ("cargo clippy -p x -- -D warnings", false),
            ("sh -c cargo", false),
        ] {
            assert_eq!(wants_empty_target_check(&argv(cmd)), want, "{cmd}");
        }
    }

    #[test]
    fn an_all_filtered_target_is_recorded_with_its_name() {
        let mut scan = EmptyTargetScan::new();
        scan.line("     Running unittests src/lib.rs (/t/debug/deps/x-d3f97a3eacd4af69)");
        scan.line("running 0 tests");
        scan.line(
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1656 filtered out; \
             finished in 0.00s",
        );
        assert_eq!(
            scan.empty_targets(),
            [
                "unittests src/lib.rs (/t/debug/deps/x-d3f97a3eacd4af69) — 0 tests ran, 1656 \
              filtered out"
            ]
        );
    }

    #[test]
    fn a_target_that_ran_tests_or_filtered_nothing_is_not_recorded() {
        let mut scan = EmptyTargetScan::new();
        for line in [
            "     Running tests/a.rs (/t/debug/deps/a-0123456789abcdef)",
            "test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 1656 filtered out; \
             finished in 0.01s",
            "   Doc-tests x",
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 0.00s",
            "test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 3 filtered out; \
             finished in 0.00s",
        ] {
            scan.line(line);
        }
        assert!(
            scan.empty_targets().is_empty(),
            "{:?}",
            scan.empty_targets()
        );
    }
}
