//! #9447 regression tests: M005 keeps a vector for every chunk whose text
//! duplicates another's, and reports the real vector gap.
//!
//! Why: the plan kept one old id per text hash and each re-chunked chunk
//! overwrote the previous claim on it. With unchanged ids the re-pointed vector
//! was then swept as an orphan, so both copies ended with no vector while M005
//! reported `unembedded=0` — 38,394 chunks on a 158k index.
//! What: seeds two pairs of identical-text files on top of the M005 fixture — a
//! `.rs` pair whose named ids change and a `.txt` pair whose ids do not — runs
//! the pass, and checks the vector store against the migrated corpus.
//! Test: `m005_keeps_a_vector_for_every_duplicate_text_chunk`,
//! `m005_reports_the_real_vector_gap`,
//! `claim_vectors_gives_each_duplicate_its_own_vector`.

use super::*;

const DUP_RS: &str = "pub fn gamma() -> u32 {\n    3\n}\n";
const DUP_TXT: &str = "shared licence header\nsecond line\nthird line\n";
const DUP_FILES: &[&str] = &["a/dup.rs", "b/dup.rs", "a/notes.txt", "b/notes.txt"];

/// Write the duplicate files into `f`'s tree and seed their legacy-shaped
/// chunks and vectors, as the fixture does for its own files. Returns the
/// seeded ids.
async fn seed_duplicates(f: &Fixture) -> Vec<String> {
    let mut chunks: Vec<RawChunk> = Vec::new();
    for file in DUP_FILES {
        let content = if file.ends_with(".rs") {
            DUP_RS
        } else {
            DUP_TXT
        };
        let abs = f.root.join(file);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, content).unwrap();
        for mut c in chunk_ast(file, content).0 {
            let (base, _, _) = crate::core::chunk_id::split_tails(&c.id);
            c.id = base.to_string();
            c.id = legacy_id(&c);
            chunks.push(c);
        }
    }
    let mut by_text: HashMap<[u8; 32], usize> = HashMap::new();
    for c in &chunks {
        *by_text.entry(text_hash(&c.content)).or_default() += 1;
    }
    assert_eq!(
        by_text.values().filter(|n| **n == 2).count(),
        2,
        "the fixture must seed two pairs of identical-text chunks: {chunks:?}"
    );
    let corpus = f.handle.indexer.read().await.corpus_store().unwrap();
    corpus.upsert_chunks(&chunks).expect("seed duplicates");
    for c in &chunks {
        f.store
            .upsert(&c.id, seed_vector(&c.content))
            .await
            .unwrap();
    }
    chunks.into_iter().map(|c| c.id).collect()
}

/// Every chunk of a duplicate-text file keeps a vector, and no old id is left
/// in the store once the corpus no longer holds it.
/// Test: this test.
#[tokio::test]
async fn m005_keeps_a_vector_for_every_duplicate_text_chunk() {
    let f = fixture_named("m005-dup-text-9447").await;
    let seeded = seed_duplicates(&f).await;

    M005ChunkIdEndLine.apply(&f.handle).await.expect("apply");

    let after = corpus_chunks(&f).await;
    let dups: Vec<&RawChunk> = after
        .iter()
        .filter(|c| DUP_FILES.contains(&c.file.as_str()))
        .collect();
    assert_eq!(dups.len(), seeded.len(), "every duplicate re-chunks");
    for c in &dups {
        assert!(
            f.store.contains(&c.id).await,
            "#9447: a chunk whose text duplicates another's lost its vector: {}",
            c.id
        );
    }
    let live: std::collections::HashSet<&String> = after.iter().map(|c| &c.id).collect();
    for id in seeded.iter().filter(|id| !live.contains(id)) {
        assert!(
            !f.store.contains(id).await,
            "an old id the corpus no longer holds stayed in the store: {id}"
        );
    }
    assert_eq!(f.embed_calls.load(Ordering::SeqCst), 0, "zero re-embed");
}

/// The gap M005 reports equals the number of migrated chunks with no vector.
/// Test: this test.
#[tokio::test]
async fn m005_reports_the_real_vector_gap() {
    let f = fixture_named("m005-dup-gap-9447").await;
    seed_duplicates(&f).await;

    let report = M005ChunkIdEndLine.run(&f.handle).await.expect("run");

    let mut gap = 0usize;
    for c in corpus_chunks(&f).await {
        if !f.store.contains(&c.id).await {
            gap += 1;
        }
    }
    assert!(gap > 0, "the fixture's recovered chunks must open a gap");
    assert_eq!(
        report.unembedded, gap,
        "#9447: M005 must report the number of chunks it left without a vector"
    );
}

/// The claim is injective and keeps unchanged ids on their own vectors, and the
/// orphan sweep spares an old id that a vector was just re-pointed onto.
/// Test: this test.
#[test]
fn claim_vectors_gives_each_duplicate_its_own_vector() {
    let (h1, h2, h3, h4) = ([1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]);
    let s = |v: &str| v.to_string();
    let pools: HashMap<[u8; 32], Vec<String>> = [
        (h1, vec![s("x"), s("y")]),
        (h2, vec![s("p")]),
        (h4, vec![s("q")]),
    ]
    .into_iter()
    .collect();
    let new = vec![
        (s("t"), h1), // claims `x` — `y` is taken by its own id
        (s("y"), h1), // unchanged id, keeps its own vector
        (s("u"), h1), // a third copy: no vector left to hand on
        (s("q"), h2), // `q`'s text changed; it claims `p`
        (s("z"), h3), // text the corpus never held
    ];

    let remap = super::super::claim_vectors(&new, &pools);

    let want: HashMap<String, String> = [(s("x"), s("t")), (s("y"), s("y")), (s("p"), s("q"))]
        .into_iter()
        .collect();
    assert_eq!(remap, want);

    let old = vec![s("x"), s("y"), s("p"), s("q")];
    let (orphans, _) = super::super::partition_orphans(&old, &remap, &BTreeSet::new());
    assert!(
        orphans.is_empty(),
        "`q` now names the vector re-pointed from `p` and must not be swept: {orphans:?}"
    );
}
