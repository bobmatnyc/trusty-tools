//! Tests for the opt-in OSV.dev lookup (#6780).
//!
//! Why: the leg is fail-open and opt-in, so every arm that produces NOTHING has
//! to be pinned by a test that reads what it said instead. A regression here
//! does not fail a build — it ships a bundle whose empty `osv.json` reads as a
//! dependency set with no known vulnerabilities.
//! What: the ecosystem table, the batch boundary, the three cache states
//! (hit / miss / offline), the 429 retry, the total-failure gap, and the config
//! knob that turns the whole leg on.
//!
//! Nothing here reaches api.osv.dev. Every network arm drives a `wiremock`
//! server on loopback, and the offline arm asserts that server received no
//! request at all.
//!
//! Test: this file.

use super::*;
use crate::grounding::osv_query::{self, Settings};
use crate::grounding::osv_rollup::{INDEX_HEADING, Rollup, TopItem, index_section, rollup};
use std::path::PathBuf;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A `querybatch` response: one package with an advisory, one clean.
const ANSWER: &str = r#"{
  "results": [
    { "vulns": [ {
        "id": "GHSA-xxxx-yyyy-zzzz",
        "summary": "Buffer overflow in the parser",
        "aliases": ["CVE-2026-0001"],
        "database_specific": { "severity": "HIGH" }
    } ] },
    {}
  ]
}"#;

/// Settings pointed at `endpoint`, caching under `dir`, online.
fn settings(endpoint: &str, dir: &Path) -> Settings {
    Settings {
        endpoint: endpoint.to_owned(),
        offline: false,
        cache_dir: dir.join("osv-cache"),
        ttl: osv_query::DEFAULT_TTL,
        time_cap: Duration::from_secs(10),
    }
}

/// The two coordinates [`ANSWER`] answers, in order.
fn pair() -> Vec<Coordinate> {
    vec![
        Coordinate::new("crates.io", "acme-parser", "1.2.3"),
        Coordinate::new("npm", "@acme/web", "4.0.0"),
    ]
}

/// An investigation snapshot carrying `deps` rows and a pre-cap `total`.
fn snapshot_at(dir: &Path, rows: &str, total: usize) -> PathBuf {
    let path = dir.join(INVENTORY_FILE);
    std::fs::write(
        &path,
        format!(r#"{{"repos":[{{"slug":"acme","deps":{{"deps":[{rows}],"total":{total}}}}}]}}"#),
    )
    .expect("write snapshot");
    path
}

/// A manifest with the one `[report]` table the write-back edits.
fn manifest_at(dir: &Path) -> PathBuf {
    let path = dir.join("manifest.toml");
    std::fs::write(&path, "[report]\ntitle = \"Acme\"\n").expect("write manifest");
    path
}

// ─── Ecosystem mapping ──────────────────────────────────────────────────────

/// 🔴 Every label `trusty-review`'s inventory can emit reaches an OSV
/// ecosystem, and the six the issue names map onto OSV's own spellings. A
/// label that stops mapping silently drops that whole language's dependencies
/// from the scan.
#[test]
fn every_inventory_ecosystem_maps_to_an_osv_name() {
    // The four labels `trusty_review::report::investigate::deps` emits today.
    for label in ["cargo", "npm", "pypi", "go"] {
        assert!(
            osv_ecosystem(label).is_some(),
            "the inventory emits {label} and it must map"
        );
    }
    assert_eq!(osv_ecosystem("cargo"), Some("crates.io"));
    assert_eq!(osv_ecosystem("npm"), Some("npm"));
    assert_eq!(osv_ecosystem("pypi"), Some("PyPI"));
    assert_eq!(osv_ecosystem("go"), Some("Go"));
    assert_eq!(osv_ecosystem("maven"), Some("Maven"));
    assert_eq!(osv_ecosystem("rubygems"), Some("RubyGems"));
    assert_eq!(osv_ecosystem("nuget"), Some("NuGet"));
    // Case and padding come from a file another crate writes, not from here.
    assert_eq!(osv_ecosystem("  Cargo "), Some("crates.io"));
    assert_eq!(
        osv_ecosystem("cocoapods"),
        None,
        "an unmapped ecosystem must be reportable rather than silently dropped"
    );
}

/// OSV's qualitative labels land on the report's two-band vocabulary, and an
/// advisory OSV published without one bands RED rather than reading as minor.
#[test]
fn osv_severity_labels_map_onto_report_bands() {
    assert_eq!(Severity::parse("critical"), Severity::Critical);
    assert_eq!(Severity::parse("MEDIUM"), Severity::Moderate);
    assert_eq!(Severity::parse(""), Severity::Unknown);
    assert_eq!(Severity::Critical.band(), cve::Severity::Red);
    assert_eq!(Severity::High.band(), cve::Severity::Red);
    assert_eq!(Severity::Moderate.band(), cve::Severity::Amber);
    assert_eq!(Severity::Low.band(), cve::Severity::Amber);
    assert_eq!(
        Severity::Unknown.band(),
        cve::Severity::Red,
        "an advisory with no stated severity is not evidence that it is minor"
    );
}

// ─── Batching ───────────────────────────────────────────────────────────────

/// 🔴 OSV rejects more than 1000 queries in one call, so the boundary is the
/// difference between a large engagement being scanned and being refused.
#[test]
fn batches_split_at_the_query_cap() {
    let many: Vec<Coordinate> = (0..MAX_QUERIES_PER_BATCH + 1)
        .map(|n| Coordinate::new("crates.io", &format!("crate-{n}"), "1.0.0"))
        .collect();

    let split = batches(&many);
    assert_eq!(split.len(), 2, "1001 coordinates are two calls");
    assert_eq!(split[0].len(), MAX_QUERIES_PER_BATCH);
    assert_eq!(split[1].len(), 1);
    assert_eq!(
        split.iter().map(|b| b.len()).sum::<usize>(),
        many.len(),
        "no coordinate is dropped at the boundary"
    );

    let exact = &many[..MAX_QUERIES_PER_BATCH];
    assert_eq!(batches(exact).len(), 1, "exactly 1000 is one call");
    assert!(batches(&[]).is_empty());
}

/// The request document is one query per coordinate, in order, shaped as OSV
/// documents it — a misspelled key is a batch that silently matches nothing.
#[test]
fn the_request_body_is_one_query_per_coordinate() {
    let body = osv_query::request_body(&pair());
    let queries = body["queries"].as_array().expect("a queries array");
    assert_eq!(queries.len(), 2);
    assert_eq!(queries[0]["package"]["name"], "acme-parser");
    assert_eq!(queries[0]["package"]["ecosystem"], "crates.io");
    assert_eq!(queries[0]["version"], "1.2.3");
    assert_eq!(queries[1]["package"]["ecosystem"], "npm");
}

// ─── Inventory ──────────────────────────────────────────────────────────────

/// The inventory the snapshot carries becomes coordinates; a row with no
/// locked version and a row in an unmapped ecosystem are each NAMED rather
/// than dropped, because both are unassessed rather than clean.
#[test]
fn the_inventory_becomes_osv_coordinates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let snapshot = snapshot_at(
        tmp.path(),
        r#"{"name":"acme-parser","ecosystem":"cargo","spec":"1","locked":"1.2.3"},
           {"name":"loose","ecosystem":"npm","spec":"^4","locked":null},
           {"name":"pods","ecosystem":"cocoapods","spec":"1","locked":"1.0.0"}"#,
        3,
    );

    let inventory = inventory(&snapshot).expect("the snapshot parses");
    assert_eq!(
        inventory.coordinates,
        vec![Coordinate::new("crates.io", "acme-parser", "1.2.3")]
    );
    assert_eq!(inventory.unpinned, vec!["loose (npm)".to_string()]);
    assert_eq!(inventory.unmapped, vec!["pods (cocoapods)".to_string()]);
    assert_eq!(inventory.listed, 3);
    assert_eq!(inventory.declared, 3);
}

/// 🔴 The producer caps its inventory at 30 rows, so a large workspace offers
/// a fraction of itself to OSV. The report must say how much it left out — a
/// scan over 30 of 134 packages that reads as a scan of the repository is the
/// exact false-clean claim this leg exists to remove.
#[test]
fn a_capped_inventory_says_how_much_it_left_out() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let snapshot = snapshot_at(
        tmp.path(),
        r#"{"name":"acme-parser","ecosystem":"cargo","spec":"1","locked":"1.2.3"}"#,
        134,
    );

    let inventory = inventory(&snapshot).expect("the snapshot parses");
    let gaps = coverage_gaps(&inventory, "acme/api");
    let capped = gaps
        .iter()
        .find(|gap| gap.contains("lists 1 of the 134"))
        .expect("the cap is stated");
    assert!(capped.contains("acme/api"), "the gap names the repository");
    assert!(capped.contains("133 were never offered to OSV"));
}

/// A snapshot the child never wrote is a named gap, not a clean scan.
#[test]
fn a_missing_snapshot_is_a_named_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cause = inventory(&tmp.path().join(INVENTORY_FILE)).expect_err("there is no snapshot");
    assert!(cause.contains(COLLECTOR), "the gap names the collector");
    assert!(cause.contains("could not be read"));
}

// ─── Cache: hit, miss, offline ──────────────────────────────────────────────

/// 🔴 A fresh cached answer is used and the endpoint is never called. Without
/// this, a resumed sweep re-queries every dependency of every repository.
#[tokio::test]
async fn a_cached_answer_is_never_fetched_again() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let settings = settings(&format!("{}/v1/querybatch", server.uri()), tmp.path());
    let coordinate = Coordinate::new("crates.io", "acme-parser", "1.2.3");
    let cached = Vuln {
        id: "GHSA-cached".to_owned(),
        aliases: vec![],
        summary: "from the cache".to_owned(),
        severity: Severity::Low,
    };
    osv_query::seed_cache(
        &settings,
        &coordinate,
        std::slice::from_ref(&cached),
        Duration::ZERO,
    );

    let (answers, errors) = osv_query::resolve(&settings, &[coordinate]).await;

    assert_eq!(answers, vec![Some(vec![cached])]);
    assert!(errors.is_empty(), "a cache hit is not a degradation");
    assert!(
        server
            .received_requests()
            .await
            .expect("recording")
            .is_empty(),
        "a cache hit must not reach the endpoint"
    );
}

/// An entry older than the TTL is a miss: an advisory database gains rows, so
/// a stale "no advisories" is exactly the answer worth expiring.
#[tokio::test]
async fn a_stale_cache_entry_is_refetched() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/querybatch"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ANSWER))
        .expect(1)
        .mount(&server)
        .await;
    let mut settings = settings(&format!("{}/v1/querybatch", server.uri()), tmp.path());
    settings.ttl = Duration::from_secs(60);
    let coordinates = pair();
    osv_query::seed_cache(&settings, &coordinates[0], &[], Duration::from_secs(3_600));

    let (answers, errors) = osv_query::resolve(&settings, &coordinates).await;

    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    let first = answers[0].as_ref().expect("the stale entry was refetched");
    assert_eq!(
        first.len(),
        1,
        "the endpoint's answer replaced the stale one"
    );
    assert_eq!(first[0].id, "GHSA-xxxx-yyyy-zzzz");
}

/// 🔴 Offline mode answers from the cache alone and opens no socket. The
/// assertion is on the server's request count rather than on a timeout,
/// because an air-gapped machine is where this mode is used and a request that
/// merely fails there is still a request that should not have been made.
#[tokio::test]
async fn an_offline_miss_is_a_named_cache_miss() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let mut settings = settings(&format!("{}/v1/querybatch", server.uri()), tmp.path());
    settings.offline = true;
    let coordinates = pair();
    let hit = Vuln {
        id: "GHSA-cached".to_owned(),
        aliases: vec![],
        summary: String::new(),
        severity: Severity::Moderate,
    };
    osv_query::seed_cache(
        &settings,
        &coordinates[0],
        std::slice::from_ref(&hit),
        Duration::ZERO,
    );

    let (answers, errors) = osv_query::resolve(&settings, &coordinates).await;

    assert_eq!(answers[0], Some(vec![hit]), "the cached half still answers");
    assert_eq!(answers[1], None, "the uncached half is unanswered");
    assert_eq!(errors.len(), 1, "one line covers the misses: {errors:?}");
    assert!(errors[0].contains("cache miss"), "{}", errors[0]);
    assert!(errors[0].contains("@acme/web@4.0.0"), "{}", errors[0]);
    assert!(
        server
            .received_requests()
            .await
            .expect("recording")
            .is_empty(),
        "offline mode must open no socket at all"
    );
}

/// A fetched answer is written to the cache, under the work directory the
/// engagement owns — deleting that directory deletes the cache with it.
#[tokio::test]
async fn a_fetched_answer_is_cached_under_the_work_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ANSWER))
        .mount(&server)
        .await;
    let settings = settings(&format!("{}/v1/querybatch", server.uri()), tmp.path());
    let coordinates = pair();

    osv_query::resolve(&settings, &coordinates).await;

    let path = osv_query::cached_path(&settings, &coordinates[0]);
    assert!(
        path.is_file(),
        "the answer was cached at {}",
        path.display()
    );
    assert!(
        path.starts_with(&settings.cache_dir),
        "the cache stays inside the working directory"
    );
}

// ─── Retry ──────────────────────────────────────────────────────────────────

/// 🔴 A 429 is retried rather than dropped. OSV rate-limits, and a sweep asking
/// about several repositories in a row is exactly the shape that trips it —
/// dropping the batch would leave a whole repository unassessed over a
/// condition that clears in a quarter of a second.
#[tokio::test]
async fn a_rate_limited_batch_is_retried_and_then_answers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ANSWER))
        .expect(1)
        .mount(&server)
        .await;
    let settings = settings(&format!("{}/v1/querybatch", server.uri()), tmp.path());

    let (answers, errors) = osv_query::resolve(&settings, &pair()).await;

    assert!(errors.is_empty(), "the retry succeeded: {errors:?}");
    assert_eq!(
        answers[0].as_ref().map(Vec::len),
        Some(1),
        "the second attempt's answer is the one that lands"
    );
    assert_eq!(answers[1], Some(vec![]), "a clean package answers empty");
}

/// A 4xx that is not 429 is not retried: it will not become a different answer,
/// and retrying it spends the repository's time cap for nothing.
#[tokio::test]
async fn a_client_error_is_not_retried() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400))
        .expect(1)
        .mount(&server)
        .await;
    let settings = settings(&format!("{}/v1/querybatch", server.uri()), tmp.path());

    let (answers, errors) = osv_query::resolve(&settings, &pair()).await;

    assert!(answers.iter().all(Option::is_none));
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("400"), "{}", errors[0]);
}

/// `querybatch` is an index over ids and may answer with the id alone. That is
/// still a vulnerability and must reach the report as one.
#[test]
fn an_id_only_answer_is_still_a_vulnerability() {
    let parsed = osv_query::parse(r#"{"results":[{"vulns":[{"id":"GHSA-bare"}]}]}"#)
        .expect("the body parses");
    assert_eq!(parsed[0][0].id, "GHSA-bare");
    assert_eq!(parsed[0][0].severity, Severity::Unknown);
    assert_eq!(
        parsed[0][0].title(),
        "OSV returned this advisory id with no summary",
        "an empty summary is stated rather than rendered as a blank cell"
    );
    assert!(osv_query::parse("not json").is_err());
    assert!(osv_query::parse(r#"{"ok":true}"#).is_err());
}

// ─── The whole leg ──────────────────────────────────────────────────────────

/// The end-to-end happy path: the bundle carries `osv.json` in the documented
/// shape, and the manifest carries one findings row per advisory.
#[tokio::test]
async fn the_scan_lands_in_the_bundle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ANSWER))
        .mount(&server)
        .await;
    let settings = settings(&format!("{}/v1/querybatch", server.uri()), tmp.path());
    snapshot_at(
        tmp.path(),
        r#"{"name":"acme-parser","ecosystem":"cargo","spec":"1","locked":"1.2.3"},
           {"name":"@acme/web","ecosystem":"npm","spec":"4","locked":"4.0.0"}"#,
        2,
    );
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into(&manifest, &settings, "acme/api").await;

    let scan: Scan = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(SCAN_FILE)).expect("osv.json was written"),
    )
    .expect("osv.json is the documented shape");
    assert_eq!(scan.queried, 2);
    assert_eq!(scan.matched, 1);
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    assert_eq!(scan.packages.len(), 1);
    assert_eq!(scan.packages[0].package, "acme-parser");
    assert_eq!(scan.packages[0].ecosystem, "crates.io");
    assert_eq!(scan.packages[0].version, "1.2.3");
    assert_eq!(scan.packages[0].vulns[0].aliases, vec!["CVE-2026-0001"]);
    assert_eq!(scan.packages[0].vulns[0].severity, Severity::High);
    assert!(
        gaps.is_empty(),
        "a scan that matched needs no gap: {gaps:?}"
    );

    let written = std::fs::read_to_string(&manifest).expect("the manifest is readable");
    assert!(written.contains(CATEGORY), "the row carries the heading");
    assert!(written.contains("GHSA-xxxx-yyyy-zzzz"));
    assert!(
        written.contains("severity = \"RED\""),
        "HIGH bands RED for the report's vocabulary: {written}"
    );
}

/// 🔴 A repository whose every batch failed has NO OSV coverage, and the
/// report must say so rather than shipping an empty scan that reads as clean.
/// This is the 409 lesson: a degradation is never a silent zero.
#[tokio::test]
async fn a_repository_whose_every_batch_fails_records_a_gap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let mut settings = settings(&format!("{}/v1/querybatch", server.uri()), tmp.path());
    settings.time_cap = Duration::from_secs(5);
    snapshot_at(
        tmp.path(),
        r#"{"name":"acme-parser","ecosystem":"cargo","spec":"1","locked":"1.2.3"}"#,
        1,
    );
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into(&manifest, &settings, "acme/api").await;

    let stated = gaps
        .iter()
        .find(|gap| gap.contains("no OSV coverage at all"))
        .unwrap_or_else(|| panic!("the total failure is stated: {gaps:?}"));
    assert!(stated.contains("acme/api"), "the gap names the repository");
    assert!(stated.contains("unassessed rather than clean"), "{stated}");

    let scan: Scan = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(SCAN_FILE)).expect("osv.json was still written"),
    )
    .expect("osv.json parses");
    assert_eq!(scan.queried, 0);
    assert!(!scan.errors.is_empty(), "the failure is in the bundle too");
}

/// A scan that ran and matched nothing states its own scope, because "OSV
/// found no advisory" and "no OSV query ran" must not read the same.
#[tokio::test]
async fn a_clean_scan_states_its_own_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"results":[{}]}"#))
        .mount(&server)
        .await;
    let settings = settings(&format!("{}/v1/querybatch", server.uri()), tmp.path());
    snapshot_at(
        tmp.path(),
        r#"{"name":"acme-parser","ecosystem":"cargo","spec":"1","locked":"1.2.3"}"#,
        1,
    );
    let manifest = manifest_at(tmp.path());

    let gaps = ground_into(&manifest, &settings, "acme/api").await;

    assert!(
        gaps.iter().any(|gap| gap.contains("returned no advisory")),
        "a clean scan states its scope: {gaps:?}"
    );
}

// ─── The run index ──────────────────────────────────────────────────────────

/// The roll-up reads every repository's `osv.json` back off disk, so the index
/// states what the bundle carries rather than what this process believed.
#[test]
fn the_rollup_counts_every_repository() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dirs: Vec<PathBuf> = ["00-acme-api", "01-acme-web"]
        .iter()
        .map(|name| {
            let dir = tmp.path().join(name);
            std::fs::create_dir_all(&dir).expect("mkdir");
            dir
        })
        .collect();
    let scan = |id: &str, severity: Severity| Scan {
        queried: 2,
        matched: 1,
        errors: vec!["one batch went unanswered".to_owned()],
        packages: vec![PackageVulns {
            package: "acme-parser".to_owned(),
            ecosystem: "crates.io".to_owned(),
            version: "1.2.3".to_owned(),
            vulns: vec![Vuln {
                id: id.to_owned(),
                aliases: vec![],
                summary: "Buffer overflow".to_owned(),
                severity,
            }],
        }],
    };
    write_scan(&dirs[0].join(SCAN_FILE), &scan("GHSA-a", Severity::Low)).expect("write");
    write_scan(
        &dirs[1].join(SCAN_FILE),
        &scan("GHSA-b", Severity::Critical),
    )
    .expect("write");

    let rollup = rollup(&dirs);

    assert_eq!(rollup.repos, 2);
    assert_eq!(rollup.queried, 4);
    assert_eq!(rollup.matched, 2);
    assert_eq!(rollup.errors, 2);
    assert_eq!(rollup.advisories(), 2);
    assert_eq!(rollup.counts.get(&Severity::Critical), Some(&1));
    assert_eq!(rollup.counts.get(&Severity::Low), Some(&1));
    assert_eq!(
        rollup.top.first().map(|item| item.id.as_str()),
        Some("GHSA-b"),
        "the worst advisory leads"
    );
    assert_eq!(rollup.top[0].repo, "01-acme-web");
}

/// A directory with no scan contributes nothing and is not an error here — the
/// repository already stated itself in the run's gap list.
#[test]
fn a_directory_with_no_scan_is_not_counted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(rollup(&[tmp.path().to_path_buf()]).is_empty());
}

/// 🔴 A run with the collector switched off says so. Before this, that run was
/// indistinguishable from one that scanned and matched nothing.
#[test]
fn a_disabled_collector_says_so_in_the_index() {
    let section = index_section(Some(&Rollup::default()));
    assert!(section.contains(INDEX_HEADING));
    assert!(section.contains("Not run (opt-in)"), "{section}");
    assert!(section.contains("[collectors]"), "{section}");
    assert!(
        section.contains("unassessed rather than clean"),
        "the line refuses to read as a clean bill: {section}"
    );
    assert_eq!(
        index_section(None),
        "",
        "a producer with no scan data claims neither way"
    );
}

/// The index carries a count per severity and the worst rows beneath it.
#[test]
fn the_index_section_counts_by_severity() {
    let mut rollup = Rollup {
        repos: 1,
        queried: 30,
        matched: 1,
        errors: 0,
        ..Rollup::default()
    };
    rollup.counts.insert(Severity::Critical, 2);
    rollup.top.push(TopItem {
        repo: "00-acme-api".to_owned(),
        id: "GHSA-a".to_owned(),
        package: "acme-parser".to_owned(),
        version: "1.2.3".to_owned(),
        severity: Severity::Critical,
        title: "Buffer overflow".to_owned(),
    });

    let section = index_section(Some(&rollup));

    assert!(section.contains("| Severity | Advisories |"), "{section}");
    assert!(section.contains("| CRITICAL | 2 |"), "{section}");
    assert!(section.contains("| LOW | 0 |"), "every band is stated");
    assert!(section.contains("| **Total** | **2** |"), "{section}");
    assert!(section.contains("30 pinned package(s)"), "{section}");
    assert!(section.contains("GHSA-a"), "the top items are listed");
}

// ─── The config knob ────────────────────────────────────────────────────────

/// 🔴 The whole leg is reachable only through `[collectors] osv`, and an
/// engagement that declares nothing must be unchanged. This is the test that
/// fails on `main`, where neither table exists.
#[test]
fn an_engagement_can_turn_the_collector_on() {
    let base = "openrouter_key = \"k\"\ninstructions = \"assess\"\n\n[tools]\ntga = \"6.0.0\"\n\
                trusty-search = \"0.52.0\"\ntrusty-analyze = \"0.12.5\"\n\
                trusty-review = \"0.33.0\"\n";

    let off: crate::config::EngagementConfig = toml::from_str(base).expect("the base config loads");
    assert!(
        !off.collectors.osv,
        "an engagement that declares nothing runs no OSV lookup"
    );
    assert!(!off.osv.offline);

    let on: crate::config::EngagementConfig = toml::from_str(&format!(
        "{base}\n[collectors]\nosv = true\n\n[osv]\noffline = true\ncache_ttl_hours = 12\n\
         time_cap_secs = 30\n"
    ))
    .expect("the declared config loads");
    assert!(on.collectors.osv, "the knob turns the leg on");
    assert!(on.osv.offline);
    assert_eq!(on.osv.cache_ttl_hours, Some(12));
    assert_eq!(on.osv.time_cap_secs, Some(30));
}

/// The declared budgets reach the transport, and an absent or zero one falls
/// back rather than reading as a request for a zero-second time cap.
#[test]
fn the_declared_budgets_reach_the_transport() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = crate::workdir::WorkDir::new(tmp.path().to_path_buf());
    let declared = |ttl, cap| crate::config::OsvSettings {
        offline: false,
        cache_ttl_hours: ttl,
        time_cap_secs: cap,
        endpoint: None,
    };

    let settings = Settings::for_engagement(&declared(Some(12), Some(30)), &work);
    assert_eq!(settings.ttl, Duration::from_secs(12 * 60 * 60));
    assert_eq!(settings.time_cap, Duration::from_secs(30));
    assert_eq!(settings.endpoint, osv_query::DEFAULT_ENDPOINT);
    assert!(
        settings
            .cache_dir
            .starts_with(work.path(crate::workdir::Area::State)),
        "the cache lives inside the directory this client owns"
    );

    let fallback = Settings::for_engagement(&declared(Some(0), None), &work);
    assert_eq!(fallback.ttl, osv_query::DEFAULT_TTL);
    assert_eq!(fallback.time_cap, osv_query::DEFAULT_TIME_CAP);
}
