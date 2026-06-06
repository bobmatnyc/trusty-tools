//! Tests for the doc and skill seeders (`seed_documentation`, `seed_skills`).
//!
//! Why: Isolates the doc/skill seeding coverage so this file and the
//! companion `mcp_tests.rs` each stay under the 500-line cap.
//! What: Stubbed-embedder integration tests against a real `RedbUsearchStore`.
//! Test: This file is itself the test coverage.
#![allow(clippy::await_holding_lock)]

use anyhow::Result;
use tempfile::tempdir;

use super::StubEmbedderShared;
use crate::init::{DOCS_SEED_SESSION_ID, ProjectInitializer, SKILLS_SEED_SESSION_ID};
use crate::memory::store::MemoryStore;
/// Process-wide mutex serializing tests that mutate `$HOME`.
///
/// Why: cargo test runs unit tests on a multi-threaded executor by
/// default; `std::env::set_var` is a process-wide mutation, so two
/// concurrent tests sandboxing HOME stomp on each other and one will
/// see the other's tempdir (or the developer's real `~/.claude/skills/`)
/// before it's restored. We re-export the crate-wide `HOME_LOCK` so
/// this module's tests serialize with HOME-mutating tests in OTHER
/// modules (e.g. `mistake_log`) too.
use crate::test_env::HOME_LOCK;

#[tokio::test]
async fn seeds_documentation_from_docs_dir() {
    use crate::memory::redb_usearch::RedbUsearchStore;
    use crate::memory::store::Segment;

    // Stub embedder: deterministic vector independent of network.
    struct StubEmbedder {
        dim: usize,
    }
    impl crate::memory::Embedder for StubEmbedder {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = vec![0.0f32; self.dim];
                    // tiny content-dependent fingerprint so distinct docs
                    // don't collapse to identical vectors.
                    for (i, b) in t.bytes().take(self.dim).enumerate() {
                        v[i] = (b as f32) / 255.0;
                    }
                    v
                })
                .collect())
        }
        fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
            Ok(self.embed(&[text])?.remove(0))
        }
        fn dimension(&self) -> usize {
            self.dim
        }
    }

    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let omd = root.join(".trusty-agents").join("state");
    tokio::fs::create_dir_all(root.join("docs/user"))
        .await
        .unwrap();
    tokio::fs::write(
        root.join("docs/user/quickstart.md"),
        "# Quickstart\n\nRun `cargo run` to launch CTRL.\n",
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(&omd).await.unwrap();

    let dim = 16;
    let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
    let embedder = StubEmbedder { dim };

    let init = ProjectInitializer::new(&root, &omd);
    let n = init.seed_documentation(&store, &embedder).await.unwrap();
    assert_eq!(n, 1, "expected one doc seeded");

    // Verify the memory store now has the content under the expected id.
    let payload = store
        .get(Segment::AgentMemory, "docs:docs/user/quickstart.md")
        .await
        .unwrap()
        .expect("doc payload should be present");
    let content = payload.get("content").and_then(|v| v.as_str()).unwrap();
    assert!(content.contains("Quickstart"));
    assert!(content.contains("cargo run"));
    assert_eq!(
        payload.get("tag").and_then(|v| v.as_str()),
        Some("docs/user")
    );
    // session_id should be the docs-seed constant so doc memories are
    // distinguishable from agent-run memories during recall/export.
    assert_eq!(
        payload.get("session_id").and_then(|v| v.as_str()),
        Some(DOCS_SEED_SESSION_ID),
    );
    assert!(
        payload.get("created_at").and_then(|v| v.as_str()).is_some(),
        "created_at timestamp should be stamped"
    );

    // Re-seeding without changes should be a no-op.
    let n2 = init.seed_documentation(&store, &embedder).await.unwrap();
    assert_eq!(n2, 0, "unchanged docs should not re-seed");

    // Confirm tracker file written.
    assert!(omd.join("docs_seeded.json").exists());
}

#[tokio::test]
async fn seed_documentation_uses_docs_seed_session() {
    // Focused test: every payload written by seed_documentation must
    // carry session_id == DOCS_SEED_SESSION_ID so doc memories can be
    // identified separately from agent run memories.
    use crate::memory::redb_usearch::RedbUsearchStore;
    use crate::memory::store::Segment;

    struct StubEmbedder {
        dim: usize,
    }
    impl crate::memory::Embedder for StubEmbedder {
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.1f32; self.dim]).collect())
        }
        fn embed_single(&self, _: &str) -> Result<Vec<f32>> {
            Ok(vec![0.1f32; self.dim])
        }
        fn dimension(&self) -> usize {
            self.dim
        }
    }

    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let omd = root.join(".trusty-agents").join("state");
    tokio::fs::create_dir_all(root.join("docs/developer"))
        .await
        .unwrap();
    tokio::fs::write(
        root.join("docs/developer/architecture.md"),
        "# Arch\n\nAgent harness in Rust.\n",
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(&omd).await.unwrap();

    let dim = 16;
    let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
    let embedder = StubEmbedder { dim };

    let init = ProjectInitializer::new(&root, &omd);
    let n = init.seed_documentation(&store, &embedder).await.unwrap();
    assert_eq!(n, 1);

    let payload = store
        .get(Segment::AgentMemory, "docs:docs/developer/architecture.md")
        .await
        .unwrap()
        .expect("payload present");
    assert_eq!(
        payload.get("session_id").and_then(|v| v.as_str()),
        Some(DOCS_SEED_SESSION_ID),
        "doc memories must be tagged with the docs-seed session"
    );
}

#[tokio::test]
async fn seed_documentation_skips_when_docs_missing() {
    use crate::memory::redb_usearch::RedbUsearchStore;

    struct StubEmbedder;
    impl crate::memory::Embedder for StubEmbedder {
        fn embed(&self, _: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![])
        }
        fn embed_single(&self, _: &str) -> Result<Vec<f32>> {
            Ok(vec![0.0; 4])
        }
        fn dimension(&self) -> usize {
            4
        }
    }

    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let omd = root.join(".trusty-agents").join("state");
    tokio::fs::create_dir_all(&omd).await.unwrap();

    let store = RedbUsearchStore::open(&omd.join("sessions/default"), 4).unwrap();
    let init = ProjectInitializer::new(&root, &omd);
    let n = init
        .seed_documentation(&store, &StubEmbedder)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn seed_skills_indexes_skill_files() {
    use crate::memory::redb_usearch::RedbUsearchStore;
    use crate::memory::store::Segment;

    // Sandbox HOME so seed_skills doesn't pick up the developer's real
    // ~/.claude/skills/ during the test. The HOME_LOCK serializes
    // concurrent HOME-sandboxing tests so they don't stomp on each other.
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_home = std::env::var_os("HOME");
    let home_sandbox = tempdir().unwrap();
    // SAFETY: HOME_LOCK is held; restoration runs before guard drop.
    unsafe {
        std::env::set_var("HOME", home_sandbox.path());
    }

    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let omd = root.join(".trusty-agents").join("state");
    let skills_dir = root.join(".trusty-agents").join("skills");
    tokio::fs::create_dir_all(&skills_dir).await.unwrap();
    tokio::fs::write(
        skills_dir.join("python-compat.md"),
        "---\nname: python-compat\ntags: [python, fastapi, bcrypt]\nsummary: Python compatibility fixes\n---\n\n# Python Compat\n\nbody\n",
    )
    .await
    .unwrap();
    // Nested skill should also be picked up.
    let nested = skills_dir.join("frameworks");
    tokio::fs::create_dir_all(&nested).await.unwrap();
    tokio::fs::write(
        nested.join("fastapi.md"),
        "---\nname: fastapi\ntags: [python, fastapi]\ndescription: FastAPI patterns\n---\n\n# FastAPI\n\nbody\n",
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(&omd).await.unwrap();

    let dim = 16;
    let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
    let embedder = StubEmbedderShared { dim };

    let init = ProjectInitializer::new(&root, &omd);
    let n = init.seed_skills(&store, &embedder).await.unwrap();
    assert_eq!(n, 2, "expected two skills seeded, got {n}");

    let payload = store
        .get(Segment::AgentMemory, "skill:python-compat")
        .await
        .unwrap()
        .expect("skill payload should be present");
    assert_eq!(
        payload.get("tag").and_then(|v| v.as_str()),
        Some("configuration/skill")
    );
    assert_eq!(
        payload.get("session_id").and_then(|v| v.as_str()),
        Some(SKILLS_SEED_SESSION_ID)
    );
    assert_eq!(
        payload.get("skill_name").and_then(|v| v.as_str()),
        Some("python-compat")
    );
    let tags = payload
        .get("skill_tags")
        .and_then(|v| v.as_array())
        .expect("skill_tags array");
    assert!(tags.iter().any(|t| t.as_str() == Some("python")));
    assert!(tags.iter().any(|t| t.as_str() == Some("fastapi")));
    assert!(payload.get("path").and_then(|v| v.as_str()).is_some());
    assert!(payload.get("created_at").and_then(|v| v.as_str()).is_some());

    // Re-seeding without changes should be a no-op.
    let n2 = init.seed_skills(&store, &embedder).await.unwrap();
    assert_eq!(n2, 0, "unchanged skills should not re-seed");

    assert!(omd.join("skills_seeded.json").exists());

    // Restore HOME.
    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[tokio::test]
async fn seed_skills_falls_back_to_filename_without_frontmatter() {
    use crate::memory::redb_usearch::RedbUsearchStore;
    use crate::memory::store::Segment;

    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_home = std::env::var_os("HOME");
    let home_sandbox = tempdir().unwrap();
    // SAFETY: HOME_LOCK is held for the duration of this test.
    unsafe {
        std::env::set_var("HOME", home_sandbox.path());
    }

    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let omd = root.join(".trusty-agents").join("state");
    let skills_dir = root.join(".trusty-agents").join("skills");
    tokio::fs::create_dir_all(&skills_dir).await.unwrap();
    // No frontmatter — name should fall back to file stem.
    tokio::fs::write(
        skills_dir.join("orphan-skill.md"),
        "# Orphan Skill\n\nNo frontmatter at all.\n",
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(&omd).await.unwrap();

    let dim = 16;
    let store = RedbUsearchStore::open(&omd.join("sessions/default"), dim).unwrap();
    let embedder = StubEmbedderShared { dim };

    let init = ProjectInitializer::new(&root, &omd);
    let n = init.seed_skills(&store, &embedder).await.unwrap();
    assert_eq!(n, 1);

    let payload = store
        .get(Segment::AgentMemory, "skill:orphan-skill")
        .await
        .unwrap()
        .expect("orphan skill should be indexed");
    assert_eq!(
        payload.get("skill_name").and_then(|v| v.as_str()),
        Some("orphan-skill")
    );

    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[tokio::test]
async fn seed_skills_skips_when_dirs_missing() {
    use crate::memory::redb_usearch::RedbUsearchStore;

    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev_home = std::env::var_os("HOME");
    let home_sandbox = tempdir().unwrap();
    // SAFETY: HOME_LOCK is held for the duration of this test.
    unsafe {
        std::env::set_var("HOME", home_sandbox.path());
    }

    let tmp = tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let omd = root.join(".trusty-agents").join("state");
    tokio::fs::create_dir_all(&omd).await.unwrap();

    let store = RedbUsearchStore::open(&omd.join("sessions/default"), 16).unwrap();
    let embedder = StubEmbedderShared { dim: 16 };
    let init = ProjectInitializer::new(&root, &omd);
    let n = init.seed_skills(&store, &embedder).await.unwrap();
    assert_eq!(n, 0);

    unsafe {
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}
