//! Bounding a clone that is ALREADY RUNNING (#5669).
//!
//! Why: [`CloneOptions::budget_bytes`](super::CloneOptions::budget_bytes) used
//! to be checked only between repositories, so 19 GiB spent against a 20 GiB
//! ceiling still admitted a 100 GB monorepo and finished at 119 GiB. This runs
//! unattended on a client's own machine during an audit engagement, and the
//! recipient has no view of the run while it happens. A start gate that cannot
//! interrupt is not a budget.
//!
//! What: while the acquisition child runs, its staged tree is measured at a
//! bounded interval. Once `spent + staged` crosses the ceiling the child is
//! signalled — `SIGTERM`, then `SIGKILL` if it does not exit within the grace —
//! and the caller gets [`Outcome::OverBudget`], which
//! [`finish_one`](super::finish_one) turns into
//! [`CloneState::BudgetExceeded`](super::CloneState::BudgetExceeded) and a
//! removed staged tree. The start gate stays: it is what stops a clone nobody
//! needs to start, and this is what stops one already fetching.
//!
//! **A finished child does not exempt its tree.** The exit is measured one last
//! time before it is reported as complete, so a clone that outran the sampling
//! interval is bounded too — and so the answer at the boundary, a child exiting
//! in the same instant the budget trips, is the same whichever arm observes it.
//! Exactly one verdict is produced per child either way.
//!
//! **A sample that fails does not disable the watchdog.** It fails the clone
//! CLOSED — the child is killed and the attempt reported — because a watchdog
//! that silently stops measuring is the fail-open this module exists to remove.
//! A PARTIAL walk is different and is not a failure: `git` creates and removes
//! temporary pack files throughout a fetch, so a walk racing one is ordinary.
//! That count is a floor, which still trips the ceiling when it crosses it and
//! is re-measured on the next interval when it does not. The tree's own ROOT is
//! the exception — an unopenable root would count 0 bytes forever, so it is
//! opened explicitly and fails closed like any other unmeasurable sample.
//!
//! Test: `super::watchdog::watchdog_tests`.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

/// How often a running clone's staged tree is measured.
///
/// The interval bounds the overshoot: a clone can exceed the ceiling by at most
/// what it writes in one sample period. Half a second keeps that overshoot
/// small against a multi-gigabyte fetch while a recursive walk of a partial
/// checkout stays cheap.
pub(super) const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

/// How long a signalled child has to exit before it is killed outright.
pub(super) const TERM_GRACE: Duration = Duration::from_secs(5);

/// The ceiling one clone is watched against.
///
/// `spent` is what earlier repositories in the same run already committed, so
/// the comparison is against the run's total rather than this repository alone
/// — the same arithmetic the start gate performs.
/// Test: `super::watchdog::watchdog_tests::the_ceiling_counts_what_earlier_repositories_spent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Watch {
    /// Bytes earlier repositories in this run already put on disk.
    pub spent: u64,
    /// The ceiling `spent + staged` must not cross.
    pub budget_bytes: u64,
    /// How often the staged tree is measured.
    pub interval: Duration,
    /// How long the child gets after `SIGTERM` before `SIGKILL`.
    pub grace: Duration,
}

impl Watch {
    /// A watch at the production interval and grace.
    pub(super) fn new(spent: u64, budget_bytes: u64) -> Self {
        Self {
            spent,
            budget_bytes,
            interval: SAMPLE_INTERVAL,
            grace: TERM_GRACE,
        }
    }

    /// Would this staged size put the run over its ceiling?
    fn over(&self, staged_bytes: u64) -> bool {
        self.spent.saturating_add(staged_bytes) > self.budget_bytes
    }
}

/// How the child is named when its failure becomes a gap line.
#[derive(Debug, Clone, Copy)]
pub(super) struct Spawn<'a> {
    /// The command, as an operator would recognise it.
    pub label: &'a str,
    /// Appended when the binary itself is not on `PATH`.
    pub missing_hint: &'a str,
}

/// What one watched acquisition produced.
///
/// [`Outcome::OverBudget`] is deliberately not a flavour of [`Outcome::Failed`]:
/// nothing went wrong with the repository or the remote, and a recipient
/// reading "FAILED" would go looking for a fault that does not exist (#5669).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    /// The child exited zero and its tree is within the ceiling.
    Completed,
    /// Acquisition failed, for this reason.
    Failed(String),
    /// The clone was stopped because its staged tree crossed the ceiling.
    OverBudget {
        /// What the tree measured when it was stopped.
        staged_bytes: u64,
        /// The ceiling it crossed.
        budget_bytes: u64,
    },
}

/// Spawn the acquisition child and watch it against the budget.
///
/// stdout is discarded — no caller has ever read it — and stderr is drained by
/// a task of its own so a chatty `git` cannot fill the pipe buffer and wedge a
/// child this function is otherwise prepared to wait on indefinitely.
/// Test: `super::watchdog::watchdog_tests::a_missing_binary_names_itself`.
pub(super) async fn run(
    spec: &Spawn<'_>,
    mut command: Command,
    staged: &Path,
    watch: Option<Watch>,
) -> Outcome {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(source) => {
            let hint = if source.kind() == std::io::ErrorKind::NotFound {
                format!(" {}", spec.missing_hint)
            } else {
                String::new()
            };
            return Outcome::Failed(format!("`{}` could not be run: {source}{hint}", spec.label));
        }
    };
    watch_child(spec, child, staged, watch).await
}

/// Watch an already-spawned child, so a test can hold its pid.
///
/// Test: `super::watchdog::watchdog_tests::a_killed_child_leaves_no_process_behind`.
pub(super) async fn watch_child(
    spec: &Spawn<'_>,
    mut child: Child,
    staged: &Path,
    watch: Option<Watch>,
) -> Outcome {
    let drain = child.stderr.take().map(|mut pipe| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf).await;
            buf
        })
    });
    let verdict = supervise(&mut child, staged, watch).await;
    let stderr = match drain {
        Some(handle) => handle.await.unwrap_or_default(),
        None => Vec::new(),
    };
    report(spec, verdict, &stderr, watch)
}

/// What the supervision loop observed, before it is worded for a report.
#[derive(Debug)]
enum Verdict {
    /// The child exited, within budget.
    Exited(std::process::ExitStatus),
    /// The child could not be waited on at all.
    WaitFailed(std::io::Error),
    /// The staged tree crossed the ceiling; the child is no longer running.
    OverBudget(u64),
    /// The staged tree could not be measured; the child is no longer running.
    Unmeasurable(String),
}

/// Run the child to its end, measuring the staged tree as it goes.
///
/// Returns exactly once, so a child is never classified twice — the completion
/// arm and the sampling arm are alternatives, not both.
/// Test: `super::watchdog::watchdog_tests::a_child_that_exits_as_the_budget_trips_is_over_budget`.
async fn supervise(child: &mut Child, staged: &Path, watch: Option<Watch>) -> Verdict {
    // #5669: with no ceiling there is nothing to enforce, so the child is not
    // sampled at all — an unbudgeted run pays no walk.
    let Some(watch) = watch else {
        return match child.wait().await {
            Ok(status) => Verdict::Exited(status),
            Err(source) => Verdict::WaitFailed(source),
        };
    };
    let mut seen = false;
    loop {
        tokio::select! {
            status = child.wait() => {
                let status = match status {
                    Ok(status) => status,
                    Err(source) => return Verdict::WaitFailed(source),
                };
                // #5669: the child finishing does not exempt its tree. One last
                // measurement is what bounds a clone that outran the interval,
                // and what makes the boundary — an exit in the same instant the
                // budget trips — answer the same whichever arm saw it first.
                return match sample(staged, &mut seen) {
                    Sample::Bytes(bytes) if watch.over(bytes) => Verdict::OverBudget(bytes),
                    Sample::Bytes(_) => Verdict::Exited(status),
                    Sample::Unmeasurable(why) => Verdict::Unmeasurable(why),
                };
            }
            () = tokio::time::sleep(watch.interval) => {
                match sample(staged, &mut seen) {
                    Sample::Bytes(bytes) if watch.over(bytes) => {
                        terminate(child, watch.grace).await;
                        return Verdict::OverBudget(bytes);
                    }
                    Sample::Bytes(_) => {}
                    Sample::Unmeasurable(why) => {
                        // #5669: fails CLOSED. A watchdog that cannot measure
                        // and keeps going is not watching anything.
                        terminate(child, watch.grace).await;
                        return Verdict::Unmeasurable(why);
                    }
                }
            }
        }
    }
}

/// End the child, politely and then not.
///
/// `SIGTERM` first because both `git` and `gh` remove their temporary pack
/// files on it, which leaves less for the caller's `remove_dir_all` to walk.
/// `SIGKILL` follows on the grace expiring, and the child is reaped either way
/// — an unreaped child is the orphan this whole path exists to avoid.
/// Test: `super::watchdog::watchdog_tests::a_killed_child_leaves_no_process_behind`.
async fn terminate(child: &mut Child, grace: Duration) {
    if let Some(pid) = child.id() {
        // `libc::kill` is the only way to send a signal other than `SIGKILL`;
        // `Child::kill` sends `SIGKILL` unconditionally. The pid is this
        // process's own child and has not been reaped, so it cannot have been
        // reused.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        if tokio::time::timeout(grace, child.wait()).await.is_ok() {
            return;
        }
    }
    let _ = child.kill().await;
}

/// One measurement of the staged tree.
#[derive(Debug, PartialEq, Eq)]
enum Sample {
    /// Bytes on disk, a floor when part of the walk was unreadable.
    Bytes(u64),
    /// The tree could not be measured at all, for this reason.
    Unmeasurable(String),
}

/// Measure the staged tree, distinguishing "not there yet" from "gone".
///
/// Why: the staged directory is created by the CHILD, not by the caller, so an
/// absent tree before the first successful measurement is ordinary and an
/// absent tree after one is the failure the watchdog must not run blind past.
/// `seen` is the only state that separates them.
/// Test: `super::watchdog::watchdog_tests::an_absent_tree_reads_as_empty_until_it_has_been_seen`,
/// `super::watchdog::watchdog_tests::a_tree_that_vanishes_after_being_seen_is_unmeasurable`,
/// `super::watchdog::watchdog_tests::an_unopenable_tree_root_is_unmeasurable_not_empty`.
fn sample(staged: &Path, seen: &mut bool) -> Sample {
    match std::fs::symlink_metadata(staged) {
        Ok(meta) if meta.is_dir() => {
            // #5669: `dir_size` answers `(0, false)` when the tree's own root
            // cannot be opened, which reads as an empty tree and never trips
            // the ceiling. Open the root here so that case fails CLOSED like
            // every other unmeasurable one. Entries BELOW the root stay a
            // floor, because `git` creating and removing pack files mid-fetch
            // makes a partial walk ordinary rather than a failure.
            if let Err(source) = std::fs::read_dir(staged) {
                return Sample::Unmeasurable(format!(
                    "the staged tree at {} could not be measured: {source}",
                    staged.display()
                ));
            }
            *seen = true;
            // The completeness flag is deliberately dropped: a partial walk is
            // a floor, and a floor over the ceiling still trips it.
            let (bytes, _floor) = super::dir_size(staged);
            Sample::Bytes(bytes)
        }
        Ok(_) => Sample::Unmeasurable(format!(
            "the staged tree at {} is not a directory",
            staged.display()
        )),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound && !*seen => Sample::Bytes(0),
        Err(source) => Sample::Unmeasurable(format!(
            "the staged tree at {} could not be measured: {source}",
            staged.display()
        )),
    }
}

/// Word one verdict as the outcome a gap line is built from.
fn report(spec: &Spawn<'_>, verdict: Verdict, stderr: &[u8], watch: Option<Watch>) -> Outcome {
    let budget_bytes = watch.map_or(0, |w| w.budget_bytes);
    match verdict {
        Verdict::Exited(status) if status.success() => Outcome::Completed,
        Verdict::Exited(status) => Outcome::Failed(format!(
            "`{}` exited {status}: {}",
            spec.label,
            String::from_utf8_lossy(stderr).trim()
        )),
        Verdict::WaitFailed(source) => Outcome::Failed(format!(
            "`{}` could not be waited for: {source}",
            spec.label
        )),
        Verdict::OverBudget(staged_bytes) => Outcome::OverBudget {
            staged_bytes,
            budget_bytes,
        },
        Verdict::Unmeasurable(why) => Outcome::Failed(format!(
            "`{}` was stopped because the disk budget could not be enforced: {why}",
            spec.label
        )),
    }
}

#[cfg(test)]
mod watchdog_tests {
    use super::*;

    const SPEC: Spawn<'static> = Spawn {
        label: "fake clone",
        missing_hint: "install it",
    };

    /// The budget-plus-tolerance a stop is asserted against.
    ///
    /// [`tiny_watch`] samples every 5ms and allows a 200ms grace, so a working
    /// watchdog finishes in tens of milliseconds. Ten seconds is loose enough
    /// that a loaded CI box never trips it and tight enough that a watchdog
    /// which never fires fails the test instead of hanging it.
    const STOP_DEADLINE: Duration = Duration::from_secs(10);

    /// A child that appends `chunk` bytes to `<dir>/blob`, `iterations` times.
    ///
    /// Every write is a shell builtin, so the child forks nothing — a kill of
    /// this pid is a kill of the whole job, and the no-orphan assertion is
    /// about the process the watchdog actually signalled.
    fn writer(dir: &Path, chunk: usize, iterations: u64) -> Command {
        let script = r#"mkdir -p "$1" || exit 1
i=0
while [ "$i" -lt "$3" ]; do
  printf '%s' "$2" >> "$1/blob"
  i=$((i + 1))
done
"#;
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(script)
            .arg("watchdog-fixture")
            .arg(dir)
            .arg("x".repeat(chunk))
            .arg(iterations.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // A fixture that outlives a FAILING test is an orphan writing to
            // the disk this module is about. Dropping the child ends it.
            .kill_on_drop(true);
        command
    }

    /// [`watch_child`] under a deadline.
    ///
    /// Every runaway fixture here writes until it is stopped, so a regression
    /// that never fires the watchdog would WEDGE the suite rather than report
    /// itself. The deadline turns that hang into an ordinary failure (#5669).
    async fn stopped(child: Child, staged: &Path, watch: Watch) -> Outcome {
        tokio::time::timeout(
            STOP_DEADLINE,
            watch_child(&SPEC, child, staged, Some(watch)),
        )
        .await
        .unwrap_or_else(|_| panic!("the watchdog left the child running past {STOP_DEADLINE:?}"))
    }

    /// Is this pid still a process on this machine?
    fn alive(pid: u32) -> bool {
        // Signal 0 performs the permission and existence check without
        // delivering anything.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    fn tiny_watch(spent: u64, budget_bytes: u64) -> Watch {
        Watch {
            spent,
            budget_bytes,
            interval: Duration::from_millis(5),
            grace: Duration::from_millis(200),
        }
    }

    /// #5669's whole point: a clone in flight is stopped, not merely refused.
    ///
    /// The child would run for hours, so the assertion is about BOTH halves —
    /// the verdict is over-budget, and it arrives promptly. A watchdog that
    /// eventually notices is the same fail-open as one that never does, so the
    /// deadline is asserted rather than left to the harness to time out on.
    #[tokio::test]
    async fn a_running_child_that_crosses_the_ceiling_is_stopped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        let child = writer(&staged, 1024, u64::from(u32::MAX))
            .spawn()
            .expect("/bin/sh");

        let started = std::time::Instant::now();
        let outcome = stopped(child, &staged, tiny_watch(0, 4096)).await;
        let elapsed = started.elapsed();

        let Outcome::OverBudget {
            staged_bytes,
            budget_bytes,
        } = outcome
        else {
            panic!("a runaway clone must be stopped, not {outcome:?}");
        };
        assert!(
            staged_bytes > 4096,
            "it crossed the ceiling: {staged_bytes}"
        );
        assert_eq!(budget_bytes, 4096);
        assert!(
            elapsed < STOP_DEADLINE,
            "stopped after {elapsed:?}, past the {STOP_DEADLINE:?} deadline"
        );
    }

    /// The reaping #5669 owes, asserted on the child itself rather than on a
    /// live-pid probe: `try_wait` answers `Some` only once the status has been
    /// collected, so a zombie left behind fails this.
    #[tokio::test]
    async fn a_terminated_child_is_reaped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut child = writer(&tmp.path().join("staged"), 1024, u64::from(u32::MAX))
            .spawn()
            .expect("/bin/sh");

        terminate(&mut child, Duration::from_millis(200)).await;

        assert!(
            matches!(child.try_wait(), Ok(Some(_))),
            "the signalled child must be reaped, not left unwaited-for"
        );
    }

    /// Several clones watched at once: each verdict is its own, each child dies.
    ///
    /// `clone_all` watches one repository at a time today, but the watchdog is
    /// what a concurrent caller would reuse, and a shared timer or a signal
    /// aimed at the wrong pid would only show up with more than one in flight.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_watches_each_stop_their_own_child() {
        const WATCHED: u32 = 4;
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut pids = Vec::new();
        let mut running = Vec::new();
        for i in 0..WATCHED {
            let staged = tmp.path().join(format!("staged-{i}"));
            let child = writer(&staged, 1024, u64::from(u32::MAX))
                .spawn()
                .expect("/bin/sh");
            pids.push(child.id().expect("a freshly spawned child has a pid"));
            running.push(tokio::spawn(async move {
                stopped(child, &staged, tiny_watch(0, 4096)).await
            }));
        }

        for (i, handle) in running.into_iter().enumerate() {
            let outcome = handle.await.expect("the watch task must not panic");
            assert!(
                matches!(outcome, Outcome::OverBudget { .. }),
                "watch {i} produced {outcome:?}"
            );
        }
        for pid in pids {
            assert!(!alive(pid), "pid {pid} survived its own watch");
        }
    }

    /// A root that cannot be opened must not read as an empty tree (#5669).
    ///
    /// `dir_size` answers `(0, false)` for it, which would hold the sample at
    /// zero for the whole clone and never trip the ceiling — the fail-open the
    /// watchdog exists to remove.
    #[test]
    fn an_unopenable_tree_root_is_unmeasurable_not_empty() {
        use std::os::unix::fs::PermissionsExt as _;

        // root ignores the mode bits, so the case cannot be staged there.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        std::fs::create_dir_all(&staged).expect("mkdir");
        std::fs::write(staged.join("blob"), vec![b'x'; 8192]).expect("write");
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o000)).expect("chmod");

        let mut seen = true;
        let sampled = sample(&staged, &mut seen);

        // Restore before the assertion so the tempdir can always be removed.
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let Sample::Unmeasurable(why) = sampled else {
            panic!("an unreadable tree root must not read as empty: {sampled:?}");
        };
        assert!(why.contains("could not be measured"), "{why}");
    }

    /// The kill reaps: nothing is left running or unwaited-for.
    #[tokio::test]
    async fn a_killed_child_leaves_no_process_behind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        let child = writer(&staged, 1024, u64::from(u32::MAX))
            .spawn()
            .expect("/bin/sh");
        let pid = child.id().expect("a freshly spawned child has a pid");

        let outcome = stopped(child, &staged, tiny_watch(0, 4096)).await;

        assert!(matches!(outcome, Outcome::OverBudget { .. }), "{outcome:?}");
        assert!(!alive(pid), "pid {pid} survived the kill path");
    }

    /// A clone that stays under the ceiling is untouched.
    #[tokio::test]
    async fn a_child_under_the_ceiling_completes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        let child = writer(&staged, 64, 4).spawn().expect("/bin/sh");

        let outcome = watch_child(&SPEC, child, &staged, Some(tiny_watch(0, 1_000_000))).await;

        assert_eq!(outcome, Outcome::Completed);
        assert!(staged.join("blob").is_file(), "the tree is left in place");
    }

    /// The boundary #5669 has to answer once: the child exits in the same
    /// instant the tree crosses the ceiling.
    ///
    /// The interval is set beyond the child's whole lifetime, so the completion
    /// arm is the only one that can fire — and it still reports over-budget,
    /// which is what makes the two arms agree instead of racing.
    #[tokio::test]
    async fn a_child_that_exits_as_the_budget_trips_is_over_budget() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        let child = writer(&staged, 1024, 1).spawn().expect("/bin/sh");
        let watch = Watch {
            interval: Duration::from_secs(600),
            ..tiny_watch(0, 100)
        };

        let outcome = watch_child(&SPEC, child, &staged, Some(watch)).await;

        let Outcome::OverBudget { staged_bytes, .. } = outcome else {
            panic!("a completed clone over the ceiling is still over it, not {outcome:?}");
        };
        assert!(staged_bytes >= 1024, "{staged_bytes}");
    }

    /// A sample that cannot be taken fails the clone CLOSED, killing the child.
    #[tokio::test]
    async fn an_unmeasurable_tree_fails_the_clone_closed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // The staged path is a FILE, so every sample of it fails from the first
        // one — the same shape as a permission error, without needing one.
        let staged = tmp.path().join("staged");
        std::fs::write(&staged, b"not a directory").expect("write");
        let child = writer(&tmp.path().join("elsewhere"), 1024, u64::from(u32::MAX))
            .spawn()
            .expect("/bin/sh");
        let pid = child.id().expect("a freshly spawned child has a pid");

        let outcome = stopped(child, &staged, tiny_watch(0, 4096)).await;

        let Outcome::Failed(why) = outcome else {
            panic!("an unmeasurable tree must fail the clone, not {outcome:?}");
        };
        assert!(
            why.contains("disk budget could not be enforced"),
            "the reason names the watchdog, not the remote: {why}"
        );
        assert!(!alive(pid), "pid {pid} survived the fail-closed kill");
    }

    /// The child creates the staged directory, so absence before it does is not
    /// a measurement failure.
    #[test]
    fn an_absent_tree_reads_as_empty_until_it_has_been_seen() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut seen = false;
        assert_eq!(
            sample(&tmp.path().join("nope"), &mut seen),
            Sample::Bytes(0)
        );
        assert!(!seen, "nothing was measured, so nothing was seen");
    }

    /// Absence AFTER a successful measurement is the fail-closed case.
    #[test]
    fn a_tree_that_vanishes_after_being_seen_is_unmeasurable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        std::fs::create_dir_all(&staged).expect("mkdir");
        std::fs::write(staged.join("f"), b"0123456789").expect("write");

        let mut seen = false;
        assert_eq!(sample(&staged, &mut seen), Sample::Bytes(10));
        assert!(seen);

        std::fs::remove_dir_all(&staged).expect("rm");
        let Sample::Unmeasurable(why) = sample(&staged, &mut seen) else {
            panic!("a tree that vanished mid-clone must not read as empty");
        };
        assert!(why.contains("could not be measured"), "{why}");
    }

    /// The ceiling is the RUN's, not this repository's.
    #[test]
    fn the_ceiling_counts_what_earlier_repositories_spent() {
        let watch = Watch::new(19, 20);
        assert!(!watch.over(1), "19 + 1 is the ceiling, not past it");
        assert!(watch.over(2), "19 + 2 is past it");
        assert_eq!(watch.interval, SAMPLE_INTERVAL);
        assert_eq!(watch.grace, TERM_GRACE);
    }

    /// No ceiling means no sampling and no interruption.
    #[tokio::test]
    async fn an_unbudgeted_child_runs_to_completion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        let child = writer(&staged, 1024, 8).spawn().expect("/bin/sh");

        assert_eq!(
            watch_child(&SPEC, child, &staged, None).await,
            Outcome::Completed
        );
    }

    /// A non-zero exit carries the child's own stderr into the gap line.
    #[tokio::test]
    async fn a_failing_child_reports_its_stderr() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("echo 'remote refused' >&2; exit 3");

        let outcome = run(&SPEC, command, &staged, Some(tiny_watch(0, 4096))).await;

        let Outcome::Failed(why) = outcome else {
            panic!("a non-zero exit is a failure, not {outcome:?}");
        };
        assert!(why.contains("remote refused"), "{why}");
        assert!(why.contains("fake clone"), "{why}");
    }

    /// A binary that is not there says so, with the caller's own hint.
    #[tokio::test]
    async fn a_missing_binary_names_itself() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let command = Command::new(tmp.path().join("no-such-binary"));

        let outcome = run(&SPEC, command, &tmp.path().join("staged"), None).await;

        let Outcome::Failed(why) = outcome else {
            panic!("a missing binary is a failure, not {outcome:?}");
        };
        assert!(why.contains("could not be run"), "{why}");
        assert!(why.contains("install it"), "the hint is carried: {why}");
    }
}
