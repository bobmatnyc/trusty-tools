//! Reproducible benchmark for the `tm ls` asset-staleness fan-out (issue #4322).
//!
//! Why: #4322's remaining scope is a PERFORMANCE claim, and a performance claim
//! is only worth what its measurement is. The numbers in the issue came from a
//! live daemon on one laptop at one moment — unreproducible, and impossible to
//! re-run against a candidate fix to prove it helped. This module builds a
//! synthetic fleet whose SHAPE matches the real one (42 catalog agents at
//! ~23.5 KiB deployed, 52 catalog skills at ~7.4 KiB, N workspaces) and times
//! [`stale_assets_for_many_under`] directly, so before/after runs on the same
//! machine measure exactly the same work.
//! What: [`bench_stale_assets_for_many`], `#[ignore]`d so it never runs in the
//! ordinary `cargo test` gate (it takes seconds and its output is a
//! measurement, not an assertion). Run it with
//! `cargo test -p trusty-mpm --lib bench_stale_assets_for_many -- --ignored --nocapture`.
//! The deployed-read count (via `update_check::deployed_reads_under`, scoped to
//! this benchmark's own fixture paths so concurrent unrelated tests cannot
//! inflate it) is the load-independent companion metric: cold `tm ls` latency is
//! dominated by the NUMBER of files opened, not by CPU, so the read count
//! predicts the cold win on any machine.
//! Test: this file is itself the measurement; the INVARIANT it exists to
//! protect is pinned by
//! `stale_assets_for_many_reads_shared_agent_dir_once_for_the_whole_fleet` in
//! `super::tests`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::summary::stale_assets_for_many_under;
use crate::core::session_assets::{session_asset_staleness_with_catalog, session_plan_under};
use crate::core::update_check::CatalogHashes;
use crate::session_manager::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// Catalog-agent count matching the real bundled roster (42 on 2026-08-02).
const AGENTS: usize = 42;
/// Catalog-skill count matching the real bundled roster (52 on 2026-08-02).
const SKILLS: usize = 52;
/// Byte size of one DEPLOYED agent — the real composed files average 23_541 B.
const AGENT_BYTES: usize = 23_541;
/// Byte size of one skill body — the real bundled skills average 7_401 B.
const SKILL_BYTES: usize = 7_401;
/// Fleet size matching the reported measurement (32 probed sessions).
const SESSIONS: usize = 32;
/// Timed repetitions. More than one because a single timing on a loaded
/// laptop is noise; the report quotes the median and the full spread.
const REPS: usize = 7;

/// Write `bytes` of deterministic filler under `path`, prefixed by `head`.
fn write_sized(path: &Path, head: &str, bytes: usize) {
    let mut body = String::with_capacity(bytes + head.len());
    body.push_str(head);
    body.push('\n');
    while body.len() < bytes {
        body.push_str("filler line for realistic file size measurement\n");
    }
    std::fs::write(path, body).unwrap();
}

/// Build a synthetic fleet shaped like the real one and return its records.
///
/// Why: the two halves of the deployed-side comparison have very different
/// sharing properties, and only a fixture that reproduces BOTH can measure the
/// difference — agents deploy into ONE machine-global directory
/// (`FrameworkPaths::agent_deploy_dir`, never rewritten per workspace since
/// #4409), while skills deploy per-workspace into
/// `<workspace>/.claude/skills`. A fixture that gave each session its own agent
/// directory would measure a system that does not exist.
/// What: seeds `agent_source_dir`/`skill_source_dir` with realistically sized
/// bodies, deploys agents ONCE into the shared deploy dir, then creates
/// `sessions` workspaces each with its own deployed skill tree, and returns one
/// `Active` record per workspace.
///
/// #5040: `base` is the caller's temp dir, used as both the framework base and
/// the parent of each workspace — the two roles a redirected `$HOME` played.
fn build_fleet(base: &Path, sessions: usize) -> Vec<SessionRecord> {
    let fw = crate::core::paths::FrameworkPaths::under(base);
    let agent_source = fw.agent_source_dir();
    let skill_source = fw.skill_source_dir();
    std::fs::create_dir_all(&agent_source).unwrap();
    std::fs::create_dir_all(&skill_source).unwrap();

    for i in 0..AGENTS {
        write_sized(
            &agent_source.join(format!("bench-agent-{i:02}.md")),
            &format!("# bench agent {i}"),
            AGENT_BYTES,
        );
    }
    for i in 0..SKILLS {
        write_sized(
            &skill_source.join(format!("bench-skill-{i:02}.md")),
            &format!("# bench skill {i}"),
            SKILL_BYTES,
        );
    }

    // Agents: ONE shared destination for the whole fleet (#4409).
    crate::core::agent_deployer::deploy_agents_filtered(
        &agent_source,
        &fw.agent_deploy_dir(),
        |_| true,
    )
    .unwrap();

    (0..sessions)
        .map(|i| {
            let ws = base.join(format!("bench-ws-{i:03}"));
            std::fs::create_dir_all(&ws).unwrap();
            let ws_fw = crate::core::paths::FrameworkPaths::for_managed_workspace_under(base, &ws);
            // Skills: per-workspace destination.
            crate::core::skill_tiers::deploy_all_skill_tiers(
                &skill_source,
                &fw.user_skill_source_dir(),
                &ws_fw.claude_skills_dir(),
                |_| true,
            )
            .unwrap();
            let mut r = super::tests::make_record(None);
            r.state = ManagedSessionState::Active;
            r.workspace_path = Some(ws);
            r
        })
        .collect()
}

/// The PRE-#4322 fan-out, preserved verbatim as the benchmark's baseline.
///
/// Why: measuring "before" by checking out `main`, running, then checking out
/// the branch and running again compares two numbers taken minutes apart on a
/// shared laptop — and this laptop runs other agents' `cargo build`s. A load
/// spike between the two runs is indistinguishable from a real improvement.
/// (Observed directly while preparing this PR: the identical baseline measured
/// 213 ms at load ~2 and 1301 ms at load ~29.) Keeping the old shape here lets
/// both variants be timed microseconds apart, under whatever load the machine
/// happens to be under, so the RATIO is meaningful even when the absolute
/// numbers are not.
/// What: shares one [`CatalogHashes`] per `(agent_source, skill_source)` pair
/// (#2444) and fans out one blocking task per session (#4326), each calling
/// [`session_asset_staleness_with_catalog`] — which reads the machine-global
/// deployed-agent directory itself, once per session. That per-session read is
/// exactly what #4322 removes.
async fn stale_assets_per_session_agent_read(
    records: Vec<SessionRecord>,
    base: PathBuf,
) -> HashMap<ManagedSessionId, bool> {
    let inputs = tokio::task::spawn_blocking(move || {
        let mut cache: HashMap<(PathBuf, PathBuf), Arc<CatalogHashes>> = HashMap::new();
        let mut out = Vec::with_capacity(records.len());
        for record in records {
            // #5040: same explicit-base resolution the measured path uses.
            let (fw, plan) = session_plan_under(&record, &base);
            let key = (plan.agent_source.clone(), plan.skill_source.clone());
            let catalog = cache
                .entry(key)
                .or_insert_with(|| {
                    Arc::new(CatalogHashes::compute(
                        &plan.agent_source,
                        &plan.skill_source,
                    ))
                })
                .clone();
            out.push((record.id, fw, plan, catalog));
        }
        out
    })
    .await
    .unwrap_or_default();

    let mut probes = tokio::task::JoinSet::new();
    for (id, fw, plan, catalog) in inputs {
        probes.spawn_blocking(move || {
            (
                id,
                session_asset_staleness_with_catalog(&fw, &plan, &catalog).stale,
            )
        });
    }
    let mut result = HashMap::with_capacity(probes.len());
    while let Some(res) = probes.join_next().await {
        if let Ok((id, stale)) = res {
            result.insert(id, stale);
        }
    }
    result
}

/// Median of a timing sample.
fn median(mut xs: Vec<Duration>) -> Duration {
    xs.sort_unstable();
    xs[xs.len() / 2]
}

/// Time the pre-#4322 and post-#4322 fan-outs ALTERNATELY and report both.
///
/// Why: the deliverable of #4322 is the measurement, not the diff. Alternating
/// the two variants within one process (rather than timing two git checkouts
/// minutes apart) is what makes the comparison survive a busy machine, and
/// printing every repetition rather than a mean keeps a load-induced outlier
/// visible instead of averaged away.
/// What: builds the fleet once, then for each repetition times the OLD path and
/// the NEW path, asserting after every pair that they produced the IDENTICAL
/// verdict map — so a "speed-up" that changed an answer fails the benchmark
/// rather than being reported as a win. The two are run in ALTERNATING order
/// (old-first on even reps, new-first on odd) so that whichever variant runs
/// second cannot be systematically advantaged by a filesystem the other just
/// warmed, nor disadvantaged by thermal/scheduling drift within the pair
/// (#4619 review, LOW). Read counts are scoped to this test's own fixture
/// paths, never to a process-global total. Prints per-rep timings, both
/// medians, the ratio, and the deployed-side read count for each variant.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
#[ignore = "benchmark: run explicitly with --ignored --nocapture"]
async fn bench_stale_assets_for_many() {
    // #5040: an explicit temp base, not a redirected `$HOME`.
    let base = super::tests::fake_base();
    let records = build_fleet(base.path(), SESSIONS);
    // Every deployed read this fixture performs lands under that base: agents
    // under `<base>/.trusty-tools/...`, skills under `<base>/bench-ws-*`.
    let scope = base.path().to_path_buf();

    // Untimed warm-up so the measurement reflects steady state rather than
    // first-touch page-cache population of the fixture we just wrote.
    let _ = stale_assets_for_many_under(records.clone(), scope.clone()).await;
    let _ = stale_assets_per_session_agent_read(records.clone(), scope.clone()).await;

    let mut old_timings = Vec::with_capacity(REPS);
    let mut new_timings = Vec::with_capacity(REPS);
    let (mut old_reads, mut new_reads) = (0usize, 0usize);

    for rep in 0..REPS {
        let run_old = || async {
            crate::core::update_check::reset_deployed_read_log();
            let t = Instant::now();
            let out = stale_assets_per_session_agent_read(records.clone(), scope.clone()).await;
            let elapsed = t.elapsed();
            let reads = crate::core::update_check::deployed_reads_under(&scope);
            (out, elapsed, reads)
        };
        let run_new = || async {
            crate::core::update_check::reset_deployed_read_log();
            let t = Instant::now();
            let out = stale_assets_for_many_under(records.clone(), scope.clone()).await;
            let elapsed = t.elapsed();
            let reads = crate::core::update_check::deployed_reads_under(&scope);
            (out, elapsed, reads)
        };

        // Alternate which variant goes first (#4619 review, LOW).
        let ((old, old_elapsed, o_reads), (new, new_elapsed, n_reads)) = if rep % 2 == 0 {
            let a = run_old().await;
            let b = run_new().await;
            (a, b)
        } else {
            let b = run_new().await;
            let a = run_old().await;
            (a, b)
        };
        old_reads = o_reads;
        new_reads = n_reads;

        assert_eq!(old.len(), SESSIONS, "every session must get a verdict");
        assert_eq!(
            old, new,
            "the shared deployed-agent read must produce the IDENTICAL verdict \
             map as the per-session read it replaces — a faster wrong answer is \
             strictly worse than the slow right one (#4322)"
        );

        println!(
            "  rep {rep} ({}): per-session {:>9.3} ms ({old_reads} reads) | \
             shared {:>9.3} ms ({new_reads} reads)",
            if rep % 2 == 0 {
                "old-first"
            } else {
                "new-first"
            },
            old_elapsed.as_secs_f64() * 1000.0,
            new_elapsed.as_secs_f64() * 1000.0,
        );
        old_timings.push(old_elapsed);
        new_timings.push(new_elapsed);
    }

    let old_med = median(old_timings);
    let new_med = median(new_timings);
    println!(
        "BENCH sessions={SESSIONS} agents={AGENTS} skills={SKILLS} \
         per_session_median={:.3}ms shared_median={:.3}ms speedup={:.2}x \
         reads {old_reads} -> {new_reads}",
        old_med.as_secs_f64() * 1000.0,
        new_med.as_secs_f64() * 1000.0,
        old_med.as_secs_f64() / new_med.as_secs_f64(),
    );
}
