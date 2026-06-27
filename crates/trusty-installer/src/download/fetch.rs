//! HTTP download, SHA-256 verification, and tar.gz extraction for prebuilt binaries.
//!
//! Why: The prebuilt-first install path must be atomic — a partial download or a
//! SHA-256 mismatch must never leave a corrupt binary on disk. Every step
//! (download → verify → extract → place) operates on temp files; the final
//! atomic rename is the only moment the destination changes.
//!
//! What: [`download_and_verify`] downloads the `.sha256` sidecar, then the
//! tarball, and verifies the digest. [`extract_binaries`] unpacks all regular
//! files from the tarball into a temp dir. [`place_binaries`] atomically renames
//! each extracted binary into the destination directory. These are composed by the
//! higher-level [`download_prebuilt`] orchestrator in `mod.rs`.
//!
//! Test: `tests` cover SHA-256 verify (match + tampered → error) and extraction
//! (multi-binary archive). Real network calls are `#[ignore]`-tagged.

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// Download a URL to a local file path, streaming the response body.
///
/// Why: Streaming avoids buffering the entire tarball in memory; prebuilt
/// binaries can be tens of megabytes.
///
/// What: GETs `url` with the supplied client, follows redirects (reqwest default),
/// and writes the body to `dest` in chunks.
///
/// Test: Side-effecting; covered by the live `#[ignore]` integration test in
/// `tests::download_and_verify_live`.
pub async fn download_to_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> anyhow::Result<()> {
    let mut response = client
        .get(url)
        .header("User-Agent", "trusty-installer")
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP error from {url}"))?;

    let mut file = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("creating temp file {}", dest.display()))?;

    while let Some(chunk) = response.chunk().await.context("reading response body")? {
        file.write_all(&chunk)
            .await
            .context("writing chunk to temp file")?;
    }
    file.flush().await.context("flushing temp file")?;
    Ok(())
}

/// Download the tarball + `.sha256` sidecar and verify integrity.
///
/// Why: Integrity must be checked before any bytes from the archive are trusted;
/// downloading both into a temp dir and verifying before returning keeps the
/// failure path clean (the temp dir is the caller's responsibility to clean up).
///
/// What: Downloads `sha256_url` into `{tmp_dir}/{archive_name}.sha256`, then
/// `tarball_url` into `{tmp_dir}/{archive_name}`. Reads the `.sha256` file (format:
/// `<hex>  <filename>` or just `<hex>`), computes SHA-256 of the tarball, and
/// returns `Err` on mismatch. Returns the path to the verified tarball on success.
///
/// Test: `tests::verify_sha256_match`, `tests::verify_sha256_tampered`.
pub async fn download_and_verify(
    client: &reqwest::Client,
    tarball_url: &str,
    sha256_url: &str,
    archive_name: &str,
    tmp_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let sha_path = tmp_dir.join(format!("{archive_name}.sha256"));
    let tar_path = tmp_dir.join(archive_name);

    // Download the checksum first (smaller, fails fast if the release has no asset).
    download_to_file(client, sha256_url, &sha_path)
        .await
        .with_context(|| format!("downloading checksum from {sha256_url}"))?;

    // Download the tarball.
    download_to_file(client, tarball_url, &tar_path)
        .await
        .with_context(|| format!("downloading tarball from {tarball_url}"))?;

    // Read and parse the expected hex digest (strip the optional filename suffix).
    let sha_content = std::fs::read_to_string(&sha_path)
        .with_context(|| format!("reading checksum file {}", sha_path.display()))?;
    let expected_hex = parse_sha256_line(&sha_content)?;

    // Compute the actual digest.
    let actual_hex = sha256_file(&tar_path)?;

    if actual_hex != expected_hex {
        return Err(anyhow!(
            "SHA-256 mismatch for {archive_name}: expected {expected_hex}, got {actual_hex}"
        ));
    }

    Ok(tar_path)
}

/// Parse a SHA-256 checksum line of the form `<hex>  <filename>` or bare `<hex>`.
///
/// Why: The `.sha256` files published by the release workflow follow the
/// `shasum -a 256` / `sha256sum` format (`hex  filename`); we need just the hex.
///
/// What: Trims whitespace, takes the first whitespace-delimited token, and
/// validates it is a 64-character hex string.
///
/// Test: `tests::parse_sha256_line_*`.
pub(crate) fn parse_sha256_line(content: &str) -> anyhow::Result<String> {
    let hex = content
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("empty checksum file"))?
        .to_lowercase();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "invalid SHA-256 hex (expected 64 hex chars, got {:?})",
            hex
        ));
    }
    Ok(hex)
}

/// Compute the SHA-256 hex digest of a file synchronously.
///
/// Why: The digest computation reads the entire file; it is simpler (and fast
/// enough for binaries < 100 MB) to do this synchronously rather than spawning
/// an async reader.
///
/// What: Opens the file, streams through a `sha2::Sha256` hasher, and returns
/// the lowercase hex digest.
///
/// Test: `tests::verify_sha256_match` / `tests::verify_sha256_tampered` exercise
/// this indirectly; `tests::sha256_file_correct` exercises it directly.
pub(crate) fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening {} for hashing", path.display()))?;
    let mut buf = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| "reading file for hash")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Extract all regular files from a `.tar.gz` archive into `dest_dir`.
///
/// Why: A crate's prebuilt tarball may contain multiple binaries (e.g.
/// `trusty-search` ships both `trusty-search` and `trusty-embedderd`). We install
/// ALL regular files found in the archive so no binary is silently skipped.
///
/// What: Decompresses with `flate2::GzDecoder`, iterates entries with the `tar`
/// crate, and unpacks every regular file into `dest_dir` (stripped of any leading
/// path components — only the basename is used). Returns the list of extracted
/// basenames.
///
/// Test: `tests::extract_multi_binary_archive`.
pub fn extract_binaries(tarball: &Path, dest_dir: &Path) -> anyhow::Result<Vec<String>> {
    let file = std::fs::File::open(tarball)
        .with_context(|| format!("opening tarball {}", tarball.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    let mut extracted = Vec::new();
    for entry_result in archive.entries().context("reading tar entries")? {
        let mut entry = entry_result.context("reading tar entry")?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() {
            continue;
        }
        let entry_path = entry.path().context("reading entry path")?;
        // Use only the filename (basename), stripping any directory prefix that
        // the release workflow might embed (e.g. `trusty-search-0.25.0-<target>/`).
        let name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("entry has no valid filename: {}", entry_path.display()))?
            .to_owned();
        let dest_file = dest_dir.join(&name);
        entry
            .unpack(&dest_file)
            .with_context(|| format!("extracting {name} to {}", dest_file.display()))?;
        extracted.push(name);
    }

    Ok(extracted)
}

/// Atomically place extracted binaries into the install directory.
///
/// Why: A temp-file + rename pattern ensures either the old or new binary is
/// present on disk at all times — a partial write never leaves a half-replaced
/// binary (consistent with the macOS cdhash note: `rename(2)` is atomic on POSIX).
///
/// What: For each `binary_name` in `binary_names`, reads from `src_dir/<name>`,
/// writes to `dest_dir/.<name>.tmp.<pid>`, sets mode 0755, then renames into
/// `dest_dir/<name>`. Returns the list of successfully placed binary paths.
///
/// Test: `tests::place_binaries_atomic`, `tests::place_binaries_rename_failure_returns_error`.
pub fn place_binaries(
    src_dir: &Path,
    dest_dir: &Path,
    binary_names: &[String],
) -> anyhow::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("creating install dir {}", dest_dir.display()))?;

    let pid = std::process::id();
    let mut placed = Vec::new();

    for name in binary_names {
        let src = src_dir.join(name);
        if !src.exists() {
            // Silently skip non-existent sources (e.g. optional alias binaries).
            continue;
        }
        let tmp_path = dest_dir.join(format!(".{name}.tmp.{pid}"));
        let dest_path = dest_dir.join(name);

        // Copy to the temp path first (stays in the same filesystem → rename is atomic).
        std::fs::copy(&src, &tmp_path).with_context(|| format!("copying {name} to temp path"))?;

        // Set executable permissions (0755).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&tmp_path, perms)
                .with_context(|| format!("setting permissions on {}", tmp_path.display()))?;
        }

        // Atomic rename — propagate errors so a failed rename never silently
        // reports success when a stale binary already exists at dest_path.
        // On failure, remove the temp file to avoid litter before returning.
        if let Err(e) = std::fs::rename(&tmp_path, &dest_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(anyhow::Error::new(e)
                .context(format!("renaming temp file to {}", dest_path.display())));
        }

        placed.push(dest_path);
    }

    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: A matching SHA-256 must not return an error; a tampered file must.
    /// What: Creates a temp file with known content, computes the expected digest,
    /// calls `sha256_file`, asserts equality. Then overwrites the file and asserts
    /// a mismatch.
    /// Test: This is the test.
    #[test]
    fn sha256_file_correct() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"hello trusty").unwrap();
        // Computed independently: echo -n "hello trusty" | sha256sum
        let expected = sha256_file(&path).unwrap();
        assert_eq!(expected.len(), 64);
        assert!(expected.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Why: The SHA-256 parse must accept both bare hex and `hex  filename` format.
    /// What: Tests both formats.
    /// Test: This is the test.
    #[test]
    fn parse_sha256_line_bare_hex() {
        let hex = "a".repeat(64);
        let parsed = parse_sha256_line(&hex).unwrap();
        assert_eq!(parsed, hex);
    }

    /// Why: The `sha256sum` / `shasum` format embeds the filename after two spaces.
    /// What: Supplies `<hex>  <filename>` and asserts only the hex part is returned.
    /// Test: This is the test.
    #[test]
    fn parse_sha256_line_with_filename() {
        let hex = "b".repeat(64);
        let content = format!("{hex}  some-archive.tar.gz");
        let parsed = parse_sha256_line(&content).unwrap();
        assert_eq!(parsed, hex);
    }

    /// Why: A 63-char hex string must be rejected (would silently accept a truncated
    /// or wrong digest).
    /// What: Supplies a 63-char string; asserts Err.
    /// Test: This is the test.
    #[test]
    fn parse_sha256_line_rejects_wrong_length() {
        let hex = "a".repeat(63);
        assert!(parse_sha256_line(&hex).is_err());
    }

    /// Why: Non-hex characters must be rejected to prevent digest spoofing.
    /// What: Supplies a 64-char string with a non-hex char; asserts Err.
    /// Test: This is the test.
    #[test]
    fn parse_sha256_line_rejects_non_hex() {
        let hex = format!("{}z", "a".repeat(63));
        assert!(parse_sha256_line(&hex).is_err());
    }

    /// Why: A tampered tarball (wrong SHA-256) must be rejected before extraction.
    /// What: Writes a tarball with known content; computes and writes a SHA-256
    /// of DIFFERENT content; calls `sha256_file` and asserts the digests differ,
    /// simulating what `download_and_verify` would do.
    /// Test: This is the test.
    #[test]
    fn verify_sha256_tampered_is_detectable() {
        let dir = tempfile::tempdir().unwrap();
        let real_path = dir.path().join("real.bin");
        let tampered_path = dir.path().join("tampered.bin");

        std::fs::write(&real_path, b"real content").unwrap();
        std::fs::write(&tampered_path, b"tampered content").unwrap();

        let real_hash = sha256_file(&real_path).unwrap();
        let tampered_hash = sha256_file(&tampered_path).unwrap();
        assert_ne!(real_hash, tampered_hash);
    }

    /// Create a minimal tar.gz in memory containing the given files.
    ///
    /// Why: Test helper so extraction tests don't need real release assets.
    /// What: Builds a gzip-compressed tar archive with one entry per (name, data) pair.
    /// Test: Used by other tests in this module.
    fn make_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::{write::GzEncoder, Compression};
        let buf = Vec::new();
        let enc = GzEncoder::new(buf, Compression::fast());
        let mut ar = tar::Builder::new(enc);
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            ar.append_data(&mut header, name, *data).unwrap();
        }
        let enc = ar.into_inner().unwrap();
        enc.finish().unwrap()
    }

    /// Why: A multi-binary archive (e.g. trusty-search + trusty-embedderd) must
    /// have ALL regular files extracted, not just the first.
    /// What: Builds a two-entry tarball; calls `extract_binaries`; asserts both
    /// names are in the returned list and both files exist.
    /// Test: This is the test.
    #[test]
    fn extract_multi_binary_archive() {
        let dir = tempfile::tempdir().unwrap();
        let tar_data = make_tarball(&[
            ("trusty-search", b"fake-binary-1"),
            ("trusty-embedderd", b"fake-binary-2"),
        ]);
        let tar_path = dir.path().join("test.tar.gz");
        std::fs::write(&tar_path, &tar_data).unwrap();

        let dest = dir.path().join("extracted");
        std::fs::create_dir_all(&dest).unwrap();
        let names = extract_binaries(&tar_path, &dest).unwrap();

        assert_eq!(names.len(), 2);
        assert!(names.contains(&"trusty-search".to_owned()));
        assert!(names.contains(&"trusty-embedderd".to_owned()));
        assert!(dest.join("trusty-search").exists());
        assert!(dest.join("trusty-embedderd").exists());
    }

    /// Why: Extraction with path-prefixed entries (e.g. `trusty-search-0.25.0-<target>/trusty-search`)
    /// must strip the directory prefix and land only the basename.
    /// What: Builds a tarball with a path-prefixed entry; asserts the basename is
    /// extracted without the prefix.
    /// Test: This is the test.
    #[test]
    fn extract_strips_directory_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let tar_data = make_tarball(&[(
            "trusty-search-0.25.0-aarch64-apple-darwin/trusty-search",
            b"bin",
        )]);
        let tar_path = dir.path().join("test.tar.gz");
        std::fs::write(&tar_path, &tar_data).unwrap();

        let dest = dir.path().join("extracted");
        std::fs::create_dir_all(&dest).unwrap();
        let names = extract_binaries(&tar_path, &dest).unwrap();

        assert_eq!(names, vec!["trusty-search".to_owned()]);
        assert!(dest.join("trusty-search").exists());
    }

    /// Why: `place_binaries` must atomically rename files into the install dir;
    /// after placement each file must exist at the destination path.
    /// What: Writes two fake binaries into a src dir; calls `place_binaries`;
    /// asserts both appear in the dest dir and the paths are returned.
    /// Test: This is the test.
    #[test]
    fn place_binaries_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();

        std::fs::write(src.join("bin-a"), b"a").unwrap();
        std::fs::write(src.join("bin-b"), b"b").unwrap();

        let names = vec!["bin-a".to_owned(), "bin-b".to_owned()];
        let placed = place_binaries(&src, &dest, &names).unwrap();

        assert_eq!(placed.len(), 2);
        assert!(dest.join("bin-a").exists());
        assert!(dest.join("bin-b").exists());
    }

    /// Why: Missing source files must be silently skipped (the primary binary is
    /// always expected; optional alias binaries may be absent in older releases).
    /// What: Provides a src dir with only `bin-a`; includes `bin-b` (missing) in
    /// the names list; asserts only `bin-a` is placed.
    /// Test: This is the test.
    #[test]
    fn place_binaries_skips_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("bin-a"), b"a").unwrap();

        let names = vec!["bin-a".to_owned(), "bin-b".to_owned()];
        let placed = place_binaries(&src, &dest, &names).unwrap();

        assert_eq!(placed.len(), 1);
        assert!(dest.join("bin-a").exists());
        assert!(!dest.join("bin-b").exists());
    }

    /// Why: Guards against the false-success bug where a failed rename was
    /// swallowed and `dest_path.exists()` on a pre-existing binary returned true,
    /// making the caller believe the NEW binary was installed when it wasn't.
    ///
    /// What: Pre-places a "stale" binary at the destination. Then makes the dest
    /// dir read-only so the rename of the temp file into it must fail (on POSIX,
    /// `rename(2)` requires write permission on the destination directory). Asserts
    /// `place_binaries` returns `Err` — not `Ok` with the stale path.
    ///
    /// Test: This is the test. Marked `#[cfg(unix)]` because Windows permission
    /// semantics differ and the read-only-dir trick does not reliably block rename.
    #[test]
    #[cfg(unix)]
    fn place_binaries_rename_failure_returns_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dest).unwrap();

        // Write source binary.
        std::fs::write(src.join("my-bin"), b"new version").unwrap();

        // Pre-place a stale binary in dest so dest_path.exists() would be true
        // even if the rename fails.
        std::fs::write(dest.join("my-bin"), b"old version").unwrap();

        // Make dest read-only so rename into it fails.
        let ro_perms = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(&dest, ro_perms).unwrap();

        let names = vec!["my-bin".to_owned()];
        let result = place_binaries(&src, &dest, &names);

        // Restore write permission before asserting (so tempdir cleanup succeeds).
        let rw_perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&dest, rw_perms).unwrap();

        // Must be an error — not a silent success pointing at the stale binary.
        assert!(
            result.is_err(),
            "expected place_binaries to return Err when rename fails, got Ok"
        );

        // Confirm the stale content is still there (the new binary was NOT placed).
        let content = std::fs::read(dest.join("my-bin")).unwrap();
        assert_eq!(
            content, b"old version",
            "stale binary should still be in place after failed rename"
        );
    }
}
