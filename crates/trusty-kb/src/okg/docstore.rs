//! Doc-store scanner — a directory of text files into [`SourceItem`]s.
//!
//! Why: the doc-store fetcher is pure filesystem work, so it belongs in this
//! deterministic crate rather than behind a network client. Its idempotency key
//! is a real content hash (path alone cannot detect an edit, and mtime lies
//! across checkouts and syncs), so an unchanged file skips and an edited file
//! re-ingests. Binary files must degrade gracefully — a PDF or a JPEG in the
//! corpus is a normal occurrence, not a run-ending error.
//!
//! What: [`scan`] walks the directory in sorted order, filters by extension,
//! rejects non-UTF-8 content as binary, hashes the bytes, and splits oversized
//! documents into deterministic part-items so no single entity becomes
//! unreadable. Every item carries its relative path as the ledger key, so the
//! scan is reproducible on any machine.
//!
//! Test: `scan_is_deterministic_and_hashes_content`, `binary_files_are_skipped`,
//! `edit_changes_the_fingerprint`, `large_file_chunks_deterministically`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::okg::ingest::SourceItem;
use crate::okg::policy::DocStorePolicy;

/// Extensions treated as text when a source names none.
pub const DEFAULT_EXTENSIONS: &[&str] = &[
    "md", "markdown", "mdx", "txt", "text", "rst", "org", "adoc", "csv", "tsv", "json", "yaml",
    "yml", "toml", "ini", "log", "html", "htm", "tex",
];

/// Characters per entity before a document is split into parts.
pub const DEFAULT_CHUNK_CHARS: usize = 20_000;

/// Hard ceiling on a single file read, so one pathological file cannot exhaust
/// memory during a bulk walk.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// What a doc-store walk found, including everything it declined to ingest.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ScanOutcome {
    /// Items ready for the ingest engine, in deterministic path order.
    #[serde(skip)]
    pub items: Vec<SourceItem>,
    /// Files present but not text (binary, or undecodable), relative paths.
    pub skipped_binary: Vec<String>,
    /// Files whose extension is not in the accepted set.
    pub skipped_extension: usize,
    /// Files that could not be read at all, with the reason.
    pub errors: Vec<String>,
}

/// Walk a doc-store directory into [`SourceItem`]s.
///
/// Why/What: see the module doc. `extensions` empty means [`DEFAULT_EXTENSIONS`].
///
/// `policy` is the READ-side confinement gate and is applied HERE, at the point
/// the filesystem is actually touched, rather than only at the tool boundary —
/// so a `registry.toml` row that was hand-edited (or written before the policy
/// existed) to point at `~/.ssh` is still refused on every later run.
/// Test: the tests in this module, plus `policy::tests`.
pub fn scan(
    dir: &Path,
    extensions: &[String],
    recursive: bool,
    chunk_chars: usize,
    policy: &DocStorePolicy,
) -> anyhow::Result<ScanOutcome> {
    // Walk the RESOLVED path the policy returned, never the caller's spelling.
    let dir = &policy.permit(dir)?;
    let accepted: Vec<String> = if extensions.is_empty() {
        DEFAULT_EXTENSIONS.iter().map(|e| e.to_string()).collect()
    } else {
        extensions
            .iter()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .collect()
    };
    let chunk_chars = chunk_chars.max(1_000);

    let mut outcome = ScanOutcome::default();
    // `follow_links(false)` is WalkDir's default, but it is pinned here because
    // it is LOAD-BEARING for confinement: `DocStorePolicy` authorises the root,
    // and this is what stops a symlink INSIDE that root from walking the scan
    // back out to `~/.ssh` or `/etc`. A refactor that flipped it would silently
    // reopen the hole the policy exists to close.
    let walker = WalkDir::new(dir)
        .follow_links(false)
        .max_depth(if recursive { usize::MAX } else { 1 })
        .sort_by_file_name();

    for entry in walker.into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = path.strip_prefix(dir) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        // Dotfiles and dot-directories are tooling, not corpus.
        if rel.starts_with('.') || rel.contains("/.") {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !accepted.iter().any(|a| a == &ext) {
            outcome.skipped_extension += 1;
            continue;
        }

        match read_text(path) {
            Ok(Some((text, fingerprint))) => {
                let timestamp = modified_iso(path);
                outcome.items.extend(chunk_items(
                    &rel,
                    &text,
                    &fingerprint,
                    timestamp.as_deref(),
                    chunk_chars,
                ));
            }
            Ok(None) => outcome.skipped_binary.push(rel),
            Err(e) => outcome.errors.push(format!("{rel}: {e}")),
        }
    }
    Ok(outcome)
}

/// Read a file as UTF-8 text plus its content hash, or `None` when binary.
fn read_text(path: &Path) -> anyhow::Result<Option<(String, String)>> {
    let len = std::fs::metadata(path)?.len();
    if len > MAX_FILE_BYTES {
        anyhow::bail!("file is {len} bytes, over the {MAX_FILE_BYTES}-byte scan limit");
    }
    let bytes = std::fs::read(path)?;
    // A NUL byte is the classic binary tell and catches UTF-16 too, which would
    // otherwise decode as text-with-nulls and produce an unreadable entity.
    if bytes.contains(&0) {
        return Ok(None);
    }
    let Ok(text) = String::from_utf8(bytes.clone()) else {
        return Ok(None);
    };
    let fingerprint = format!("sha256:{:x}", Sha256::digest(&bytes));
    Ok(Some((text, fingerprint)))
}

/// Split one document into deterministic part-items.
///
/// Why: a 200 KB export as a single entity is unreadable and unusable for
/// retrieval, but chunking must not break idempotency — so every chunk carries
/// the WHOLE file's hash as its fingerprint (any edit re-ingests every part) and
/// a positional item id (`<rel>#<n>`) so parts dedupe independently and a file
/// that shrinks leaves its surplus parts detectable as vanished.
/// What: splits on blank lines at the chunk boundary, falling back to a hard cut
/// when a single paragraph exceeds the budget.
/// Test: `large_file_chunks_deterministically`.
fn chunk_items(
    rel: &str,
    text: &str,
    fingerprint: &str,
    timestamp: Option<&str>,
    chunk_chars: usize,
) -> Vec<SourceItem> {
    let stem = rel.rsplit_once('.').map(|(s, _)| s).unwrap_or(rel);
    let chunks = split_chunks(text, chunk_chars);
    let total = chunks.len();

    chunks
        .into_iter()
        .enumerate()
        .map(|(i, body)| {
            let (item_id, name, title) = if total == 1 {
                (rel.to_string(), stem.to_string(), stem.to_string())
            } else {
                (
                    format!("{rel}#{i}"),
                    format!("{stem} part {}", i + 1),
                    format!("{stem} (part {} of {total})", i + 1),
                )
            };
            let mut fields = BTreeMap::new();
            fields.insert("source_path".to_string(), rel.to_string());
            if total > 1 {
                fields.insert("part".to_string(), format!("{} of {total}", i + 1));
            }
            SourceItem {
                item_id,
                fingerprint: fingerprint.to_string(),
                name,
                title,
                timestamp: timestamp.map(str::to_string),
                body,
                fields,
                // A content hash is a real change signal, so never volatile.
                volatile: false,
            }
        })
        .collect()
}

/// Cut `text` into chunks of at most `budget` chars, preferring blank-line breaks.
fn split_chunks(text: &str, budget: usize) -> Vec<String> {
    if text.chars().count() <= budget {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n") {
        let para_len = para.chars().count();
        if !current.is_empty() && current.chars().count() + para_len + 2 > budget {
            out.push(std::mem::take(&mut current));
        }
        if para_len > budget {
            // A single oversized paragraph: hard-cut it on char boundaries.
            let chars: Vec<char> = para.chars().collect();
            for slice in chars.chunks(budget) {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                out.push(slice.iter().collect());
            }
            continue;
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// A file's mtime as an ISO-8601 UTC timestamp, when the platform reports one.
fn modified_iso(path: &Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let dt = chrono::DateTime::from_timestamp(secs as i64, 0)?;
    Some(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A policy that permits exactly the fixture directory.
    fn policy(tmp: &tempfile::TempDir) -> DocStorePolicy {
        DocStorePolicy::new(vec![tmp.path().canonicalize().unwrap()])
    }

    fn seed(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for (rel, bytes) in files {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
        tmp
    }

    /// Why: the walk order and the fingerprints ARE the idempotency contract —
    /// two scans of an untouched tree must be identical.
    /// What: scans a nested corpus twice and asserts the item lists match, are
    /// path-sorted, carry content hashes, and record provenance.
    /// Test: self-contained.
    #[test]
    fn scan_is_deterministic_and_hashes_content() {
        let tmp = seed(&[
            ("b.md", b"beta"),
            ("a.txt", b"alpha"),
            ("nested/c.md", b"gamma"),
            ("skipme.png", b"not-text-by-extension"),
            (".hidden/d.md", b"hidden"),
        ]);
        let first = scan(tmp.path(), &[], true, DEFAULT_CHUNK_CHARS, &policy(&tmp)).unwrap();
        let second = scan(tmp.path(), &[], true, DEFAULT_CHUNK_CHARS, &policy(&tmp)).unwrap();
        assert_eq!(first.items, second.items, "scan must be deterministic");

        let ids: Vec<&str> = first.items.iter().map(|i| i.item_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["a.txt", "b.md", "nested/c.md"],
            "sorted, dotdirs skipped"
        );
        assert_eq!(
            first.skipped_extension, 1,
            "the .png is filtered by extension"
        );
        assert!(first.items[0].fingerprint.starts_with("sha256:"));
        assert_eq!(
            first.items[2].fields.get("source_path").map(String::as_str),
            Some("nested/c.md")
        );
        assert_eq!(
            first.items[2].name, "nested/c",
            "name derives from the rel path"
        );

        // Non-recursive must stay at depth 1.
        let flat = scan(tmp.path(), &[], false, DEFAULT_CHUNK_CHARS, &policy(&tmp)).unwrap();
        assert_eq!(flat.items.len(), 2);
    }

    /// Why: a binary file in the corpus is normal; it must be reported and
    /// stepped over, never crash the walk or produce a garbage entity.
    /// What: seeds a NUL-bearing and an invalid-UTF-8 file with text extensions
    /// alongside a good one; asserts only the good one becomes an item.
    /// Test: self-contained.
    #[test]
    fn binary_files_are_skipped() {
        let tmp = seed(&[
            ("good.md", b"readable"),
            ("nulls.txt", b"pdf\x00\x01binary"),
            ("badutf.txt", &[0xff, 0xfe, 0x41, 0x42]),
        ]);
        let out = scan(tmp.path(), &[], true, DEFAULT_CHUNK_CHARS, &policy(&tmp)).unwrap();
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].item_id, "good.md");
        assert_eq!(
            out.skipped_binary,
            vec!["badutf.txt".to_string(), "nulls.txt".to_string()],
            "both binaries surfaced by name"
        );
        assert!(out.errors.is_empty(), "binary content is not an error");
    }

    /// Why: change detection must be content-based — the whole point of hashing
    /// rather than trusting a path or an mtime.
    /// What: scans, edits one file, rescans, and asserts only that file's
    /// fingerprint moved.
    /// Test: self-contained.
    #[test]
    fn edit_changes_the_fingerprint() {
        let tmp = seed(&[("a.md", b"before"), ("b.md", b"stable")]);
        let before = scan(tmp.path(), &[], true, DEFAULT_CHUNK_CHARS, &policy(&tmp)).unwrap();
        std::fs::write(tmp.path().join("a.md"), b"after").unwrap();
        let after = scan(tmp.path(), &[], true, DEFAULT_CHUNK_CHARS, &policy(&tmp)).unwrap();

        assert_ne!(
            before.items[0].fingerprint, after.items[0].fingerprint,
            "a.md changed"
        );
        assert_eq!(
            before.items[1].fingerprint, after.items[1].fingerprint,
            "b.md did not"
        );
        assert_eq!(
            before.items[0].item_id, after.items[0].item_id,
            "id is stable"
        );
    }

    /// Why: chunking must not break the ledger — every part of one file shares
    /// the file's hash so any edit re-ingests all parts, and part ids are
    /// positional so they dedupe independently.
    /// What: chunks a multi-paragraph document under a small budget and asserts
    /// ids, shared fingerprint, and reproducibility.
    /// Test: self-contained.
    #[test]
    fn large_file_chunks_deterministically() {
        let para = "x".repeat(60);
        let body = vec![para.as_str(); 40].join("\n\n");
        let tmp = seed(&[("big.md", body.as_bytes())]);

        let out = scan(tmp.path(), &[], true, 1_000, &policy(&tmp)).unwrap();
        assert!(
            out.items.len() > 1,
            "expected a split; got {}",
            out.items.len()
        );
        let ids: Vec<&str> = out.items.iter().map(|i| i.item_id.as_str()).collect();
        assert_eq!(ids[0], "big.md#0");
        assert_eq!(ids[1], "big.md#1");
        let fp = &out.items[0].fingerprint;
        assert!(
            out.items.iter().all(|i| &i.fingerprint == fp),
            "all parts share the whole-file hash"
        );
        assert_eq!(out.items[0].name, "big part 1");
        assert!(out.items[0].fields.contains_key("part"));

        // Reassembling the parts must lose nothing.
        let joined: String = out
            .items
            .iter()
            .map(|i| i.body.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_eq!(joined.chars().filter(|c| *c == 'x').count(), 40 * 60);
        assert_eq!(
            out.items,
            scan(tmp.path(), &[], true, 1_000, &policy(&tmp))
                .unwrap()
                .items
        );
    }
}
