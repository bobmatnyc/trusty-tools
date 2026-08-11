//! #5036 (read half): an ALIASED palace's lexical lane must read the corpus
//! the backfill actually wrote.
//!
//! Why: `palace_aliases.json` on this host maps `bobmatnyc-trusty-tools →
//! trusty-tools`. `open_palace` follows that redirect, so the recall handler
//! held a handle on `trusty-tools` while passing the requested slug to the
//! BM25 lane. `palace_ids_on_disk` only enumerates directories holding a
//! `palace.json`, and the alias has none — so the startup sweep indexes the
//! canonical palace, logs full coverage, and the search reads a corpus nobody
//! ever wrote to.
//!
//! The divergence is between a RESOLVED handle and a REQUESTED slug, so a mock
//! cannot show it: this test registers a real alias, resolves it through the
//! real registry, and drives a real `trusty-bm25-daemon` over its real socket.
//!
//! What: `#[ignore]`d because it needs the daemon binary. Run with
//! `cargo test -p trusty-memory --test bm25_alias_recall -- --include-ignored`.
//!
//! Test: this *is* the test file.

mod alias_lane;

use serde_json::json;
use trusty_common::bm25_client::socket_path_for_palace;
use trusty_common::memory_core::palace::{Drawer, PalaceId};
use trusty_memory::bm25_backfill::run_startup_sweep;
use trusty_memory::tools::dispatch_tool;

/// Why: the startup sweep writes the CANONICAL palace's corpus and never sees
/// the alias. A recall issued against the alias slug therefore has to reach
/// that same corpus, or the lane is a no-op that reports success.
/// What: seeds one drawer, sweeps, then recalls THROUGH THE ALIAS on the
/// embedder-warming path — where the lexical lane is the only lane, so a
/// vector hit cannot mask its result — and asserts the drawer comes back.
/// Test: this test itself. Against the parent commit the lane addresses the
/// default palace's socket and the recall returns nothing.
#[ignore = "requires trusty-bm25-daemon binary on disk; run with --include-ignored"]
#[tokio::test(flavor = "multi_thread")]
async fn an_aliased_recall_reads_the_corpus_the_backfill_wrote() {
    let fx = alias_lane::Aliased::new("r");
    let handle = fx
        .state
        .registry
        .open_palace(&fx.state.data_root, &PalaceId::new(fx.canonical.clone()))
        .expect("open canonical palace");

    // A drawer nothing but the lexical lane can reach: pushed onto the served
    // table (so a BM25 hit has something to hydrate from) but absent from
    // `l1_drawers`, and the palace has no identity row, so `retrieve_l0_l1`
    // returns nothing at all.
    let token = "zqxjrollout";
    let drawer = Drawer::new(uuid::Uuid::new_v4(), format!("{token} staging plan"));
    let drawer_id = drawer.id.to_string();
    handle.drawers.write().push(drawer);
    assert!(
        handle.identity.is_empty() && handle.l1_drawers.is_empty(),
        "L0/L1 must be empty, or the drawer could surface without the lexical lane"
    );

    // The sweep is the writer. It enumerates the canonical palace only.
    let outcome = run_startup_sweep(&fx.state).await;
    assert!(
        outcome.all_verified() && outcome.swept == 1,
        "precondition: the sweep must verify the canonical palace's coverage; got {outcome:?}"
    );

    let payload = dispatch_tool(
        &fx.state,
        "memory_recall",
        json!({ "palace": fx.alias, "query": token, "top_k": 5 }),
    )
    .await
    .expect("memory_recall through the alias");

    let ids: Vec<String> = payload["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|r| r["drawer_id"].as_str().map(str::to_string))
        .collect();
    assert!(
        ids.contains(&drawer_id),
        "#5036: a recall through alias '{}' must read the corpus the backfill wrote \
         for '{}'. Got {ids:?}",
        fx.alias,
        fx.canonical,
    );

    // And the lane must never have addressed the requested slug: a daemon
    // spawned under the alias name is the bug, even when it happens to be empty.
    assert!(
        !socket_path_for_palace(&fx.alias).exists(),
        "#5036: no BM25 daemon may be started for the alias slug — the lane is \
         keyed on the resolved palace id"
    );

    fx.shutdown().await;
}
