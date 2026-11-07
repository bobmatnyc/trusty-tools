//! Test helpers for building `CodeChunk` corpora from real on-disk source files.
//!
//! Why: The integration tests in this crate validate the analysis pipeline by
//! running it against this project's own Rust source. To do that without
//! hand-crafting chunks we need a small utility that walks a directory and
//! splits each source file into overlapping windows shaped like the chunks
//! produced by trusty-search.
//!
//! What: `chunks_from_file` splits one file into 40-line windows stepping
//! every 20 lines. `chunks_from_dir` walks a directory recursively, applies
//! the file splitter to every file with the requested extension, and returns
//! the flattened chunk list.
//!
//! Test: exercised indirectly by the integration tests in
//! `integration_tests.rs`; a direct unit test
//! (`chunks_from_file_produces_windows`) verifies windowing for a synthetic
//! file.

use std::fs;
use std::path::{Path, PathBuf};

use crate::types::CodeChunk;

/// Window size in lines for each generated chunk.
const WINDOW_LINES: usize = 40;
/// Step between successive windows in lines (so windows overlap by 20).
const STEP_LINES: usize = 20;

/// Read `path` and split it into overlapping 40-line `CodeChunk` windows.
///
/// Why: Mirrors the chunking shape trusty-search produces well enough that
/// downstream analysis sees realistic content.
/// What: Reads the file, normalizes line endings via `lines()`, and produces
/// one chunk per window with collision-safe id `{path}:{start}:{end}`.
/// Test: `chunks_from_file_produces_windows` covers the windowing math.
pub(crate) fn chunks_from_file(path: &Path) -> anyhow::Result<Vec<CodeChunk>> {
    let content = fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();
    let file_str = path.to_string_lossy().to_string();

    if lines.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    let mut start: usize = 0;
    while start < lines.len() {
        let end = (start + WINDOW_LINES).min(lines.len());
        // Use 1-based line numbers for human friendliness; matches trusty-search.
        let start_line = start + 1;
        let end_line = end;
        let body = lines[start..end].join("\n");
        out.push(CodeChunk {
            id: format!("{file_str}:{start_line}:{end_line}"),
            file: file_str.clone(),
            start_line,
            end_line,
            content: body,
            function_name: None,
            score: 0.0,
            compact_snippet: None,
            match_reason: String::new(),
        });
        // Last window already consumed the tail; bail out.
        if end == lines.len() {
            break;
        }
        start += STEP_LINES;
    }

    Ok(out)
}

/// Walk `dir` recursively, collecting files whose name ends with `ext`
/// (case-sensitive, including the leading dot, e.g. `".rs"`), and return the
/// concatenation of `chunks_from_file` over each.
///
/// Why: Drives integration tests that analyze whole crate trees.
/// What: Iterative depth-first walk; skips unreadable directories silently
/// (returns the partial result) but surfaces file-read errors.
/// Test: `chunks_from_dir_collects_rs_files` exercises a temp tree.
pub(crate) fn chunks_from_dir(dir: &Path, ext: &str) -> anyhow::Result<Vec<CodeChunk>> {
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    let mut out: Vec<CodeChunk> = Vec::new();

    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                // Skip common build/output dirs to keep tests fast.
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if matches!(name, "target" | ".git" | "node_modules") {
                        continue;
                    }
                }
                stack.push(path);
            } else if file_type.is_file() {
                let matches = path.to_str().map(|s| s.ends_with(ext)).unwrap_or(false);
                if matches {
                    let chunks = chunks_from_file(&path)?;
                    out.extend(chunks);
                }
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn chunks_from_file_produces_windows() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("a.rs");
        // 100 lines of trivial content.
        let mut body = String::new();
        for i in 0..100 {
            body.push_str(&format!("let x{i} = {i};\n"));
        }
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();

        let chunks = chunks_from_file(&p).unwrap();
        assert!(!chunks.is_empty());
        // First window spans lines 1..=40.
        assert_eq!(chunks[0].start_line, 1);
        assert!(chunks[0].end_line >= 40);
        // ids are collision-safe.
        let ids: std::collections::HashSet<_> = chunks.iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids.len(), chunks.len());
    }

    #[test]
    fn chunks_from_dir_collects_rs_files() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();

        fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();
        fs::write(sub.join("b.rs"), "fn b() {}\n").unwrap();
        fs::write(tmp.path().join("c.txt"), "not rust\n").unwrap();

        let chunks = chunks_from_dir(tmp.path(), ".rs").unwrap();
        let files: std::collections::HashSet<_> = chunks.iter().map(|c| c.file.clone()).collect();
        assert_eq!(files.len(), 2, "expected 2 rust files, got {files:?}");
    }
}

/// Block a synchronous fake reply for `delay`, so the client's call times out.
///
/// Why: a plain `thread::sleep` keeps the worker's queued tasks — the
/// client's retry included — behind the sleep. `block_in_place` hands them
/// to another worker first. Call it only on an [`IsolatedFake`].
pub(crate) fn stall_reply(delay: std::time::Duration) {
    tokio::task::block_in_place(|| std::thread::sleep(delay));
}

/// A fake trusty-search socket served on its own runtime (#10781).
///
/// Why: a fake reply that sleeps to simulate a slow page blocks the worker it
/// runs on. On the test's own runtime that worker can own the timer the
/// client's call timeout waits on, so the timeout never fires and the test
/// proves nothing.
/// What: builds a separate two-worker runtime, starts the fake on it, and
/// shuts that runtime down in the background on drop.
/// Test: `a_page_that_times_out_once_still_completes_the_walk`.
pub(crate) struct IsolatedFake {
    fake: Option<crate::core::fake_search::FakeSearchSocket>,
    rt: Option<tokio::runtime::Runtime>,
}

impl IsolatedFake {
    pub(crate) fn serve(
        reply: impl Fn(&str, &serde_json::Value) -> crate::core::fake_search::Reply
            + Send
            + Sync
            + 'static,
    ) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("fake search runtime");
        let fake = {
            let _guard = rt.enter();
            crate::core::fake_search::FakeSearchSocket::serve(reply)
        };
        Self {
            fake: Some(fake),
            rt: Some(rt),
        }
    }

    pub(crate) fn get(&self) -> &crate::core::fake_search::FakeSearchSocket {
        self.fake.as_ref().expect("fake present until drop")
    }
}

impl Drop for IsolatedFake {
    fn drop(&mut self) {
        drop(self.fake.take());
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}
