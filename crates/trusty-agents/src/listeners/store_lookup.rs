//! Exact persisted event lookup for withdrawal authorization (#4283).
use super::{EventStore, StoredEvent, events_dir, is_included};
use anyhow::{Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::BufRead,
    path::Path,
};

const MAX_EVENT_BYTES: u64 = 1024 * 1024;
const MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;
impl EventStore {
    /// Why: absence from a bounded processing batch is not evidence of upstream deletion.
    /// What: stream the log and retain only requested IDs, using the newest record and current filters.
    /// Retains at most 16 MiB across 10000 IDs. Missing IDs stay unknown;
    /// oversized/malformed records fail without authorizing withdrawal.
    /// Test: `published_event_survives_unrelated_event_batch_rollover`, `exact_lookup_bounds_and_latest_filter_are_enforced`.
    pub(crate) async fn read_exact(
        ids: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, StoredEvent>> {
        Self::read_exact_at(&events_dir()?, ids).await
    }
    pub(crate) async fn read_exact_at(
        dir: &Path,
        ids: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, StoredEvent>> {
        anyhow::ensure!(ids.len() <= 10000, "Exact event lookup exceeds 10000 IDs");
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let path = dir.join("events.jsonl");
        let filters = Self::load_filters_at(dir).await?;
        let ids = ids.clone();
        tokio::task::spawn_blocking(move || scan(&path, &ids, &filters)).await?
    }
}
fn scan(
    path: &Path,
    ids: &BTreeSet<String>,
    filters: &HashMap<String, bool>,
) -> Result<BTreeMap<String, StoredEvent>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error).context("Open persisted event log"),
    };
    let mut reader = std::io::BufReader::new(file);
    let mut found = BTreeMap::new();
    let mut sizes = BTreeMap::new();
    let mut retained = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        let mut limited = std::io::Read::take(&mut reader, MAX_EVENT_BYTES + 1);
        if limited.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        anyhow::ensure!(
            line.len() as u64 <= MAX_EVENT_BYTES,
            "Stored event exceeds 1 MiB lookup limit"
        );
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let mut event: StoredEvent =
            serde_json::from_slice(&line).context("Invalid persisted event record")?;
        if ids.contains(&event.id) {
            retained = retained
                .saturating_sub(sizes.insert(event.id.clone(), line.len()).unwrap_or(0))
                .saturating_add(line.len());
            anyhow::ensure!(
                retained <= MAX_RETAINED_BYTES,
                "Requested events exceed 16 MiB retained lookup limit"
            );
            event.included = is_included(filters, &event.event_type);
            found.insert(event.id.clone(), event);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn exact_lookup_bounds_and_latest_filter_are_enforced() {
        let tmp = tempfile::tempdir().unwrap();
        let mut event = StoredEvent {
            id: "mail:one".into(),
            listener_id: "mail".into(),
            provider: "gmail".into(),
            event_type: "message.received".into(),
            ts: "2026-09-01T00:00:00Z".into(),
            from: None,
            subject: None,
            snippet: Some("old".into()),
            included: true,
            labels: vec![],
        };
        EventStore::append_at(tmp.path(), &event).await.unwrap();
        event.snippet = Some("new".into());
        EventStore::append_at(tmp.path(), &event).await.unwrap();
        EventStore::set_filter_at(tmp.path(), &event.event_type, false)
            .await
            .unwrap();
        let ids = BTreeSet::from([event.id.clone(), "unknown".into()]);
        let records = EventStore::read_exact_at(tmp.path(), &ids).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[&event.id].snippet.as_deref(), Some("new"));
        assert!(!records[&event.id].included);
        std::fs::write(
            tmp.path().join("events.jsonl"),
            vec![b'x'; MAX_EVENT_BYTES as usize + 1],
        )
        .unwrap();
        assert!(
            EventStore::read_exact_at(tmp.path(), &ids)
                .await
                .unwrap_err()
                .to_string()
                .contains("1 MiB")
        );
        std::fs::write(tmp.path().join("events.jsonl"), "").unwrap();
        let mut retained_ids = BTreeSet::new();
        event.snippet = Some("x".repeat(MAX_EVENT_BYTES as usize / 2));
        for n in 0..33 {
            event.id = n.to_string();
            retained_ids.insert(event.id.clone());
            EventStore::append_at(tmp.path(), &event).await.unwrap();
        }
        assert!(
            EventStore::read_exact_at(tmp.path(), &retained_ids)
                .await
                .unwrap_err()
                .to_string()
                .contains("16 MiB")
        );
        let too_many = (0..10001).map(|n| n.to_string()).collect();
        assert!(
            EventStore::read_exact_at(tmp.path(), &too_many)
                .await
                .is_err()
        );
    }
}
