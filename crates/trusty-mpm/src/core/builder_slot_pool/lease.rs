//! Builder-slot leases that outlive the daemon that granted them (#8819).
//!
//! Why: the daemon's lease on a slot is its in-memory delegation record, and a
//! restart drops every one. The builders keep running — they are harness
//! subagents, not daemon children — so the restarted daemon offered their slots
//! to new dispatches and handed them over under live builds. A lease file
//! beside each handed-over slot is what a restarted daemon reads instead.
//! What: [`SlotPool::record_lease`] writes `<parent>/.slot-<n>.lease` at each
//! handover; [`SlotPool::read_lease`] reads it back, or the `served:` line of a
//! pre-#8819 marker; [`judge_lease`] decides from that record whether the slot
//! is still held. A pid is never the authority on its own: a lease holds only
//! while its owner's pid runs with the start time recorded at the grant, and
//! never past the lease TTL. Evidence that cannot be read or verified reads as
//! held, never free (ADR-0045).
//! Test: the `#[cfg(test)]` suite below, and `builder_slot_lease_tests`.

use std::path::{Path, PathBuf};

use trusty_common::github_path::GithubPath;

use super::handover::{LEASED_LINE, SERVED_PREFIX};
use super::{SEED_MARKER, STAGING_PREFIX_DOT, SlotPool, SlotPoolError, unique_nanos};
use crate::core::session::SessionId;

/// How far a live owner's start time may sit from the one the lease recorded.
///
/// Why: both sides are whole seconds read at different moments, so exact
/// equality would call a live owner "reused" — same tolerance as #7771's.
const START_TOLERANCE_SECS: i64 = 2;

/// One slot's lease, as written at the handover.
///
/// What: the holder's `tool_use_id`, the dispatching session's pid and that
/// process's start time (Unix seconds, `None` when unknown), and when the
/// slot was granted (Unix seconds).
/// Test: `a_lease_round_trips_through_its_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotLease {
    /// The `tool_use_id` the slot was handed to.
    pub holder: String,
    /// The pid of the session that dispatched the holder.
    pub owner_pid: Option<u32>,
    /// That process's start time, which a reused pid does not share.
    pub owner_start: Option<i64>,
    /// When the slot was handed to `holder`.
    pub granted_at: i64,
    /// The session that dispatched `holder`, so a restored lease can name it.
    pub session: Option<SessionId>,
}

/// What the disk says about a slot's holder, before it is judged.
///
/// Test: `a_lease_round_trips_through_its_file`,
/// `an_unreadable_lease_is_unreadable_not_absent`,
/// `a_pre_8819_served_marker_is_read_as_served`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseRecord {
    /// No lease file and no holder on the marker.
    Absent,
    /// A lease file this module wrote.
    Leased(SlotLease),
    /// No lease file, but a pre-#8819 handover named `holder` at `served_at`.
    Served {
        /// The holder the marker names.
        holder: String,
        /// The marker's modification time, in Unix seconds.
        served_at: i64,
    },
    /// The evidence exists and could not be read or parsed.
    Unreadable {
        /// What could not be read, naming the file.
        why: String,
        /// The file's modification time, in Unix seconds; `None` when even
        /// its metadata cannot be read.
        since: Option<i64>,
    },
}

/// Whether a slot is held by a lease the daemon has no record of.
///
/// Test: `judge_lease_covers_every_arm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseVerdict {
    /// No live lease: the slot may be handed over.
    Free,
    /// The lease's owner still runs with its recorded start time.
    Held(String),
    /// The evidence could not be read or verified, so the slot is not free.
    Unverifiable(String),
}

impl LeaseVerdict {
    /// Does this verdict keep the slot from being handed out?
    #[must_use]
    pub fn blocks(&self) -> bool {
        !matches!(self, Self::Free)
    }
}

/// What the process table says about a lease's owner pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerProbe {
    /// No process has that pid.
    Gone,
    /// A process has that pid; its start time, or why it could not be read.
    Running(Result<i64, String>),
}

impl SlotPool {
    /// The pool root every repo's pool shares, for [`slots_under`] (#8819).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where slot `index`'s lease lives: `<parent>/.slot-<n>.lease`.
    ///
    /// Why: beside the slot, like the seed-failure record, so it never reads
    /// as a slot index and no seed or handover inside the slot touches it.
    #[must_use]
    pub fn lease_path(&self, index: u32) -> PathBuf {
        let slot = self.slot_path(index);
        slot.with_file_name(format!("{STAGING_PREFIX_DOT}slot-{index}.lease"))
    }

    /// Record `lease` for slot `index`, atomically (#8819).
    ///
    /// # Errors
    ///
    /// [`SlotPoolError::Create`] when the lease cannot be written. The caller
    /// must not hand the slot out: a lease that is not on disk is lost at the
    /// next restart.
    ///
    /// Test: `a_lease_round_trips_through_its_file`.
    pub fn record_lease(&self, index: u32, lease: &SlotLease) -> Result<(), SlotPoolError> {
        let path = self.lease_path(index);
        let mut body = format!(
            "#8819 builder slot lease\nholder: {}\ngranted_at: {}\n",
            lease.holder, lease.granted_at
        );
        if let Some(pid) = lease.owner_pid {
            body.push_str(&format!("owner_pid: {pid}\n"));
        }
        if let Some(start) = lease.owner_start {
            body.push_str(&format!("owner_start: {start}\n"));
        }
        if let Some(session) = lease.session {
            body.push_str(&format!("session: {}\n", session.0));
        }
        let draft = path.with_file_name(format!(
            "{STAGING_PREFIX_DOT}slot-{index}.lease.draft.{}.{}",
            std::process::id(),
            unique_nanos()
        ));
        // #8819 critic: synced before the rename, so a crash cannot publish an
        // empty lease that reads as unverifiable for a whole TTL.
        let written = std::fs::File::create(&draft).and_then(|mut file| {
            std::io::Write::write_all(&mut file, body.as_bytes())?;
            file.sync_all()
        });
        written
            .and_then(|()| std::fs::rename(&draft, &path))
            .map_err(|source| {
                drop(std::fs::remove_file(&draft));
                SlotPoolError::Create { path, source }
            })
    }

    /// Remove slot `index`'s lease. Best effort: a leftover lease names a
    /// holder the granting daemon knows, so it reads as free until a restart.
    pub fn clear_lease(&self, index: u32) {
        drop(std::fs::remove_file(self.lease_path(index)));
    }
}

/// Remove the lease beside `slot_dir` when it still names `holder` (#8819).
///
/// Why: a finished builder's lease otherwise reads as held after a restart
/// until the TTL, since its dispatching session outlives it. Only `holder`'s
/// own lease goes, so a newer holder's lease is never deleted.
/// What: a missing lease is a no-op; one naming another holder is left; a
/// lease that cannot be read or removed is logged and left, which costs at
/// most one TTL.
/// Test: `a_completed_builder_frees_its_slot_across_a_restart_8819`.
pub fn clear_lease_of(slot_dir: &Path, holder: &str) {
    let Some(path) = lease_path_of(slot_dir) else {
        return;
    };
    match std::fs::read_to_string(&path) {
        Ok(body) if parse_lease(&body).is_some_and(|lease| lease.holder == holder) => {
            if let Err(err) = std::fs::remove_file(&path) {
                tracing::warn!(lease = %path.display(), "could not remove a finished builder's lease: {err}");
            }
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(lease = %path.display(), "could not read a finished builder's lease: {err}");
        }
    }
}

/// `<parent>/.slot-<n>.lease` for the slot directory `<parent>/slot-<n>`.
fn lease_path_of(slot_dir: &Path) -> Option<PathBuf> {
    let name = slot_dir.file_name()?.to_str()?;
    Some(slot_dir.with_file_name(format!("{STAGING_PREFIX_DOT}{name}.lease")))
}

impl SlotPool {
    /// Read slot `index`'s lease, or the holder a pre-#8819 marker names.
    ///
    /// What: any read failure other than a missing file is
    /// [`LeaseRecord::Unreadable`], never [`LeaseRecord::Absent`].
    /// Test: `a_lease_round_trips_through_its_file`,
    /// `an_unreadable_lease_is_unreadable_not_absent`,
    /// `a_pre_8819_served_marker_is_read_as_served`.
    #[must_use]
    pub fn read_lease(&self, index: u32) -> LeaseRecord {
        let path = self.lease_path(index);
        match std::fs::read_to_string(&path) {
            Ok(body) => parse_lease(&body)
                .map_or_else(|| unreadable(&path, "is malformed"), LeaseRecord::Leased),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                read_served(&self.slot_path(index).join(SEED_MARKER))
            }
            Err(err) => unreadable(&path, &err.to_string()),
        }
    }
}

/// [`LeaseRecord::Unreadable`] for `path`, stamped with its mtime when that
/// can be read (#8819 critic: the TTL bounds it).
fn unreadable(path: &Path, why: &str) -> LeaseRecord {
    let since = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).timestamp());
    LeaseRecord::Unreadable {
        why: format!("{}: {why}", path.display()),
        since,
    }
}

/// The holder a pre-#8819 marker names, and when it was named.
fn read_served(marker: &std::path::Path) -> LeaseRecord {
    let fail = |err: std::io::Error| unreadable(marker, &err.to_string());
    let body = match std::fs::read_to_string(marker) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return LeaseRecord::Absent,
        Err(err) => return fail(err),
    };
    // #8819 critic: a post-#8819 handover's lease file was the authority; with
    // it gone, the holder finished or was never handed the slot.
    if body.lines().any(|line| line == LEASED_LINE) {
        return LeaseRecord::Absent;
    }
    let Some(holder) = body
        .lines()
        .find_map(|line| line.strip_prefix(SERVED_PREFIX))
    else {
        return LeaseRecord::Absent;
    };
    let holder = holder.split_once(" from ").map_or(holder, |(h, _)| h);
    match std::fs::metadata(marker).and_then(|m| m.modified()) {
        Ok(modified) => LeaseRecord::Served {
            holder: holder.to_string(),
            served_at: chrono::DateTime::<chrono::Utc>::from(modified).timestamp(),
        },
        Err(err) => fail(err),
    }
}

/// Parse a lease body; `None` when a required field is missing or malformed.
fn parse_lease(body: &str) -> Option<SlotLease> {
    let field = |key: &str| {
        body.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(": "))
    };
    let holder = field("holder").filter(|h| !h.is_empty())?.to_string();
    let granted_at = field("granted_at")?.parse().ok()?;
    let owner_pid = field("owner_pid").map(str::parse).transpose().ok()?;
    let owner_start = field("owner_start").map(str::parse).transpose().ok()?;
    let session = field("session")
        .map(uuid::Uuid::parse_str)
        .transpose()
        .ok()?
        .map(SessionId);
    Some(SlotLease {
        holder,
        owner_pid,
        owner_start,
        granted_at,
        session,
    })
}

/// Every slot directory in every repo's pool under `root` (#8819).
///
/// Why: the builder cap is machine-wide, so a restarted daemon must count the
/// leases in every repo's pool, not only the pool of the dispatch asking.
/// What: `<root>/<owner>/<repo>/slot-<n>` as `(pool, n)`. A directory that
/// exists and cannot be listed is an `Err` naming it, so the caller can count
/// it as unverifiable rather than as empty; a missing `root` is no slots.
/// Test: `slots_under_lists_every_repos_slots`.
#[must_use]
pub fn slots_under(root: &Path) -> Vec<Result<(SlotPool, u32), String>> {
    // #8819 critic: an entry that cannot be read fails the whole listing,
    // the same unverifiable answer as a `read_dir` failure, never a skip.
    let list = |dir: &Path| -> Result<Vec<(String, PathBuf)>, String> {
        let fail = |err: std::io::Error| format!("{}: {err}", dir.display());
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(fail(err)),
        };
        let mut dirs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(fail)?;
            let is_dir = entry.file_type().map_err(fail)?.is_dir();
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if is_dir && !name.starts_with('.') {
                dirs.push((name, entry.path()));
            }
        }
        Ok(dirs)
    };
    let mut out = Vec::new();
    let owners = match list(root) {
        Ok(owners) => owners,
        Err(err) => return vec![Err(err)],
    };
    for (owner, owner_dir) in owners {
        let repos = match list(&owner_dir) {
            Ok(repos) => repos,
            Err(err) => {
                out.push(Err(err));
                continue;
            }
        };
        for (repo, repo_dir) in repos {
            let identity = GithubPath {
                owner: owner.clone(),
                repo,
            };
            let pool = SlotPool::new(root.to_path_buf(), identity, 1);
            match list(&repo_dir) {
                Ok(slots) => out.extend(slots.iter().filter_map(|(name, _)| {
                    let index = name.strip_prefix("slot-")?.parse().ok()?;
                    Some(Ok((pool.clone(), index)))
                })),
                Err(err) => out.push(Err(err)),
            }
        }
    }
    out
}

/// Is the slot this record describes still held by a lease the daemon lost?
///
/// Why: a restarted daemon has no delegation for a builder granted before the
/// restart, so the disk is its only evidence. A pid is reused, so the pid's
/// start time must match the one recorded at the grant; and a lease whose
/// owner never exits is still bounded by `ttl_secs`, like the in-memory lease.
/// What: in order — `known_holder` (the daemon has a record for this holder,
/// and its record decides) or an absent record is [`LeaseVerdict::Free`]; an
/// unreadable record is [`LeaseVerdict::Unverifiable`] until its mtime is `ttl_secs` old, free after, and unverifiable for good when it has no mtime; a lease at or past
/// `ttl_secs` is free; a pre-#8819 `served:` marker inside the TTL is
/// unverifiable, since it records no owner. For a lease file: no owner pid is
/// unverifiable; a gone pid is free; a running pid whose start time cannot be
/// read, or was not recorded, is unverifiable; a start time within
/// [`START_TOLERANCE_SECS`] is [`LeaseVerdict::Held`]; any other is a reused
/// pid, free.
/// Test: `judge_lease_covers_every_arm`.
#[must_use]
pub fn judge_lease(
    record: &LeaseRecord,
    now: i64,
    ttl_secs: i64,
    known_holder: &dyn Fn(&str) -> bool,
    owner: &dyn Fn(u32) -> OwnerProbe,
) -> LeaseVerdict {
    let (holder, since) = match record {
        LeaseRecord::Absent => return LeaseVerdict::Free,
        // #8819 critic: bounded by the file's own mtime, so an unreadable lease
        // cannot hold its slot forever; no mtime at all stays unverifiable.
        LeaseRecord::Unreadable { since: Some(t), .. } if now - t >= ttl_secs => {
            return LeaseVerdict::Free;
        }
        LeaseRecord::Unreadable { why, .. } => return LeaseVerdict::Unverifiable(why.clone()),
        LeaseRecord::Leased(lease) => (lease.holder.as_str(), lease.granted_at),
        LeaseRecord::Served { holder, served_at } => (holder.as_str(), *served_at),
    };
    if known_holder(holder) || now - since >= ttl_secs {
        return LeaseVerdict::Free;
    }
    let LeaseRecord::Leased(lease) = record else {
        return LeaseVerdict::Unverifiable(format!(
            "a pre-#8819 handover to {holder} records no owner to verify"
        ));
    };
    let Some(pid) = lease.owner_pid else {
        return LeaseVerdict::Unverifiable(format!("the lease of {holder} records no owner pid"));
    };
    match (owner(pid), lease.owner_start) {
        (OwnerProbe::Gone, _) => LeaseVerdict::Free,
        (OwnerProbe::Running(Err(err)), _) => LeaseVerdict::Unverifiable(format!(
            "owner pid {pid} of {holder} runs and its start time could not be read: {err}"
        )),
        (OwnerProbe::Running(Ok(_)), None) => LeaseVerdict::Unverifiable(format!(
            "owner pid {pid} of {holder} runs and the lease records no start time"
        )),
        (OwnerProbe::Running(Ok(actual)), Some(recorded))
            if (actual - recorded).abs() <= START_TOLERANCE_SECS =>
        {
            LeaseVerdict::Held(format!(
                "owner pid {pid} of {holder} runs with the start time the lease recorded"
            ))
        }
        (OwnerProbe::Running(Ok(_)), Some(_)) => LeaseVerdict::Free,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::github_path::GithubPath;

    fn pool(root: &std::path::Path) -> SlotPool {
        let identity = GithubPath {
            owner: "acme".to_string(),
            repo: "widgets".to_string(),
        };
        SlotPool::new(root.join("pool"), identity, 4)
    }

    fn lease(pid: Option<u32>, start: Option<i64>, granted_at: i64) -> SlotLease {
        SlotLease {
            holder: "toolu_A".to_string(),
            owner_pid: pid,
            owner_start: start,
            granted_at,
            session: Some(SessionId::new()),
        }
    }

    /// #8819: every repo's slots are listed, and nothing that is not a slot.
    #[test]
    fn slots_under_lists_every_repos_slots() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool(root.path());
        pool.seed(0, None).expect("a seeded slot 0");
        pool.record_lease(0, &lease(Some(1), Some(1), 1))
            .expect("lease");
        let other = SlotPool::new(
            root.path().join("pool"),
            GithubPath {
                owner: "acme".to_string(),
                repo: "gadgets".to_string(),
            },
            4,
        );
        other.seed(3, None).expect("a seeded slot 3");
        let mut found: Vec<PathBuf> = slots_under(&root.path().join("pool"))
            .into_iter()
            .map(|slot| slot.map(|(p, i)| p.slot_path(i)).expect("listable"))
            .collect();
        found.sort();
        assert_eq!(found, vec![other.slot_path(3), pool.slot_path(0)]);
        assert!(slots_under(&root.path().join("absent")).is_empty());
    }

    #[test]
    fn a_lease_round_trips_through_its_file() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool(root.path());
        pool.seed(0, None).expect("a seeded slot 0");
        let written = lease(Some(42), Some(1_000), 2_000);
        pool.record_lease(0, &written).expect("lease written");
        assert_eq!(pool.read_lease(0), LeaseRecord::Leased(written));
        assert_eq!(pool.existing_indexes(), vec![0], "a lease is not a slot");
        pool.clear_lease(0);
        assert_eq!(pool.read_lease(0), LeaseRecord::Absent);
    }

    /// #8819 fail-closed: a lease that exists and cannot be read is never
    /// read as no lease at all.
    #[test]
    fn an_unreadable_lease_is_unreadable_not_absent() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool(root.path());
        pool.seed(0, None).expect("a seeded slot 0");
        std::fs::write(pool.lease_path(0), "holder: toolu_A\n").expect("partial lease");
        assert!(matches!(pool.read_lease(0), LeaseRecord::Unreadable { .. }));
        std::fs::remove_file(pool.lease_path(0)).expect("rm");
        std::fs::create_dir(pool.lease_path(0)).expect("a directory in its place");
        assert!(matches!(pool.read_lease(0), LeaseRecord::Unreadable { .. }));
    }

    #[test]
    fn a_pre_8819_served_marker_is_read_as_served() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool(root.path());
        pool.seed(0, None).expect("a seeded slot 0");
        assert_eq!(pool.read_lease(0), LeaseRecord::Absent, "never handed over");
        let marker = pool.slot_path(0).join(SEED_MARKER);
        let body = std::fs::read_to_string(&marker).expect("marker");
        std::fs::write(&marker, format!("{body}served: toolu_A from /co\n")).expect("served");
        let LeaseRecord::Served { holder, served_at } = pool.read_lease(0) else {
            panic!("expected Served, got {:?}", pool.read_lease(0));
        };
        assert_eq!(holder, "toolu_A");
        assert!((chrono::Utc::now().timestamp() - served_at).abs() < 60);
    }

    /// #8819 critic: only the finished holder's own lease is removed.
    #[test]
    fn clear_lease_of_removes_only_the_named_holders_lease() {
        let root = tempfile::tempdir().expect("tempdir");
        let pool = pool(root.path());
        pool.seed(0, None).expect("a seeded slot 0");
        pool.record_lease(0, &lease(Some(1), Some(1), 1))
            .expect("lease");
        clear_lease_of(&pool.slot_path(0), "toolu_NEWER");
        assert!(pool.lease_path(0).is_file(), "another holder's lease stays");
        clear_lease_of(&pool.slot_path(0), "toolu_A");
        assert_eq!(pool.read_lease(0), LeaseRecord::Absent);
    }

    fn unknown(_: &str) -> bool {
        false
    }

    fn known(holder: &str) -> bool {
        holder == "toolu_A"
    }

    fn gone(_: u32) -> OwnerProbe {
        OwnerProbe::Gone
    }

    fn started_500(_: u32) -> OwnerProbe {
        OwnerProbe::Running(Ok(500))
    }

    fn started_900(_: u32) -> OwnerProbe {
        OwnerProbe::Running(Ok(900))
    }

    fn start_unreadable(_: u32) -> OwnerProbe {
        OwnerProbe::Running(Err("no entry".to_string()))
    }

    /// Judge `record` at t=1050 against a 100 s TTL.
    fn judge(
        record: &LeaseRecord,
        holder_known: fn(&str) -> bool,
        owner: fn(u32) -> OwnerProbe,
    ) -> LeaseVerdict {
        judge_lease(record, 1_050, 100, &holder_known, &owner)
    }

    /// #8819 items 2 and 4: a lease holds only on a live owner whose start time
    /// matches; a dead or reused pid, a known holder, or an expired lease frees
    /// the slot; anything unverifiable keeps it.
    #[test]
    fn judge_lease_covers_every_arm() {
        let leased = |pid, start| LeaseRecord::Leased(lease(pid, start, 1_000));
        let live = leased(Some(7), Some(501));
        assert!(matches!(
            judge(&live, unknown, started_500),
            LeaseVerdict::Held(_)
        ));
        assert_eq!(
            judge(&live, unknown, started_900),
            LeaseVerdict::Free,
            "reused pid"
        );
        assert_eq!(
            judge(&live, unknown, gone),
            LeaseVerdict::Free,
            "dead owner"
        );
        assert_eq!(
            judge(&live, known, started_500),
            LeaseVerdict::Free,
            "record decides"
        );
        assert_eq!(
            judge_lease(&live, 1_100, 100, &unknown, &started_500),
            LeaseVerdict::Free,
            "a lease never outlives its TTL"
        );
        for (record, owner, why) in [
            (
                live.clone(),
                start_unreadable as fn(u32) -> OwnerProbe,
                "start unreadable",
            ),
            (leased(Some(7), None), started_500, "no recorded start"),
            (leased(None, None), started_500, "no owner pid"),
            (
                LeaseRecord::Unreadable {
                    why: "x".into(),
                    since: None,
                },
                started_500,
                "unreadable",
            ),
        ] {
            assert!(
                matches!(
                    judge(&record, unknown, owner),
                    LeaseVerdict::Unverifiable(_)
                ),
                "{why}"
            );
        }
        // #8819 critic: an unreadable lease is bounded by its own mtime.
        let unreadable_at = |since| LeaseRecord::Unreadable {
            why: "x".into(),
            since: Some(since),
        };
        assert!(
            judge(&unreadable_at(1_000), unknown, gone).blocks(),
            "inside the TTL"
        );
        assert_eq!(
            judge(&unreadable_at(900), unknown, gone),
            LeaseVerdict::Free,
            "past it"
        );
        let served = LeaseRecord::Served {
            holder: "toolu_A".to_string(),
            served_at: 1_000,
        };
        assert!(judge(&served, unknown, gone).blocks(), "legacy marker");
        assert!(!judge(&served, known, gone).blocks());
        assert_eq!(
            judge(&LeaseRecord::Absent, unknown, gone),
            LeaseVerdict::Free
        );
    }
}
