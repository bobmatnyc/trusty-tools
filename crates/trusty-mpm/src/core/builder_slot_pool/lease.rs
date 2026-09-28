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

use std::path::PathBuf;

use super::handover::SERVED_PREFIX;
use super::{SEED_MARKER, STAGING_PREFIX_DOT, SlotPool, SlotPoolError, unique_nanos};

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
    Unreadable(String),
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
        let draft = path.with_file_name(format!(
            "{STAGING_PREFIX_DOT}slot-{index}.lease.draft.{}.{}",
            std::process::id(),
            unique_nanos()
        ));
        std::fs::write(&draft, body)
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
            Ok(body) => parse_lease(&body).map_or_else(
                || LeaseRecord::Unreadable(format!("{} is malformed", path.display())),
                LeaseRecord::Leased,
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                read_served(&self.slot_path(index).join(SEED_MARKER))
            }
            Err(err) => LeaseRecord::Unreadable(format!("{}: {err}", path.display())),
        }
    }
}

/// The holder a pre-#8819 marker names, and when it was named.
fn read_served(marker: &std::path::Path) -> LeaseRecord {
    let unreadable =
        |err: std::io::Error| LeaseRecord::Unreadable(format!("{}: {err}", marker.display()));
    let body = match std::fs::read_to_string(marker) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return LeaseRecord::Absent,
        Err(err) => return unreadable(err),
    };
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
        Err(err) => unreadable(err),
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
    Some(SlotLease {
        holder,
        owner_pid,
        owner_start,
        granted_at,
    })
}

/// Is the slot this record describes still held by a lease the daemon lost?
///
/// Why: a restarted daemon has no delegation for a builder granted before the
/// restart, so the disk is its only evidence. A pid is reused, so the pid's
/// start time must match the one recorded at the grant; and a lease whose
/// owner never exits is still bounded by `ttl_secs`, like the in-memory lease.
/// What: in order — `known_holder` (the daemon has a record for this holder,
/// and its record decides) or an absent record is [`LeaseVerdict::Free`]; an
/// unreadable record is [`LeaseVerdict::Unverifiable`]; a lease at or past
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
        LeaseRecord::Unreadable(why) => return LeaseVerdict::Unverifiable(why.clone()),
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
        }
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
        assert!(matches!(pool.read_lease(0), LeaseRecord::Unreadable(_)));
        std::fs::remove_file(pool.lease_path(0)).expect("rm");
        std::fs::create_dir(pool.lease_path(0)).expect("a directory in its place");
        assert!(matches!(pool.read_lease(0), LeaseRecord::Unreadable(_)));
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
                LeaseRecord::Unreadable("x".into()),
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
