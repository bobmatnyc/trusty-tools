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
//! bounded interval. Once `spent + staged` crosses the ceiling the child's whole
//! PROCESS GROUP is signalled — `SIGTERM`, then `SIGKILL` if it does not exit
//! within the grace. The group, not the child: a clone forks `ssh`,
//! `git-index-pack` and `git-unpack-objects`, and those grandchildren are what
//! hold the sockets and write the bytes, so a signal aimed at the direct child
//! leaves the fetch running against a staged directory the caller has already
//! removed. The caller gets [`Outcome::OverBudget`], which
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
//! the exception — an unopenable root would count 0 bytes forever — and
//! [`super::disk::measure_tree`] is where the two are told apart, for this module and
//! for every other disk figure the crate reports.
//!
//! Test: `super::watchdog::watchdog_tests`.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Mutex, PoisonError};
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

/// How long a clone group gets to wind down after a Ctrl-C.
///
/// Deliberately far shorter than [`TERM_GRACE`]: this one runs while an operator
/// is holding a terminal that has stopped answering them, and five seconds there
/// reads as a hang. `git` and `ssh` unlink their temporary files on the `SIGINT`
/// that arrives first, so the `SIGKILL` this precedes is only for whatever
/// ignored it.
const INTERRUPT_GRACE: Duration = Duration::from_millis(250);

/// The clone process groups this process has taken out of the terminal's reach.
///
/// Why: `process_group(0)` in [`run`] is what lets one signal reach a whole
/// clone tree, and the very same call is what removes that tree from the
/// terminal's FOREGROUND group — so a raw Ctrl-C arrives at `taudit` alone. This
/// list is the only record of what the terminal can no longer signal, and
/// [`stop_clones_on_interrupt`] is what walks it (#5669).
/// What: pids of process-group LEADERS, added when a child is spawned and
/// removed once it has been waited on. The sweep clones one repository at a
/// time, so there is at most one entry in practice — a list rather than a slot
/// because a second caller must not be able to displace the first's group.
/// Test: `super::watchdog::watchdog_tests::an_interrupt_kills_a_detached_clone_group`,
/// `super::watchdog::watchdog_tests::a_detached_group_is_deregistered_when_its_guard_drops`.
static DETACHED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// One entry in [`DETACHED`], removed however [`run`] returns.
///
/// A panic between the spawn and the wait would otherwise leave a pid behind
/// that a later interrupt would signal, and pids are reused.
/// Test: `super::watchdog::watchdog_tests::a_detached_group_is_deregistered_when_its_guard_drops`.
struct Detached(u32);

impl Detached {
    /// Record a child that now leads a process group of its own.
    fn register(pid: u32) -> Self {
        // A poisoned lock must not disarm the one record of what is still
        // running: this guards against orphaned clones, so it recovers the
        // inner value rather than skipping the write.
        DETACHED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(pid);
        Self(pid)
    }
}

impl Drop for Detached {
    fn drop(&mut self) {
        DETACHED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|pid| *pid != self.0);
    }
}

/// Give Ctrl-C back its meaning, then die the way an uncaught one would have.
///
/// Why: [`run`] puts every clone in a process group of its own so that one
/// signal reaches the whole tree, and the price is that the tree is no longer in
/// the terminal's foreground group. This crate installed no signal handling at
/// all, so a raw Ctrl-C killed `taudit` by `SIGINT`'s default disposition — no
/// destructors, `kill_on_drop` never reached — and left `ssh` and
/// `git-index-pack` fetching into a client's disk with nothing watching the
/// ceiling any more (#5669).
/// What: awaits `SIGINT`, passes it on to every group the terminal can no longer
/// reach, kills whatever outlives [`INTERRUPT_GRACE`], and then re-raises
/// `SIGINT` under the default disposition. Awaiting this registers a handler, so
/// `SIGINT` stops killing the process on its own from the first poll onwards;
/// the re-raise is what puts that back. Never returns.
///
/// This function is what installs the `SIGINT` handler, via
/// `tokio::signal::ctrl_c()` — the crate does not register one at load time,
/// only when this is awaited, and only for as long as it stays pending.
/// `tokio::signal::ctrl_c()` fans one OS signal out to every concurrent
/// listener rather than claiming it exclusively, so a host binary that also
/// awaits its own `tokio::signal::ctrl_c()` gets notified alongside this one,
/// not instead of it — both run. A host binary that installs a signal handler
/// by any OTHER means (a raw `sigaction`, the `signal-hook` crate) is
/// untested against this function and may race it for the disposition.
///
/// This belongs to the BINARY, not to [`super::clone_all`]: it ends the process,
/// and a front end embedding this crate owns that decision itself.
/// Test: `super::watchdog::watchdog_tests::an_interrupt_kills_a_detached_clone_group`.
pub async fn stop_clones_on_interrupt() {
    if tokio::signal::ctrl_c().await.is_err() {
        // The handler could not be installed, so `SIGINT` keeps its default
        // disposition and this task has nothing left to contribute. Returning
        // would instead re-raise on a signal that never arrived.
        std::future::pending::<()>().await;
    }
    interrupt_detached_groups(INTERRUPT_GRACE).await;
    die_by_sigint()
}

/// Deliver the interrupt on to every detached clone group.
///
/// Split from [`stop_clones_on_interrupt`] so it can be reasoned about without a
/// real `SIGINT`; the groups it reads and the signalling it delegates are tested
/// separately, because a test that called this would signal every OTHER test's
/// clone in the same process.
/// Test: `super::watchdog::watchdog_tests::an_interrupt_kills_a_detached_clone_group`.
async fn interrupt_detached_groups(grace: Duration) {
    let groups = DETACHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    stop_groups(&groups, grace).await;
}

/// Signal these process groups, killing whatever outlives the grace.
///
/// Test: `super::watchdog::watchdog_tests::an_interrupt_kills_a_detached_clone_group`.
async fn stop_groups(groups: &[u32], grace: Duration) {
    if groups.is_empty() {
        return;
    }
    // #5669: `SIGINT`, because that is exactly what the terminal would have
    // delivered had `process_group(0)` not moved the tree out of its reach.
    for pgid in groups {
        signal_group(*pgid, libc::SIGINT);
    }
    tokio::time::sleep(grace).await;
    // A process may IGNORE `SIGINT` — a shell gives its background jobs
    // `SIG_IGN` for it, and that disposition survives `exec`. The kill is what
    // makes "nothing keeps writing" true rather than merely requested.
    for pgid in groups {
        signal_group(*pgid, libc::SIGKILL);
    }
}

/// Die the way an uncaught Ctrl-C would have.
///
/// Exiting 0 — or on any invented code — tells a shell's `set -e`, a `for` loop
/// over engagements, and a CI runner that the run finished. Restoring the
/// default disposition and re-raising is what makes the parent observe
/// `WIFSIGNALED(SIGINT)` instead, which is where the conventional 130 comes
/// from.
fn die_by_sigint() -> ! {
    // SAFETY: both calls act on this process alone and take no pointer.
    // `SIG_DFL` for `SIGINT` is termination, so the `raise` does not return.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::raise(libc::SIGINT);
    }
    // Reachable only if `SIGINT` is blocked for this thread, which nothing here
    // does. 130 is the shell's own encoding of the same death.
    std::process::exit(130)
}

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
///
/// **The child leads its own process group.** A real clone is a TREE, not one
/// process: `gh` forks `git`, and `git` forks `ssh`, `git-index-pack` and
/// `git-unpack-objects`, every one of which writes into the staged tree.
/// Signalling the direct child alone leaves those grandchildren fetching, and an
/// orphan that keeps writing — into a directory [`super::finish_one`] has by
/// then already removed, and which the orphan recreates — defeats the ceiling
/// that stopped it. `process_group(0)` is what makes one signal reach all of
/// them; [`terminate`] sends it (#5669).
///
/// The cost is that the child no longer shares this process's group, so a
/// terminal `SIGINT` reaches the audit and not the clone. That is why the group
/// is recorded in [`DETACHED`] the moment it is spawned:
/// [`stop_clones_on_interrupt`] is what forwards a Ctrl-C to it, and a binary
/// that does not run that task gets a clone which outlives its own audit.
/// Test: `super::watchdog::watchdog_tests::a_missing_binary_names_itself`,
/// `super::watchdog::watchdog_tests::a_budget_kill_reaches_a_grandchild`,
/// `super::watchdog::watchdog_tests::an_interrupt_kills_a_detached_clone_group`.
pub(super) async fn run(
    spec: &Spawn<'_>,
    mut command: Command,
    staged: &Path,
    watch: Option<Watch>,
) -> Outcome {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // See #5669.
        .process_group(0)
        .kill_on_drop(true);
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
    // #5669: the `process_group(0)` above put this tree beyond the terminal's
    // reach, so record it while it runs — see `stop_clones_on_interrupt`.
    let _detached = child.id().map(Detached::register);
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

/// End the child AND everything it forked, politely and then not.
///
/// `SIGTERM` first because both `git` and `gh` remove their temporary pack files
/// on it, which leaves less for the caller's `remove_dir_all` to walk. `SIGKILL`
/// follows on the grace expiring, and the child is reaped either way — an
/// unreaped child is the orphan this whole path exists to avoid.
///
/// Both signals go to the whole process group, because the direct child is not
/// where the bytes come from: see [`run`] and [`signal_tree`].
/// Test: `super::watchdog::watchdog_tests::a_killed_child_leaves_no_process_behind`,
/// `super::watchdog::watchdog_tests::a_budget_kill_reaches_a_grandchild`.
async fn terminate(child: &mut Child, grace: Duration) {
    if let Some(pid) = child.id() {
        signal_tree(pid, libc::SIGTERM);
        // #5669: `Ok(Err(_))` is a wait that FAILED, not a child that exited.
        // `is_ok()` on the timeout treated the two the same and returned
        // without ever sending the `SIGKILL` this grace exists to precede.
        if let Ok(Ok(_)) = tokio::time::timeout(grace, child.wait()).await {
            return;
        }
        signal_tree(pid, libc::SIGKILL);
    }
    // Reaps, and covers the child whose pid is already gone.
    let _ = child.kill().await;
}

/// Signal the child and every process it forked.
///
/// Why: killing the direct child alone is what let a clone keep running (#5669)
/// — `git-index-pack` and `ssh` are grandchildren, they hold the sockets and do
/// the writing, and nothing reparents them into the grave when their parent
/// dies. [`run`] puts the child in a process group of its own precisely so one
/// signal can reach the lot.
/// What: `kill(-pid)` — the whole group — when the child LEADS its own group,
/// which is what `process_group(0)` arranged. A child that does not lead one is
/// in THIS process's group, where a group signal would kill the audit itself, so
/// it is signalled alone. The check is `getpgid`, asked rather than assumed,
/// because [`watch_child`] is also reached with children this module did not
/// spawn.
/// Test: `super::watchdog::watchdog_tests::a_budget_kill_reaches_a_grandchild`,
/// `super::watchdog::watchdog_tests::a_child_in_our_own_process_group_is_signalled_alone`.
fn signal_tree(pid: u32, signal: libc::c_int) {
    let raw = pid as libc::pid_t;
    // SAFETY: the pid is this process's own child and has not been reaped, so
    // it cannot have been reused; `getpgid` only reads.
    let leads_a_group = unsafe { libc::getpgid(raw) == raw };
    if leads_a_group {
        signal_group(pid, signal);
    } else {
        // SAFETY: a positive pid names that process alone, which is the only
        // safe target for a child sharing THIS process's group.
        unsafe { libc::kill(raw, signal) };
    }
}

/// Signal a process group named by its leader's pid.
///
/// Why: a group OUTLIVES its leader, and [`interrupt_detached_groups`] needs
/// that. Its first signal kills the leading `gh` or `git`, which this process
/// then reaps — after which `getpgid` on that pid answers `ESRCH` and
/// [`signal_tree`] can no longer recognise the group, so the follow-up `SIGKILL`
/// would land on nothing and the grandchildren would keep fetching (#5669).
/// Every pid in [`DETACHED`] is a leader by construction — [`run`] is the only
/// registrar and it always sets `process_group(0)` — so the group can be named
/// directly there rather than probed for.
/// What: `kill(-pgid)`. A group id is not reused while any member survives,
/// which is exactly the case this is called in; an already-empty group answers
/// `ESRCH` and nothing happens.
/// Test: `super::watchdog::watchdog_tests::an_interrupt_kills_a_detached_clone_group`.
fn signal_group(pgid: u32, signal: libc::c_int) {
    // SAFETY: a negative pid names the process GROUP with that id and takes no
    // pointer. `pgid` leads a group in both call paths — checked by `getpgid` in
    // `signal_tree`, guaranteed by `process_group(0)` in the other.
    unsafe { libc::kill(-(pgid as libc::pid_t), signal) };
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
        // #5669: one measurement, so there is no window between a guard and the
        // walk it guards — an explicit `read_dir` check followed by `dir_size`'s
        // own `read_dir` were two separate opens, and a root that became
        // unreadable between them still read as an empty tree.
        // `super::disk::measure_tree` fails on an unreadable ROOT, which would
        // otherwise count 0 forever and never trip the ceiling, and returns a
        // floor for anything below it: `git` creates and removes pack files
        // throughout a fetch, so a partial walk is ordinary, and a floor over
        // the ceiling still trips it.
        Ok(meta) if meta.is_dir() => match super::disk::measure_tree(staged) {
            Ok((bytes, _floor)) => {
                *seen = true;
                Sample::Bytes(bytes)
            }
            Err(why) => Sample::Unmeasurable(why),
        },
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

    /// A child that FORKS, the way every real clone does.
    ///
    /// `gh` forks `git`, `git` forks `ssh` and `git-index-pack`; [`writer`]
    /// forks nothing, so no test built on it can see a grandchild survive the
    /// kill. The grandchild here only sleeps — the claim under test is that the
    /// budget kill REACHES it, and a grandchild that also wrote would be a
    /// runaway of its own on the day the kill regresses. The parent does the
    /// writing that trips the ceiling.
    ///
    /// `sh -c` runs without job control, so the background job stays in the
    /// parent's process group: reaching it is exactly the group-kill property.
    fn forking_writer(dir: &Path, pidfile: &Path, chunk: usize) -> Command {
        let script = r#"mkdir -p "$1" || exit 1
sleep 300 &
printf '%s' "$!" > "$2"
while :; do
  printf '%s' "$3" >> "$1/blob"
done
"#;
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(script)
            .arg("watchdog-forking-fixture")
            .arg(dir)
            .arg(pidfile)
            .arg("x".repeat(chunk))
            .kill_on_drop(true);
        command
    }

    /// Wait out the window between a signal and the reaping that follows it.
    ///
    /// A grandchild's parent dies first, so the grandchild is reparented and
    /// reaped by `init` rather than by anything here — `kill(pid, 0)` answers
    /// "alive" for the moment it spends as a zombie. Polling a bounded deadline
    /// keeps that from reading as a survival.
    async fn died_within(pid: u32, deadline: Duration) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < deadline {
            if !alive(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        !alive(pid)
    }

    /// The pid [`forking_writer`] recorded, once it has finished recording it.
    ///
    /// The fixture writes the pid while the test polls for it, so a single read
    /// could catch a half-written number and name an unrelated process. Two
    /// identical reads of a pid that is actually alive is what settles it.
    async fn grandchild_pid(pidfile: &Path) -> u32 {
        let started = std::time::Instant::now();
        let mut last = None;
        while started.elapsed() < STOP_DEADLINE {
            if let Ok(text) = std::fs::read_to_string(pidfile)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                if last == Some(pid) && alive(pid) {
                    return pid;
                }
                last = Some(pid);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the fixture recorded no live grandchild within {STOP_DEADLINE:?}");
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

    /// A snapshot of [`DETACHED`], so an assertion never holds its lock.
    fn registered() -> Vec<u32> {
        DETACHED.lock().expect("an unpoisoned lock").clone()
    }

    /// The process group this pid belongs to.
    fn group_of(pid: u32) -> u32 {
        // SAFETY: `getpgid` only reads, and the pid names a live process the
        // caller has just observed.
        let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
        assert!(pgid > 0, "pid {pid} has no process group");
        pgid as u32
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

    /// The kill reaches the whole clone, not just the process this module
    /// spawned (#5669).
    ///
    /// A real clone's bytes come from grandchildren — `ssh`, `git-index-pack`,
    /// `git-unpack-objects` — and one that outlives the budget kill keeps
    /// fetching into, and RECREATES, the staged directory `finish_one` has by
    /// then removed. Every other kill test here uses a fixture that forks
    /// nothing, so this is the only one that can see it.
    ///
    /// Goes through [`run`], not [`watch_child`], because `process_group(0)` is
    /// applied there: this asserts the production spawn path, not a property a
    /// test fixture arranged for itself.
    ///
    /// A surviving grandchild fails this test in either of two ways, and both
    /// are the same defect. It inherits the stderr pipe, so it holds that pipe
    /// open after its parent dies and `watch_child`'s drain never finishes —
    /// that is the deadline arm. When it does not hold the pipe, the pid check
    /// at the end is what catches it.
    #[tokio::test]
    async fn a_budget_kill_reaches_a_grandchild() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        let pidfile = tmp.path().join("grandchild.pid");

        let outcome = tokio::time::timeout(
            STOP_DEADLINE,
            run(
                &SPEC,
                forking_writer(&staged, &pidfile, 1024),
                &staged,
                Some(tiny_watch(0, 4096)),
            ),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "no verdict within {STOP_DEADLINE:?} — a grandchild that survived the kill is \
                 still holding the inherited stderr pipe open"
            )
        });

        assert!(matches!(outcome, Outcome::OverBudget { .. }), "{outcome:?}");
        let pid: u32 = std::fs::read_to_string(&pidfile)
            .expect("the fixture recorded its grandchild's pid")
            .trim()
            .parse()
            .expect("a pid");
        assert!(
            died_within(pid, Duration::from_secs(5)).await,
            "grandchild {pid} outlived the budget kill and would keep writing into the tree \
             the caller is about to remove"
        );
    }

    /// A child this module did not put in its own group is signalled ALONE.
    ///
    /// The group signal is `kill(-pid)`, and `-pid` for a child sharing our
    /// group would name some other group entirely — or, if this process led it,
    /// the audit itself. [`signal_tree`] asks `getpgid` rather than assuming,
    /// and this is the arm that answers no: the child is spawned without
    /// `process_group`, so it inherits ours.
    #[tokio::test]
    async fn a_child_in_our_own_process_group_is_signalled_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut child = writer(&tmp.path().join("staged"), 1024, u64::from(u32::MAX))
            .spawn()
            .expect("/bin/sh");
        let pid = child.id().expect("a freshly spawned child has a pid");
        assert_ne!(
            unsafe { libc::getpgid(pid as libc::pid_t) },
            pid as libc::pid_t,
            "the fixture must NOT lead its own group for this to be the case under test"
        );

        terminate(&mut child, Duration::from_millis(200)).await;

        // Reaching this line at all is half the assertion: a group signal aimed
        // at our own group would have killed the test process.
        assert!(
            matches!(child.try_wait(), Ok(Some(_))),
            "a child sharing our group is still ended and reaped"
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

    /// A Ctrl-C must reach the clone the terminal can no longer see (#5669).
    ///
    /// `process_group(0)` is what took the tree out of the foreground group, so
    /// a raw `SIGINT` killed `taudit` on the default disposition and left `ssh`
    /// and `git-index-pack` writing into a client's disk with the watchdog dead.
    /// The budget here is `u64::MAX`, which takes the watchdog itself out of the
    /// picture: the only thing that can stop this fixture is the interrupt path.
    #[tokio::test]
    async fn an_interrupt_kills_a_detached_clone_group() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let staged = tmp.path().join("staged");
        let pidfile = tmp.path().join("grandchild.pid");

        let clone = tokio::spawn({
            let (staged, pidfile) = (staged.clone(), pidfile.clone());
            async move {
                // No ceiling this fixture can reach, so nothing but the
                // interrupt can end it.
                let unreachable_ceiling = Watch {
                    budget_bytes: u64::MAX,
                    ..tiny_watch(0, u64::MAX)
                };
                run(
                    &SPEC,
                    forking_writer(&staged, &pidfile, 1024),
                    &staged,
                    Some(unreachable_ceiling),
                )
                .await
            }
        });

        let pid = grandchild_pid(&pidfile).await;
        let group = group_of(pid);

        // Half the finding: `run` must have RECORDED this group, or an interrupt
        // has nothing to walk. `interrupt_detached_groups` is not called here —
        // it signals every registered group, and the other tests in this binary
        // clone too.
        assert!(
            registered().contains(&group),
            "run must register the group it detached; {group} is not in {:?}",
            registered()
        );
        // The other half: signalling that group ends the whole tree.
        stop_groups(&[group], Duration::from_millis(50)).await;

        assert!(
            died_within(pid, Duration::from_secs(5)).await,
            "grandchild {pid} outlived the interrupt and would keep fetching into a tree \
             nothing is watching any more"
        );
        // Reap the fixture's own shell, which the same signal stopped.
        let _ = tokio::time::timeout(STOP_DEADLINE, clone).await;
    }

    /// A guard's entry does not outlive the guard (#5669).
    ///
    /// A pid left in [`DETACHED`] is a pid a later interrupt signals, and the
    /// kernel reuses pids. Asserted on a sentinel rather than on a real clone
    /// because [`DETACHED`] is process-global: a sibling test registering its
    /// own clone must not be able to decide this one.
    #[test]
    fn a_detached_group_is_deregistered_when_its_guard_drops() {
        // Above every pid this kernel hands out, so it names nothing.
        const SENTINEL: u32 = 0x7fff_fffe;

        {
            let _guard = Detached::register(SENTINEL);
            assert!(
                registered().contains(&SENTINEL),
                "registered while the guard is held"
            );
        }

        assert!(
            !registered().contains(&SENTINEL),
            "the entry is gone once the guard drops"
        );
    }
}
