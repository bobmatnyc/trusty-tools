//! Pure calendar-window planning, with bounded identities and no upstream execution.
use super::*;
use chrono::{DateTime, Months, Utc};
use sha2::{Digest, Sha256};

pub(super) const MAX_JOBS: usize = 10_000;
pub(super) fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
pub(super) fn boundary(anchor: DateTime<Utc>, offset: i32) -> Result<DateTime<Utc>> {
    let date = if offset < 0 {
        anchor.checked_sub_months(Months::new(offset.unsigned_abs()))
    } else {
        anchor.checked_add_months(Months::new(offset as u32))
    };
    date.ok_or_else(|| {
        KnowledgeError::BadRequest("Calendar interval exceeds supported dates".into())
    })
}
pub(super) fn windows(
    state: &KnowledgeState,
    now: DateTime<Utc>,
    open: bool,
) -> Result<Vec<Window>> {
    if now < state.anchor_at {
        return Err(KnowledgeError::BadRequest(
            "Clock precedes initialization".into(),
        ));
    }
    let mut out = Vec::new();
    for offset in -(state.history_months as i32)..1200 {
        let start = boundary(state.anchor_at, offset)?;
        let end = boundary(state.anchor_at, offset + 1)?;
        if start > now || (!open && end > now) {
            break;
        }
        out.push(Window { start, end });
        if end > now {
            break;
        }
    }
    if out.last().is_some_and(|w| w.end <= now) && boundary(state.anchor_at, 1200)? <= now {
        return Err(KnowledgeError::InvalidState(
            "Calendar catchup limit exceeded".into(),
        ));
    }
    Ok(out)
}
pub(super) fn add_job(
    state: &mut KnowledgeState,
    source: &SourceDescriptor,
    window: Window,
) -> Result<()> {
    let id = digest(&[
        &state.assistant_id,
        &source.id,
        &source.revision,
        &window.start.to_rfc3339(),
        &window.end.to_rfc3339(),
    ]);
    if state
        .jobs
        .iter()
        .any(|j| j.id == id && j.status != JobStatus::Cancelled)
    {
        return Ok(());
    }
    state
        .jobs
        .retain(|job| job.id != id || job.status != JobStatus::Cancelled);
    if state.jobs.len() >= MAX_JOBS {
        return Err(KnowledgeError::InvalidState(
            "Knowledge job limit reached".into(),
        ));
    }
    let blocked = |reason: &str| StageStatus {
        status: JobStatus::BlockedOnDependency,
        reason: reason.into(),
    };
    let reasons = source.dependency_reasons.clone();

    state.jobs.push(KnowledgeJob {
        id,
        record: None,
        source_id: source.id.clone(),
        source_revision: source.revision.clone(),
        window,
        status: JobStatus::BlockedOnDependency,
        indexing: blocked("Raw indexing execution and readiness must be verified separately"),
        extraction: blocked("Business entity extraction upstream contract required"),
        cleanup: blocked("Bounded model cleanup upstream contract required"),
        publication: blocked("Validated extraction output and index acknowledgement required"),
        dependency_reasons: reasons,
    });
    Ok(())
}
pub(super) fn reconcile_state(
    state: &mut KnowledgeState,
    sources: &[SourceDescriptor],
    now: DateTime<Utc>,
) -> Result<()> {
    if sources.len() > 128 {
        return Err(KnowledgeError::BadRequest(
            "At most 128 knowledge sources are supported".into(),
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for s in sources {
        if s.id.is_empty()
            || s.id.len() > 512
            || s.revision.is_empty()
            || s.revision.len() > 512
            || s.display_name.len() > 512
            || s.dependency_reasons.len() > 16
            || s.dependency_reasons.iter().any(|r| r.len() > 1024)
            || !ids.insert(&s.id)
        {
            return Err(KnowledgeError::BadRequest(
                "Invalid or duplicate source descriptor".into(),
            ));
        }
    }
    for job in &mut state.jobs {
        if !sources
            .iter()
            .any(|s| s.id == job.source_id && s.revision == job.source_revision)
        {
            job.status = JobStatus::Cancelled;
            for stage in [
                &mut job.indexing,
                &mut job.extraction,
                &mut job.cleanup,
                &mut job.publication,
            ] {
                stage.status = JobStatus::Cancelled;
                stage.reason = "Source removed or its authorized binding changed".into();
            }
        }
    }
    state.sources = sources.to_vec();
    if !state.paused {
        for window in windows(state, now, false)? {
            for source in sources {
                add_job(state, source, window.clone())?;
            }
        }
    }
    Ok(())
}
