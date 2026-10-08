//! Known-answer recall eval: the trusty-memory release gate (#9281).
//!
//! Why: recall ranking has regressed silently before (#8246, #9142): a stale
//! status snapshot outranked the ruling that replaced it, and no gate noticed.
//! A fixed corpus with known answers, scored by the real model, turns "recall
//! got worse" into a red pre-publish job.
//! What: loads `testdata/recall_eval/corpus.json` (32 queries, 12 groups that
//! each pair a CURRENT drawer with a SUPERSEDED one) into a temp palace, recalls
//! every query, and logs each expected drawer's rank. The gate fails when hit@1
//! drops below `baseline.json`, when any superseded drawer ranks above its
//! current one, when the baseline or fingerprint file is missing or corrupt,
//! or when the embedder is unavailable or is not the pinned model. It never
//! skips and never defaults.
//! Test: the two `#[ignore]`d real-model tests run in the pre-publish lane
//! (`scripts/prepublish-ignored-tests.tsv`); the rest run in the default suite
//! and never touch the process-global embedder.

mod recall_support;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, ensure, Context, Result};
use chrono::Duration;
use recall_support::{backdate, create_palaces, fact_key, rank_of, recall, remember, supersede};
use serde::Deserialize;
use tempfile::TempDir;
use trusty_common::memory_core::embed::Embedder;
use trusty_memory::AppState;
use uuid::Uuid;

/// Smallest corpus the gate accepts (#9281 acceptance criterion 1).
const MIN_QUERIES: usize = 26;
/// Supersession groups the corpus must carry (#9281; #9421 added two).
const GROUPS: usize = 12;
/// Superseded-above-current ceiling. Pinned in code, not in the baseline file.
const SUPERSEDED_PIN: usize = 0;
/// Hits each query recalls.
const TOP_K: u64 = 10;
/// Tags production demotion treats as a snapshot (`tools::recall_rank`).
const SNAPSHOT_TAGS: &[&str] = &["status", "resume-target", "snapshot", "session-snapshot"];

type SharedEmbedder = Arc<dyn Embedder + Send + Sync>;

fn testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/recall_eval")
        .join(name)
}

/// How a group's superseded drawer is made stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mechanism {
    /// An unkeyed snapshot, demoted by age (#8246).
    Demotion,
    /// A Tier C slot the current drawer retires on write (ADR-0028 D6).
    FactKey,
    /// #9421: an untagged, unkeyed drawer linked to its replacement by a
    /// `superseded_by` KG edge.
    SupersededBy,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusDrawer {
    id: String,
    text: String,
    tags: Vec<String>,
    #[serde(default)]
    fact_key: Option<String>,
    #[serde(default)]
    age_days: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    id: String,
    text: String,
    expected: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Group {
    id: String,
    query: String,
    current: String,
    superseded: String,
    mechanism: Mechanism,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    #[serde(default)]
    #[allow(dead_code)] // documentation for the reader of the JSON
    about: String,
    palace: String,
    drawers: Vec<CorpusDrawer>,
    queries: Vec<Query>,
    groups: Vec<Group>,
}

impl Corpus {
    /// Read and validate the corpus; any defect is an error.
    fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("recall-eval corpus {} unreadable", path.display()))?;
        let corpus: Corpus = serde_json::from_str(&raw)
            .with_context(|| format!("recall-eval corpus {} unparseable", path.display()))?;
        corpus.validate()?;
        Ok(corpus)
    }

    fn drawer(&self, id: &str) -> Result<(usize, &CorpusDrawer)> {
        self.drawers
            .iter()
            .enumerate()
            .find(|(_, d)| d.id == id)
            .ok_or_else(|| anyhow!("corpus names unknown drawer {id}"))
    }

    /// Why: a corpus whose groups cannot engage a mechanism measures nothing.
    /// What: size floors, unique ids, resolvable references, and per-group
    /// shape: a `fact_key` group shares one slot, written superseded first; a
    /// `demotion` group's superseded drawer is an aged, unkeyed snapshot; a
    /// `superseded_by` group is unkeyed, carries no snapshot tag, and is
    /// written superseded first.
    /// Test: `the_shipped_files_validate`, `a_short_corpus_fails_validation`.
    fn validate(&self) -> Result<()> {
        ensure!(
            self.queries.len() >= MIN_QUERIES,
            "corpus has {} queries; the gate needs at least {MIN_QUERIES}",
            self.queries.len()
        );
        ensure!(
            self.groups.len() == GROUPS,
            "corpus has {} groups; the gate needs exactly {GROUPS}",
            self.groups.len()
        );
        let ids: HashSet<&str> = self.drawers.iter().map(|d| d.id.as_str()).collect();
        ensure!(ids.len() == self.drawers.len(), "duplicate drawer id");
        let qids: HashSet<&str> = self.queries.iter().map(|q| q.id.as_str()).collect();
        ensure!(qids.len() == self.queries.len(), "duplicate query id");
        for q in &self.queries {
            self.drawer(&q.expected)?;
        }
        for g in &self.groups {
            let q = self
                .queries
                .iter()
                .find(|q| q.id == g.query)
                .ok_or_else(|| anyhow!("{}: unknown query {}", g.id, g.query))?;
            ensure!(
                q.expected == g.current,
                "{}: query must expect current",
                g.id
            );
            let (ci, cur) = self.drawer(&g.current)?;
            let (si, sup) = self.drawer(&g.superseded)?;
            ensure!(sup.age_days > 0, "{}: superseded drawer must be aged", g.id);
            match g.mechanism {
                Mechanism::FactKey => {
                    ensure!(
                        sup.fact_key.is_some() && sup.fact_key == cur.fact_key,
                        "{}: both drawers must share one fact_key",
                        g.id
                    );
                    ensure!(si < ci, "{}: superseded must be written first", g.id);
                }
                Mechanism::Demotion => {
                    ensure!(
                        sup.fact_key.is_none() && cur.fact_key.is_none(),
                        "{}: a demotion group is unkeyed",
                        g.id
                    );
                    ensure!(
                        sup.tags.iter().any(|t| SNAPSHOT_TAGS.contains(&t.as_str())),
                        "{}: superseded drawer needs a snapshot tag",
                        g.id
                    );
                }
                Mechanism::SupersededBy => {
                    // #9421: only the edge may demote it — no tag, no slot.
                    ensure!(
                        sup.fact_key.is_none() && cur.fact_key.is_none(),
                        "{}: a superseded_by group is unkeyed",
                        g.id
                    );
                    ensure!(
                        !sup.tags.iter().any(|t| SNAPSHOT_TAGS.contains(&t.as_str())),
                        "{}: a superseded_by drawer carries no snapshot tag",
                        g.id
                    );
                    ensure!(si < ci, "{}: superseded must be written first", g.id);
                }
            }
        }
        for m in [
            Mechanism::Demotion,
            Mechanism::FactKey,
            Mechanism::SupersededBy,
        ] {
            ensure!(
                self.groups.iter().any(|g| g.mechanism == m),
                "corpus has no {m:?} group"
            );
        }
        Ok(())
    }
}

/// The recorded gate floor (`testdata/recall_eval/baseline.json`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    #[serde(default)]
    #[allow(dead_code)] // provenance for the reader of the JSON
    about: String,
    model: String,
    queries: usize,
    hit_at_1: usize,
    groups: usize,
    superseded_above_current: usize,
}

/// Load the baseline, failing closed.
///
/// Why (#9281 criterion 2): a gate that reads a missing floor as zero passes
/// every regression. What: a missing, unreadable, unparseable or
/// unknown-field file is an error, and so is one with no queries, a zero
/// hit@1 floor, a hit@1 above its query count, or a superseded count other
/// than the code pin.
/// Test: `a_missing_baseline_fails_the_gate`, `a_corrupt_baseline_fails_the_gate`,
/// `a_zero_floor_never_reaches_the_gate`.
fn load_baseline(path: &Path) -> Result<Baseline> {
    // See #9281: fail closed — never a default baseline.
    let raw = std::fs::read_to_string(path).with_context(|| {
        format!(
            "recall-eval baseline {} is missing or unreadable — the gate never defaults",
            path.display()
        )
    })?;
    let b: Baseline = serde_json::from_str(&raw)
        .with_context(|| format!("recall-eval baseline {} is unparseable", path.display()))?;
    ensure!(
        b.queries > 0 && b.groups > 0,
        "baseline records no queries or groups"
    );
    // #9281: a zero floor is `Baseline::default()`'s, the red-proof's floor;
    // it passes every hit@1 regression, so the shipped gate refuses it.
    ensure!(
        b.hit_at_1 > 0,
        "baseline hit@1 floor is zero — a zero floor passes every regression"
    );
    ensure!(
        b.hit_at_1 <= b.queries,
        "baseline hit@1 {} exceeds its {} queries",
        b.hit_at_1,
        b.queries
    );
    ensure!(
        b.superseded_above_current == SUPERSEDED_PIN,
        "baseline superseded-above-current {} is not the pinned {SUPERSEDED_PIN}",
        b.superseded_above_current
    );
    Ok(b)
}

/// The pinned model's reference embedding (`model_fingerprint.json`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    model: String,
    #[allow(dead_code)] // provenance for the reader of the JSON
    source: String,
    min_cosine: f64,
    text: String,
    vector: Vec<f32>,
}

impl Fingerprint {
    fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("model fingerprint {} unreadable", path.display()))?;
        let fp: Fingerprint = serde_json::from_str(&raw)
            .with_context(|| format!("model fingerprint {} unparseable", path.display()))?;
        ensure!(!fp.vector.is_empty(), "model fingerprint has no vector");
        ensure!(
            fp.min_cosine > 0.0 && fp.min_cosine <= 1.0,
            "model fingerprint min_cosine out of range"
        );
        Ok(fp)
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum();
    let na: f64 = a.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

/// Require the pinned model behind the process-wide embedder.
///
/// Why (#9281 criterion 3): with no embedder, recall silently serves the
/// L0/L1 fallback, and a mock or int8 model scores a different gate. Neither
/// may pass or skip. What: an `Err` is "unavailable"; otherwise the
/// fingerprint text is embedded and its cosine against the recorded
/// sentence-transformers vector must reach `min_cosine` (0.999; int8 scores
/// about 0.99, the hash mock near 0).
/// Test: `an_unavailable_embedder_fails_the_gate`, `a_mock_embedder_fails_the_model_pin`.
async fn require_pinned_model(embedder: Result<SharedEmbedder>, fp: &Fingerprint) -> Result<()> {
    // See #9281: fail closed — an unavailable embedder never skips the gate.
    let e = embedder
        .context("recall-eval: the embedder is unavailable — the gate fails, never skips")?;
    let mut v = e
        .embed_batch(std::slice::from_ref(&fp.text))
        .await
        .context("recall-eval: the embedder is unavailable (fingerprint embed failed)")?;
    let v = v
        .pop()
        .context("recall-eval: the embedder returned no vector")?;
    ensure!(
        v.len() == fp.vector.len(),
        "embedder is not the pinned {}: dimension {} != {}",
        fp.model,
        v.len(),
        fp.vector.len()
    );
    let cos = cosine(&v, &fp.vector);
    ensure!(
        cos >= fp.min_cosine,
        "embedder is not the pinned {}: fingerprint cosine {cos:.6} < {}",
        fp.model,
        fp.min_cosine
    );
    Ok(())
}

/// Which staleness mechanisms the corpus load leaves engaged.
///
/// Why (#9281 criterion 5): the gate must go red when any mechanism is
/// reverted. Production has no switch for any, so the load neutralises each
/// one's input: demotion's is a drawer's age, retirement's is a shared slot,
/// supersession's (#9421) is the `superseded_by` edge.
/// What: `demotion: false` skips backdating, so every snapshot is age zero and
/// `temporal_weight` is 1.0. `retirement: false` moves each superseded drawer
/// to its own slot (`<key>-prior`), so the current write retires nothing and
/// the old drawer stays a live Tier C fact, exempt from demotion. Since
/// #9433 a retirement also writes the `superseded_by` edge, so a FactKey
/// group needs neither age nor a hand-written edge to rank correctly.
/// `supersession: false` writes no hand-written edge.
/// Test: `recall_eval_goes_red_with_demotion_or_retirement_off`.
#[derive(Debug, Clone, Copy)]
struct Mechanisms {
    demotion: bool,
    retirement: bool,
    supersession: bool,
}

impl Mechanisms {
    const ON: Self = Self {
        demotion: true,
        retirement: true,
        supersession: true,
    };
}

/// One query's outcome.
#[derive(Debug, Clone)]
struct QueryOutcome {
    id: String,
    expected: String,
    rank: Option<usize>,
    top: Option<String>,
}

/// One group's outcome.
#[derive(Debug, Clone)]
struct GroupOutcome {
    id: String,
    mechanism: Mechanism,
    current: Option<usize>,
    superseded: Option<usize>,
    /// Recall scores of (current, superseded), logged so margins are visible.
    scores: [Option<f64>; 2],
}

impl GroupOutcome {
    /// The group fails: the superseded drawer outranks the current one, or the
    /// current drawer was not recalled at all.
    /// Test: `an_unrecalled_drawer_counts_against_the_gate`.
    fn superseded_above(&self) -> bool {
        match (self.current, self.superseded) {
            (Some(c), Some(s)) => s < c,
            (Some(_), None) => false,
            // #9281: a missing `rank_of` for the current drawer is a failure,
            // never a skip — a group that recalls neither drawer proves nothing.
            (None, _) => true,
        }
    }
}

#[derive(Debug, Clone)]
struct Report {
    queries: Vec<QueryOutcome>,
    groups: Vec<GroupOutcome>,
}

impl Report {
    fn hit_at_1(&self) -> usize {
        self.queries.iter().filter(|q| q.rank == Some(0)).count()
    }

    fn superseded_above(&self) -> Vec<&GroupOutcome> {
        self.groups
            .iter()
            .filter(|g| g.superseded_above())
            .collect()
    }

    fn summary(&self) -> String {
        let flipped: Vec<&str> = self
            .superseded_above()
            .iter()
            .map(|g| g.id.as_str())
            .collect();
        format!(
            "hit@1 {}/{}; superseded-above-current {}/{} {flipped:?}",
            self.hit_at_1(),
            self.queries.len(),
            flipped.len(),
            self.groups.len()
        )
    }

    /// Log every query's rank and every group's pair of ranks.
    fn log(&self, label: &str) {
        for q in &self.queries {
            eprintln!(
                "recall-eval [{label}] {} rank={:?} expected={} top={:?}",
                q.id, q.rank, q.expected, q.top
            );
        }
        for g in &self.groups {
            eprintln!(
                "recall-eval [{label}] {} {:?} current={:?} superseded={:?} scores={:?}{}",
                g.id,
                g.mechanism,
                g.current,
                g.superseded,
                g.scores,
                if g.superseded_above() {
                    " SUPERSEDED-ABOVE"
                } else {
                    ""
                }
            );
        }
        eprintln!("recall-eval [{label}] {}", self.summary());
    }
}

/// Why: the gate's two thresholds, with the baseline checked against the
/// corpus it claims to describe.
/// What: fails when the baseline was recorded for another corpus size or
/// model, when hit@1 falls below it, or when any superseded drawer outranks
/// its current one. Every failure is listed.
/// Test: `the_verdict_fails_below_baseline_or_on_any_superseded_hit`.
fn verdict(report: &Report, baseline: &Baseline, model: &str) -> Result<()> {
    let mut failures = Vec::new();
    if baseline.model != model {
        failures.push(format!(
            "baseline measured with {}, gate pinned to {model}",
            baseline.model
        ));
    }
    if baseline.queries != report.queries.len() || baseline.groups != report.groups.len() {
        failures.push(format!(
            "baseline describes {} queries / {} groups, corpus has {} / {} — re-measure",
            baseline.queries,
            baseline.groups,
            report.queries.len(),
            report.groups.len()
        ));
    }
    if report.hit_at_1() < baseline.hit_at_1 {
        failures.push(format!(
            "hit@1 {} fell below the baseline {}",
            report.hit_at_1(),
            baseline.hit_at_1
        ));
    }
    let flipped = report.superseded_above();
    if flipped.len() > SUPERSEDED_PIN {
        let ids: Vec<&str> = flipped.iter().map(|g| g.id.as_str()).collect();
        failures.push(format!(
            "superseded-above-current {}/{} {ids:?}, pinned at {SUPERSEDED_PIN}",
            flipped.len(),
            report.groups.len()
        ));
    }
    if failures.is_empty() {
        return Ok(());
    }
    bail!("recall-eval gate failed: {}", failures.join("; "))
}

/// Write the corpus into a fresh palace on `state`; returns corpus id -> drawer id.
async fn load_corpus(
    state: &AppState,
    tmp: &TempDir,
    corpus: &Corpus,
    mech: Mechanisms,
) -> Result<HashMap<String, Uuid>> {
    let palace = corpus.palace.as_str();
    create_palaces(state, tmp, &[palace]).await;
    let superseded: HashSet<&str> = corpus
        .groups
        .iter()
        .map(|g| g.superseded.as_str())
        .collect();
    let mut ids = HashMap::new();
    for d in &corpus.drawers {
        let key = match &d.fact_key {
            Some(k) if !mech.retirement && superseded.contains(d.id.as_str()) => {
                Some(format!("{k}-prior"))
            }
            other => other.clone(),
        };
        let tags: Vec<&str> = d.tags.iter().map(String::as_str).collect();
        let id = remember(state, palace, &d.text, &tags, key.as_deref()).await;
        ids.insert(d.id.clone(), id);
    }
    if mech.demotion {
        for d in corpus.drawers.iter().filter(|d| d.age_days > 0) {
            backdate(state, palace, ids[&d.id], Duration::days(d.age_days));
        }
    }
    if mech.supersession {
        for g in corpus
            .groups
            .iter()
            .filter(|g| g.mechanism == Mechanism::SupersededBy)
        {
            supersede(state, palace, ids[&g.superseded], ids[&g.current]).await;
        }
    }
    // The seam must do what it claims, or the run measures nothing.
    for g in corpus
        .groups
        .iter()
        .filter(|g| g.mechanism == Mechanism::FactKey)
    {
        let retired = fact_key(state, palace, ids[&g.superseded]).is_none();
        ensure!(
            retired == mech.retirement,
            "{}: superseded slot retired={retired}, expected {}",
            g.id,
            mech.retirement
        );
        ensure!(
            fact_key(state, palace, ids[&g.current]).is_some(),
            "{}: current drawer lost its slot",
            g.id
        );
    }
    Ok(ids)
}

/// Recall every query and score it.
async fn measure(state: &AppState, corpus: &Corpus, ids: &HashMap<String, Uuid>) -> Report {
    let by_uuid: HashMap<String, &str> = ids
        .iter()
        .map(|(k, v)| (v.to_string(), k.as_str()))
        .collect();
    let mut ranks: HashMap<&str, Vec<serde_json::Value>> = HashMap::new();
    let mut queries = Vec::new();
    for q in &corpus.queries {
        let results = recall(state, &corpus.palace, &q.text, TOP_K).await;
        let top = results
            .first()
            .and_then(|r| r["drawer_id"].as_str().or_else(|| r["id"].as_str()))
            .and_then(|u| by_uuid.get(u))
            .map(|s| s.to_string());
        queries.push(QueryOutcome {
            id: q.id.clone(),
            expected: q.expected.clone(),
            rank: rank_of(&results, ids[&q.expected]),
            top,
        });
        ranks.insert(q.id.as_str(), results);
    }
    let groups = corpus
        .groups
        .iter()
        .map(|g| {
            let results = &ranks[g.query.as_str()];
            let (cur, sup) = (ids[&g.current], ids[&g.superseded]);
            let score = |id| rank_of(results, id).and_then(|r| results[r]["score"].as_f64());
            GroupOutcome {
                id: g.id.clone(),
                mechanism: g.mechanism,
                current: rank_of(results, cur),
                superseded: rank_of(results, sup),
                scores: [score(cur), score(sup)],
            }
        })
        .collect();
    Report { queries, groups }
}

/// Run the corpus once on a fresh real-model state and log the ranks.
///
/// Why: both real-model tests need the same pinned-model check, load and score.
/// What: a fresh `AppState` with no rulings palaces and no mock seeding; the
/// embedder must resolve and match the fingerprint before any recall, so a
/// cold or wrong embedder fails here instead of serving the degraded lane.
/// Test: `recall_eval_holds_the_recorded_baseline`,
/// `recall_eval_goes_red_with_demotion_or_retirement_off`.
async fn run_real(corpus: &Corpus, fp: &Fingerprint, mech: Mechanisms, label: &str) -> Report {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf()).with_rulings_palaces(Vec::new());
    require_pinned_model(state.embedder().await, fp)
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    let ids = load_corpus(&state, &tmp, corpus, mech)
        .await
        .unwrap_or_else(|e| panic!("{label}: {e:#}"));
    let report = measure(&state, corpus, &ids).await;
    report.log(label);
    report
}

fn shipped() -> (Corpus, Fingerprint) {
    let corpus = Corpus::load(&testdata("corpus.json")).unwrap_or_else(|e| panic!("{e:#}"));
    let fp =
        Fingerprint::load(&testdata("model_fingerprint.json")).unwrap_or_else(|e| panic!("{e:#}"));
    (corpus, fp)
}

/// Why (#9281): the release gate. What: the shipped corpus, mechanisms on,
/// scored against `baseline.json`. Measures before loading the baseline, so
/// a missing baseline still fails but prints the value to record.
#[tokio::test]
#[ignore = "loads the real all-MiniLM-L6-v2 model; pre-publish ignored-tests lane (#9281)"]
async fn recall_eval_holds_the_recorded_baseline() {
    let (corpus, fp) = shipped();
    let report = run_real(&corpus, &fp, Mechanisms::ON, "mechanisms on").await;
    let baseline = load_baseline(&testdata("baseline.json"))
        .unwrap_or_else(|e| panic!("{e:#}; measured {}", report.summary()));
    verdict(&report, &baseline, &fp.model).unwrap_or_else(|e| panic!("{e:#}"));
}

/// Why (#9281 criterion 5): the 0/12 pin is only worth something if it can go
/// red. What: the same corpus with each mechanism neutralised in turn (see
/// [`Mechanisms`]); each run must flip a group of the mechanism it disabled,
/// and the verdict must fail on supersession alone (hit@1 floor 0).
#[tokio::test]
#[ignore = "loads the real all-MiniLM-L6-v2 model; pre-publish ignored-tests lane (#9281)"]
async fn recall_eval_goes_red_with_demotion_or_retirement_off() {
    let (corpus, fp) = shipped();
    let arms = [
        (
            "demotion off",
            Mechanisms {
                demotion: false,
                ..Mechanisms::ON
            },
            // #9433: a retired slot incumbent now carries a `superseded_by`
            // edge, so age is no longer a FactKey group's only signal.
            &[Mechanism::Demotion][..],
        ),
        (
            "retirement off",
            Mechanisms {
                retirement: false,
                ..Mechanisms::ON
            },
            &[Mechanism::FactKey][..],
        ),
        (
            // #9421: no edge, so the untagged groups lose their only signal.
            "supersession off",
            Mechanisms {
                supersession: false,
                ..Mechanisms::ON
            },
            &[Mechanism::SupersededBy][..],
        ),
    ];
    for (label, mech, must_flip) in arms {
        let report = run_real(&corpus, &fp, mech, label).await;
        let flipped = report.superseded_above();
        for m in must_flip {
            assert!(
                flipped.iter().any(|g| g.mechanism == *m),
                "{label}: no {m:?} group flipped — the gate cannot see this revert: {}",
                report.summary()
            );
        }
        let floor_only = Baseline {
            model: fp.model.clone(),
            queries: report.queries.len(),
            groups: report.groups.len(),
            ..Baseline::default()
        };
        let err = verdict(&report, &floor_only, &fp.model)
            .expect_err("the gate must fail with a mechanism off");
        assert!(
            format!("{err:#}").contains("superseded-above-current"),
            "{label}: {err:#}"
        );
    }
}

/// The shipped corpus, baseline and fingerprint parse and agree.
#[test]
fn the_shipped_files_validate() {
    let (corpus, fp) = shipped();
    let baseline = load_baseline(&testdata("baseline.json")).unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!(baseline.queries, corpus.queries.len());
    assert_eq!(baseline.groups, corpus.groups.len());
    assert_eq!(baseline.model, fp.model);
}

/// A corpus below the size floor is refused.
#[test]
fn a_short_corpus_fails_validation() {
    let (mut corpus, _) = shipped();
    corpus.queries.truncate(MIN_QUERIES - 1);
    assert!(corpus.validate().is_err());
}

/// Why (#9281 criterion 2): a missing baseline must fail, never default to 0.
#[test]
fn a_missing_baseline_fails_the_gate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let err = load_baseline(&tmp.path().join("baseline.json"))
        .expect_err("a missing baseline must fail the gate");
    assert!(format!("{err:#}").contains("missing"), "{err:#}");
}

/// Why (#9281 criterion 2): a corrupt baseline must fail, never default.
/// What: unparseable, empty, unknown-field, inconsistent and unpinned files.
#[test]
fn a_corrupt_baseline_fails_the_gate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ok = r#""model":"m","queries":30,"groups":10"#;
    let cases = [
        ("not json", "hit@1 = 14".to_string()),
        ("empty object", "{}".to_string()),
        (
            "unknown field",
            format!(r#"{{{ok},"hit_at_1":1,"superseded_above_current":0,"x":1}}"#),
        ),
        (
            "hit above queries",
            format!(r#"{{{ok},"hit_at_1":31,"superseded_above_current":0}}"#),
        ),
        (
            "superseded unpinned",
            format!(r#"{{{ok},"hit_at_1":20,"superseded_above_current":1}}"#),
        ),
        (
            "zero queries",
            r#"{"model":"m","queries":0,"groups":10,"hit_at_1":0,"superseded_above_current":0}"#
                .to_string(),
        ),
    ];
    for (label, body) in cases {
        let path = tmp.path().join("baseline.json");
        std::fs::write(&path, body).expect("write");
        assert!(load_baseline(&path).is_err(), "{label}: must fail the gate");
    }
}

/// Why (#9281 criterion 3): an unavailable embedder fails, never skips.
#[tokio::test]
async fn an_unavailable_embedder_fails_the_gate() {
    let (_, fp) = shipped();
    let err = require_pinned_model(Err(anyhow!("FastEmbedder cold init timed out")), &fp)
        .await
        .expect_err("an unavailable embedder must fail the gate");
    assert!(format!("{err:#}").contains("unavailable"), "{err:#}");
}

/// Why (#9281 criterion 3): the model is pinned; the hash mock must not pass.
#[tokio::test]
async fn a_mock_embedder_fails_the_model_pin() {
    let (_, fp) = shipped();
    let mock: SharedEmbedder = Arc::new(trusty_common::embedder::MockEmbedder::new(384));
    let err = require_pinned_model(Ok(mock), &fp)
        .await
        .expect_err("a non-pinned embedder must fail the gate");
    assert!(format!("{err:#}").contains("pinned"), "{err:#}");
}

/// Why: the verdict is the gate. What: passes at the baseline, fails one
/// below it, and fails on a single superseded-above-current group.
#[test]
fn the_verdict_fails_below_baseline_or_on_any_superseded_hit() {
    let q = |rank| QueryOutcome {
        id: "q".into(),
        expected: "d".into(),
        rank,
        top: None,
    };
    let g = |current, superseded| GroupOutcome {
        id: "g".into(),
        mechanism: Mechanism::Demotion,
        current,
        superseded,
        scores: [None, None],
    };
    let baseline = Baseline {
        model: "m".into(),
        queries: 2,
        hit_at_1: 1,
        groups: 1,
        ..Baseline::default()
    };
    let pass = Report {
        queries: vec![q(Some(0)), q(Some(3))],
        groups: vec![g(Some(0), Some(1))],
    };
    assert!(verdict(&pass, &baseline, "m").is_ok());
    let below = Report {
        queries: vec![q(Some(1)), q(None)],
        ..pass.clone()
    };
    assert!(verdict(&below, &baseline, "m").is_err());
    for flipped in [g(Some(2), Some(0)), g(None, Some(4))] {
        let r = Report {
            groups: vec![flipped],
            ..pass.clone()
        };
        assert!(verdict(&r, &baseline, "m").is_err());
    }
    assert!(verdict(&pass, &baseline, "other-model").is_err());
}

/// Why (#9281 Fail-Open Check): `Baseline::default()` is the red-proof's zero
/// floor and must never become the shipped gate's floor.
/// What: a baseline file carrying a zero hit@1 floor is refused at load, and
/// the default baseline itself fails the verdict on a perfect report.
#[test]
fn a_zero_floor_never_reaches_the_gate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("baseline.json");
    let zero = r#"{"model":"m","queries":1,"groups":1,"hit_at_1":0,"superseded_above_current":0}"#;
    std::fs::write(&path, zero).expect("write");
    let err = load_baseline(&path).expect_err("a zero floor must fail the gate");
    assert!(format!("{err:#}").contains("floor is zero"), "{err:#}");
    let perfect = Report {
        queries: vec![QueryOutcome {
            id: "q".into(),
            expected: "d".into(),
            rank: Some(0),
            top: None,
        }],
        groups: vec![GroupOutcome {
            id: "g".into(),
            mechanism: Mechanism::FactKey,
            current: Some(0),
            superseded: Some(1),
            scores: [None, None],
        }],
    };
    assert!(verdict(&perfect, &Baseline::default(), "m").is_err());
}

/// Why (#9281 Fail-Open Check): a drawer `rank_of` cannot find must count
/// against the gate, never be skipped.
/// What: an unrecalled expected drawer is a hit@1 miss, and a group whose
/// current drawer is unrecalled fails the pin even when the superseded one is
/// unrecalled too.
#[test]
fn an_unrecalled_drawer_counts_against_the_gate() {
    let report = Report {
        queries: vec![QueryOutcome {
            id: "q".into(),
            expected: "d".into(),
            rank: None,
            top: None,
        }],
        groups: vec![GroupOutcome {
            id: "g".into(),
            mechanism: Mechanism::Demotion,
            current: None,
            superseded: None,
            scores: [None, None],
        }],
    };
    assert_eq!(report.hit_at_1(), 0, "an unrecalled drawer is a miss");
    assert_eq!(
        report.superseded_above().len(),
        1,
        "an unrecalled current drawer fails its group"
    );
    let floor_only = Baseline {
        model: "m".into(),
        queries: 1,
        groups: 1,
        ..Baseline::default()
    };
    let err = verdict(&report, &floor_only, "m").expect_err("the gate must fail");
    assert!(
        format!("{err:#}").contains("superseded-above-current"),
        "{err:#}"
    );
}
