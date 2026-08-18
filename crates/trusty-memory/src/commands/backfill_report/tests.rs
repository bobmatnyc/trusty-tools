//! Tests for the ADR-0028 read-only backfill report.
//!
//! The load-bearing ones are the exactness of the log join (a silent
//! under-count would make the ranking meaningless) and the read-only contract
//! (a report that mutates the estate is the one outcome the ADR forbids).

use std::path::Path;

use chrono::{Duration, Utc};
use trusty_common::memory_core::palace::{Drawer, DrawerType};
use uuid::Uuid;

use super::candidates::build_census;
use super::log_index::InjectionIndex;
use super::render::{render_json, render_text};
use super::signals::{observe, Signal};
use crate::commands::prompt_context::format::drawer_preview;

// ---------------------------------------------------------------- helpers

/// One JSONL log line in the real `PromptLogEntry` shape.
fn log_line(palace: &str, injection: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-08-01T00:00:00Z",
        "hook_type": "UserPromptSubmit",
        "injection_kind": "prompt-context-facts",
        "palace": palace,
        "trigger_prompt": "hi",
        "injection": injection,
        "injection_length": injection.len(),
        "duration_ms": 1,
    })
    .to_string()
}

/// Render a drawer section the way `compose_injection` does.
fn section(palace: &str, bullets: &[(&str, &[&str])]) -> String {
    let mut s = format!("## Relevant memories from palace `{palace}`\n");
    for (preview, tags) in bullets {
        s.push_str("- ");
        s.push_str(preview);
        if !tags.is_empty() {
            s.push_str("  _(tags: ");
            s.push_str(
                &tags
                    .iter()
                    .map(|t| format!("`{t}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            s.push_str(")_");
        }
        s.push('\n');
    }
    s
}

fn write_log(dir: &Path, name: &str, lines: &[String]) {
    std::fs::write(dir.join(name), format!("{}\n", lines.join("\n"))).expect("write log");
}

fn drawer(content: &str, importance: f32, age_days: i64, tags: &[&str]) -> Drawer {
    let mut d = Drawer::new(Uuid::new_v4(), content);
    d.importance = importance;
    d.created_at = Utc::now() - Duration::days(age_days);
    d.tags = tags.iter().map(|t| t.to_string()).collect();
    d.drawer_type = DrawerType::Unknown;
    d
}

// ------------------------------------------------------------- log_index

#[test]
fn bullets_are_counted_once_per_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The same drawer appears under two palace sections in ONE injection. It
    // reached one turn, so it must count once.
    let inj = format!(
        "{}\n{}",
        section("p", &[("alpha fact", &["status"])]),
        section("p", &[("alpha fact", &["status"])])
    );
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", &inj)],
    );

    let index = InjectionIndex::scan_dir(dir.path()).expect("scan");
    assert_eq!(index.injections_for("p", "alpha fact"), 1);
    assert_eq!(index.total_injections("p"), 1);
}

#[test]
fn kg_facts_section_is_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let inj = format!(
        "{}\n## Relevant KG facts\n- tag:status **tags** drawer:abc\n",
        section("p", &[("real drawer", &["status"])])
    );
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", &inj)],
    );

    let index = InjectionIndex::scan_dir(dir.path()).expect("scan");
    assert_eq!(index.injections_for("p", "real drawer"), 1);
    assert_eq!(
        index.injections_for("p", "tag:status **tags** drawer:abc"),
        0,
        "a KG triple line must never be mistaken for a drawer bullet"
    );
}

#[test]
fn truncated_tail_bullet_matches_by_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let full = "SESSION CHECKPOINT 2026-07-16 — MERGE TRAIN COMPLETE, eight PRs merged this session and every one reviewed";
    // The 4 KiB cap cut this bullet mid-preview: the tag run is gone and the
    // whole injection ends with the cap's `…`.
    let cut = &full[..80];
    let inj = format!("## Relevant memories from palace `p`\n- {cut}…");
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", &inj)],
    );

    let index = InjectionIndex::scan_dir(dir.path()).expect("scan");
    assert_eq!(
        index.injections_for("p", full),
        1,
        "a drawer cut by the byte cap still reached the turn"
    );
}

#[test]
fn short_partial_is_not_matched() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Far below MIN_PARTIAL_CHARS — matching it as a prefix would inflate every
    // drawer whose content starts with "SESSION".
    let inj = "## Relevant memories from palace `p`\n- SESSION…";
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", inj)],
    );

    let index = InjectionIndex::scan_dir(dir.path()).expect("scan");
    assert_eq!(index.injections_for("p", "SESSION CHECKPOINT one"), 0);
    assert_eq!(index.injections_for("p", "SESSION CHECKPOINT two"), 0);
}

#[test]
fn untagged_bullet_counts_exactly() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Short AND untagged — the case that would vanish if every tag-less bullet
    // were treated as a byte-cap tail, since it is far below MIN_PARTIAL_CHARS.
    let short = "untagged drawer";
    let inj = format!("## Relevant memories from palace `p`\n- {short}\n");
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", &inj)],
    );

    let index = InjectionIndex::scan_dir(dir.path()).expect("scan");
    assert_eq!(index.injections_for("p", short), 1);
}

#[test]
fn untagged_bullet_mid_block_is_not_treated_as_a_tail() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The injection IS cap-truncated, but the untagged bullet is not its last
    // line, so only the genuine tail may be prefix-matched.
    let inj = "## Relevant memories from palace `p`\n- untagged\n- tail bullet that the cap cut before its tags could be written out…";
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", inj)],
    );

    let index = InjectionIndex::scan_dir(dir.path()).expect("scan");
    assert_eq!(index.injections_for("p", "untagged"), 1);
    assert_eq!(
        index.injections_for(
            "p",
            "tail bullet that the cap cut before its tags could be written out… and then some more"
        ),
        1
    );
}

#[test]
fn counts_are_scoped_per_palace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let inj = section("a", &[("shared text", &["status"])]);
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("a", &inj), log_line("a", &inj)],
    );

    let index = InjectionIndex::scan_dir(dir.path()).expect("scan");
    assert_eq!(index.injections_for("a", "shared text"), 2);
    assert_eq!(index.injections_for("b", "shared text"), 0);
}

#[test]
fn scan_dir_on_missing_directory_is_empty_not_error() {
    let index = InjectionIndex::scan_dir(Path::new("/nonexistent/trusty/logs")).expect("scan");
    assert!(index.saw_no_logs());
    assert_eq!(index.stats.injections_counted, 0);
}

#[test]
fn scan_dir_counts_across_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let inj = section("p", &[("rolled fact", &["status"])]);
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", &inj)],
    );
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-02.jsonl",
        &[log_line("p", &inj)],
    );
    // Not a hook log — must be ignored.
    std::fs::write(dir.path().join("other.jsonl"), log_line("p", &inj)).expect("write");

    let index = InjectionIndex::scan_dir(dir.path()).expect("scan");
    assert_eq!(index.stats.files_scanned, 2);
    assert_eq!(index.injections_for("p", "rolled fact"), 2);
}

#[test]
fn non_prompt_context_entries_are_not_counted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut line: serde_json::Value =
        serde_json::from_str(&log_line("p", &section("p", &[("x", &["status"])]))).expect("json");
    line["injection_kind"] = serde_json::json!("inbox-check-messages");
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[line.to_string()],
    );

    let index = InjectionIndex::scan_dir(dir.path()).expect("scan");
    assert_eq!(index.total_injections("p"), 0);
    assert_eq!(index.injections_for("p", "x"), 0);
}

#[test]
fn preview_matches_injection_bullet() {
    // The join's whole correctness rests on the report rendering a preview
    // identically to the injection. Pin it: a >220-char body must truncate the
    // same way on both sides, which it does because both call `drawer_preview`.
    let content = "word ".repeat(200);
    let preview = drawer_preview(&content);
    assert_eq!(preview.chars().count(), 220);
    assert!(preview.ends_with('…'));

    let dir = tempfile::tempdir().expect("tempdir");
    let inj = section("p", &[(preview.as_str(), &["status"])]);
    write_log(
        dir.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", &inj)],
    );
    let index = InjectionIndex::scan_dir(dir.path()).expect("scan");
    assert_eq!(index.injections_for("p", &drawer_preview(&content)), 1);
}

// --------------------------------------------------------------- signals

#[test]
fn tags_are_reported_verbatim() {
    let d = drawer(
        "some fact",
        0.5,
        1,
        &["status", "resume-target", "unrelated"],
    );
    let s = observe(&d, 1.0, None);
    assert!(s.contains(&Signal::Tagged("status".into())));
    assert!(s.contains(&Signal::Tagged("resume-target".into())));
    assert!(
        !s.iter()
            .any(|x| matches!(x, Signal::Tagged(t) if t == "unrelated")),
        "only the tags §C4 censused are surfaced"
    );
}

#[test]
fn date_stamp_only_scans_the_opening() {
    let opening = drawer("SESSION CHECKPOINT 2026-07-16 — done", 0.5, 1, &[]);
    assert!(observe(&opening, 1.0, None).contains(&Signal::DateStamped));

    let buried = drawer(&format!("{} 2026-07-16", "x".repeat(400)), 0.5, 1, &[]);
    assert!(
        !observe(&buried, 1.0, None).contains(&Signal::DateStamped),
        "a date deep in the body is not a headline date stamp"
    );
}

#[test]
fn weight_retained_needs_both_age_and_weight() {
    // 3 days old: too young to be called stale, even though weight is retained.
    let young = drawer("x", 1.0, 3, &[]);
    assert!(!observe(&young, 3.0, None).contains(&Signal::WeightRetained));

    // 19 days at a 90-day half-life keeps 86% — the ADR §C7 case exactly.
    let old = drawer("x", 1.0, 19, &[]);
    assert!(observe(&old, 19.0, None).contains(&Signal::WeightRetained));
}

#[test]
fn max_importance_and_expiry_are_reported() {
    let mut d = drawer("x", 1.0, 1, &[]);
    assert!(observe(&d, 1.0, None).contains(&Signal::MaxImportance));
    assert!(observe(&d, 1.0, None).contains(&Signal::NoExpiry));

    d.expires_at = Some(Utc::now() + Duration::days(7));
    assert!(!observe(&d, 1.0, None).contains(&Signal::NoExpiry));
}

#[test]
fn no_signal_implies_empty_list() {
    let mut d = drawer("plain text with no date and no tags", 0.5, 1, &[]);
    d.expires_at = Some(Utc::now() + Duration::days(1));
    assert!(observe(&d, 1.0, None).is_empty());
}

#[test]
fn predates_log_window_fires_only_outside_coverage() {
    let window_start = Utc::now() - Duration::days(31);

    // Created before the logs begin: part of its life is unmeasured, so a 0 here
    // means "unknown", not "cold".
    let older = drawer("older than the logs", 0.5, 60, &[]);
    assert!(observe(&older, 60.0, Some(window_start)).contains(&Signal::PredatesLogWindow));

    // Created inside the window: a 0 here genuinely means nobody retrieves it.
    let inside = drawer("created inside the window", 0.5, 5, &[]);
    assert!(!observe(&inside, 5.0, Some(window_start)).contains(&Signal::PredatesLogWindow));
}

#[test]
fn predates_log_window_is_silent_when_no_log_was_scanned() {
    // With no window at all, every count is 0 for one reason the report states
    // once at the top. Tagging every row would be noise, not information.
    let ancient = drawer("very old drawer", 0.5, 900, &[]);
    assert!(!observe(&ancient, 900.0, None).contains(&Signal::PredatesLogWindow));
}

// -------------------------------------------------------- census + render

/// Build a palace on disk and return (registry_dir, palace_slug).
fn fixture_palace(root: &Path, slug: &str, drawers: &[Drawer]) {
    use std::sync::Arc;
    use trusty_common::memory_core::store::kg_redb::KgStoreRedb;
    use trusty_common::memory_core::store::OpenIntent;

    let data_dir = root.join(slug);
    std::fs::create_dir_all(&data_dir).expect("palace dir");
    // A palace is discovered by `PalaceStore::list_palaces` via its metadata;
    // write it the same way the store does.
    trusty_common::memory_core::store::PalaceStore::save_palace(
        &trusty_common::memory_core::palace::Palace {
            id: trusty_common::memory_core::palace::PalaceId(slug.to_string()),
            name: slug.to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: data_dir.clone(),
        },
    )
    .expect("save palace");

    let store = KgStoreRedb::open_with_intent(&data_dir.join("kg.redb"), OpenIntent::Writer)
        .expect("open store");
    let store = Arc::new(store);
    for d in drawers {
        store.upsert_drawer(d).expect("upsert");
    }
}

#[test]
fn ranks_by_injection_count() {
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");

    let hot = drawer("HOT drawer content", 1.0, 20, &["status"]);
    let cold = drawer("COLD drawer content", 0.5, 20, &[]);
    fixture_palace(root.path(), "p", &[hot.clone(), cold.clone()]);

    let hot_inj = section("p", &[(&drawer_preview(hot.content()), &["status"])]);
    let cold_inj = section("p", &[(&drawer_preview(cold.content()), &[])]);
    write_log(
        logs.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[
            log_line("p", &hot_inj),
            log_line("p", &hot_inj),
            log_line("p", &hot_inj),
            log_line("p", &cold_inj),
        ],
    );

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");
    assert_eq!(census.rows.len(), 2);
    assert_eq!(census.rows[0].drawer_id, hot.id);
    assert_eq!(census.rows[0].injections, 3);
    assert!((census.rows[0].share_of_turns - 0.75).abs() < 1e-9);
    assert_eq!(census.rows[1].drawer_id, cold.id);
    assert_eq!(census.rows[1].injections, 1);
}

#[test]
fn min_injections_filters() {
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    let a = drawer("kept drawer", 0.5, 5, &[]);
    let b = drawer("dropped drawer", 0.5, 5, &[]);
    fixture_palace(root.path(), "p", &[a.clone(), b.clone()]);
    let inj = section("p", &[(&drawer_preview(a.content()), &[])]);
    write_log(
        logs.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", &inj), log_line("p", &inj)],
    );

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 2).expect("census");
    assert_eq!(census.rows.len(), 1);
    assert_eq!(census.rows[0].drawer_id, a.id);
    assert_eq!(
        census.drawers_total, 2,
        "the filter hides rows, it does not hide that the drawers were read"
    );
}

#[test]
fn zero_injection_drawers_rank_last() {
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    let never = drawer("never injected", 1.0, 30, &["status"]);
    fixture_palace(root.path(), "p", std::slice::from_ref(&never));

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");
    assert_eq!(census.rows.len(), 1);
    assert_eq!(
        census.rows[0].injections, 0,
        "high importance alone must never manufacture a frequency"
    );
    assert_eq!(census.rows[0].share_of_turns, 0.0);
}

#[test]
fn colliding_excerpts_are_marked_on_every_affected_row() {
    // Two drawers whose contents differ only past the 220-char preview cut, so
    // they render an identical excerpt and the hook log cannot tell them apart.
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    let shared_head = "SESSION PAUSE 2026-07-16 — RESUME TARGET. ".repeat(8);
    let a = drawer(&format!("{shared_head} FIRST tail"), 1.0, 20, &["status"]);
    let b = drawer(&format!("{shared_head} SECOND tail"), 1.0, 20, &["status"]);
    let unique = drawer("a drawer nobody else resembles", 0.5, 20, &[]);
    fixture_palace(root.path(), "p", &[a.clone(), b.clone(), unique.clone()]);

    assert_eq!(
        drawer_preview(a.content()),
        drawer_preview(b.content()),
        "fixture precondition: these two must actually collide"
    );

    let inj = section("p", &[(&drawer_preview(a.content()), &["status"])]);
    write_log(
        logs.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", &inj), log_line("p", &inj)],
    );

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");

    let row_a = census.rows.iter().find(|r| r.drawer_id == a.id).expect("a");
    let row_b = census.rows.iter().find(|r| r.drawer_id == b.id).expect("b");
    let row_u = census
        .rows
        .iter()
        .find(|r| r.drawer_id == unique.id)
        .expect("u");

    assert_eq!(row_a.collision_peers, Some(1), "both sides must be marked");
    assert_eq!(row_b.collision_peers, Some(1), "both sides must be marked");
    assert_eq!(
        row_u.collision_peers, None,
        "a unique excerpt is not marked"
    );

    assert_eq!(
        (row_a.injections, row_b.injections),
        (2, 2),
        "the shared count is reported on both — the marker is what makes that honest"
    );
    assert_ne!(
        row_a.content_digest, row_b.content_digest,
        "the digest must separate rows the excerpt cannot"
    );

    // And the reader sees it, in the header and on the rows.
    let mut buf = Vec::new();
    render_text(&mut buf, &census, &index.stats, 25).expect("render");
    let s = String::from_utf8(buf).expect("utf8");
    assert!(s.contains("2 row(s) share an excerpt"));
    // Count stanza markers only — the header's "Marked ⚠ SHARED below" mentions
    // the same string.
    assert_eq!(
        s.lines().filter(|l| l.starts_with("    ⚠ SHARED")).count(),
        2
    );
    assert!(s.contains(&row_a.content_digest));
    assert!(s.contains(&row_b.content_digest));
}

#[test]
fn unique_excerpts_are_not_marked() {
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    fixture_palace(
        root.path(),
        "p",
        &[drawer("alpha", 0.5, 1, &[]), drawer("beta", 0.5, 1, &[])],
    );

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");
    assert!(census.rows.iter().all(|r| r.collision_peers.is_none()));

    let mut buf = Vec::new();
    render_text(&mut buf, &census, &index.stats, 25).expect("render");
    let s = String::from_utf8(buf).expect("utf8");
    assert!(!s.contains("SHARED"), "no collision, no warning");
    assert!(!s.contains("share an excerpt"));
}

#[test]
fn identical_excerpts_in_different_palaces_do_not_collide() {
    // The hook-log index is keyed per palace, so the same excerpt in two palaces
    // is two independent counts and neither is misattributed.
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    let same = "an excerpt that appears in two palaces";
    fixture_palace(root.path(), "a", &[drawer(same, 0.5, 1, &[])]);
    fixture_palace(root.path(), "b", &[drawer(same, 0.5, 1, &[])]);

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");
    assert_eq!(census.rows.len(), 2);
    assert!(census.rows.iter().all(|r| r.collision_peers.is_none()));
}

#[test]
fn collision_json_field_is_always_present() {
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    let d = drawer("solo drawer", 0.5, 1, &[]);
    fixture_palace(root.path(), "p", std::slice::from_ref(&d));

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");
    let mut buf = Vec::new();
    render_json(&mut buf, &census, &index.stats, 25).expect("render");
    let v: serde_json::Value = serde_json::from_slice(&buf).expect("json");

    // Present-and-null, never absent: a consumer tests one field rather than
    // inferring safety from a missing key.
    assert!(
        v["candidates"][0]
            .as_object()
            .expect("obj")
            .contains_key("excerpt_collision_peers"),
        "the field must exist even when there is no collision"
    );
    assert!(v["candidates"][0]["excerpt_collision_peers"].is_null());
    assert!(v["candidates"][0]["content_digest"].is_string());
}

#[test]
fn palace_filter_narrows_the_census() {
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    fixture_palace(root.path(), "keep", &[drawer("a", 0.5, 1, &[])]);
    fixture_palace(root.path(), "skip", &[drawer("b", 0.5, 1, &[])]);

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, Some("keep"), 0).expect("census");
    assert_eq!(census.outcomes.len(), 1);
    assert_eq!(census.outcomes[0].palace, "keep");
}

#[test]
fn report_writes_nothing_to_the_palace() {
    // The ADR forbids exactly one outcome: a backfill tool that mutates. An
    // expired drawer is the sharp case — `PalaceHandle::open` would DELETE it.
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    let mut expired = drawer("long expired status drawer", 1.0, 40, &["status"]);
    expired.expires_at = Some(Utc::now() - Duration::days(30));
    let live = drawer("live drawer", 0.5, 1, &[]);
    fixture_palace(root.path(), "p", &[expired.clone(), live.clone()]);

    let kg = root.path().join("p").join("kg.redb");
    let before = std::fs::metadata(&kg).expect("stat").len();

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");

    assert_eq!(
        census.drawers_total, 2,
        "the expired drawer must still be READ — it is precisely what a human triages"
    );
    assert!(
        census
            .rows
            .iter()
            .any(|r| r.drawer_id == expired.id && r.has_expiry),
        "an already-triaged drawer is reported as such, not silently dropped"
    );
    assert_eq!(
        std::fs::metadata(&kg).expect("stat").len(),
        before,
        "the report must not have written to the palace store"
    );

    // And the drawer is still there on a fresh read.
    let index2 = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census2 = build_census(root.path(), &index2, None, 0).expect("census");
    assert_eq!(census2.drawers_total, 2);
}

#[test]
fn incompatible_store_is_reported_not_recreated() {
    // Opening an incompatible-format store with `Database::create` renames it
    // aside and starts a fresh empty one (concurrent_open.rs, #702). Reading a
    // copy means that happens to the copy — and the report must say so rather
    // than present the palace as empty.
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    fixture_palace(root.path(), "good", &[drawer("a", 0.5, 1, &[])]);
    fixture_palace(root.path(), "bad", &[drawer("b", 0.5, 1, &[])]);
    let bad_kg = root.path().join("bad").join("kg.redb");
    std::fs::write(&bad_kg, b"not a redb file at all").expect("corrupt");
    let before = std::fs::read(&bad_kg).expect("read");

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");

    assert_eq!(census.outcomes.len(), 2);
    let bad = census
        .outcomes
        .iter()
        .find(|o| o.palace == "bad")
        .expect("bad");
    assert!(bad.error.is_some(), "a bad palace is reported, not fatal");
    assert_eq!(census.drawers_total, 1, "the good palace still reports");
    assert_eq!(
        std::fs::read(&bad_kg).expect("read"),
        before,
        "the unreadable store must be left exactly as found, never recreated"
    );
    assert!(
        !bad_kg.with_extension("redb.v2-incompatible").exists(),
        "no backup file may be left beside the live store"
    );
}

#[test]
fn palace_without_a_kg_store_is_empty_not_an_error() {
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    fixture_palace(root.path(), "p", &[drawer("a", 0.5, 1, &[])]);
    std::fs::remove_file(root.path().join("p").join("kg.redb")).expect("remove");

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");
    assert_eq!(census.outcomes.len(), 1);
    assert!(census.outcomes[0].error.is_none());
    assert_eq!(census.drawers_total, 0);
}

#[test]
fn text_output_leads_with_the_read_only_contract() {
    let census = super::candidates::Census::default();
    let stats = super::log_index::ScanStats::default();
    let mut buf = Vec::new();
    render_text(&mut buf, &census, &stats, 25).expect("render");
    let s = String::from_utf8(buf).expect("utf8");
    assert!(s.contains("READ-ONLY"));
    assert!(s.contains("never sets expires_at"));
    assert!(
        s.contains("does not migrate the drawers already on disk"),
        "the help must state that this report is the only route for existing drawers"
    );
}

#[test]
fn empty_census_says_why() {
    let census = super::candidates::Census::default();
    let stats = super::log_index::ScanStats::default();
    let mut buf = Vec::new();
    render_text(&mut buf, &census, &stats, 25).expect("render");
    let s = String::from_utf8(buf).expect("utf8");
    assert!(
        s.contains("missing data, not an absence of stale drawers"),
        "zero counts from a missing log must not read as a clean estate"
    );
}

#[test]
fn stanza_carries_every_decision_field() {
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    let d = drawer(
        "SESSION CHECKPOINT 2026-07-16 — merge train complete",
        1.0,
        19,
        &["status"],
    );
    fixture_palace(root.path(), "p", std::slice::from_ref(&d));
    let inj = section("p", &[(&drawer_preview(d.content()), &["status"])]);
    write_log(
        logs.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", &inj)],
    );

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");
    let mut buf = Vec::new();
    render_text(&mut buf, &census, &index.stats, 25).expect("render");
    let s = String::from_utf8(buf).expect("utf8");

    assert!(s.contains(&super::candidates::short_id(&d.id)), "drawer id");
    assert!(s.contains("SESSION CHECKPOINT 2026-07-16"), "excerpt");
    assert!(s.contains("injections   1"), "frequency");
    assert!(
        s.contains("100.0% of p turns"),
        "share names its denominator"
    );
    assert!(s.contains("importance 1.00"), "importance");
    assert!(s.contains("age          19"), "age");
    assert!(s.contains("tag:status"), "signals");
    assert!(s.contains("not set"), "expiry state");
    assert!(
        !s.to_lowercase().contains("point-in-time"),
        "the report must not emit a tier verdict"
    );
}

#[test]
fn json_round_trips() {
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    let d = drawer("json drawer", 0.75, 4, &["status"]);
    fixture_palace(root.path(), "p", std::slice::from_ref(&d));
    let inj = section("p", &[(&drawer_preview(d.content()), &["status"])]);
    write_log(
        logs.path(),
        "enriched-prompts.2026-08-01.jsonl",
        &[log_line("p", &inj)],
    );

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");
    let mut buf = Vec::new();
    render_json(&mut buf, &census, &index.stats, 25).expect("render");
    let v: serde_json::Value = serde_json::from_slice(&buf).expect("json");

    assert_eq!(v["read_only"], serde_json::json!(true));
    assert_eq!(v["candidates"][0]["drawer_id"], d.id.to_string());
    assert_eq!(v["candidates"][0]["injections"], 1);
    assert_eq!(v["coverage"]["injections_counted"], 1);
    assert!(v["candidates"][0]["signals"]
        .as_array()
        .expect("signals")
        .iter()
        .any(|s| s == "tag:status"));
}

#[test]
fn limit_caps_the_output_without_hiding_the_total() {
    let logs = tempfile::tempdir().expect("logs");
    let root = tempfile::tempdir().expect("root");
    let ds: Vec<Drawer> = (0..5)
        .map(|i| drawer(&format!("drawer number {i}"), 0.5, 1, &[]))
        .collect();
    fixture_palace(root.path(), "p", &ds);

    let index = InjectionIndex::scan_dir(logs.path()).expect("scan");
    let census = build_census(root.path(), &index, None, 0).expect("census");
    let mut buf = Vec::new();
    render_text(&mut buf, &census, &index.stats, 2).expect("render");
    let s = String::from_utf8(buf).expect("utf8");
    assert!(s.contains("(2 of 5 matching drawers shown"));
}
