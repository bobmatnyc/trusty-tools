//! Explicit fixture selection for benchmarks that may delete index data.
//!
//! Set TRUSTY_SEARCH_TEST_URL to the dedicated daemon's http://127.0.0.1:PORT,
//! TRUSTY_DATA_DIR to its disposable data directory (containing http_addr and
//! an empty .trusty-search-test-daemon marker), and
//! TRUSTY_SEARCH_TEST_CORPUS_ROOT to a disposable workspace copy without .git.
//! Create .trusty-search-test-corpus in that copy to acknowledge its purpose.
//! Start the fixture daemon with --no-auto-discover; register only copied roots.

#![allow(dead_code)] // Each benchmark uses a different subset of the helpers.

use std::path::{Path, PathBuf};

pub fn validate_endpoint(url: &str, advertised_addr: &str) -> Result<(), String> {
    let addr = url
        .strip_prefix("http://")
        .ok_or("TRUSTY_SEARCH_TEST_URL must use http://127.0.0.1:PORT")?;
    let socket: std::net::SocketAddr = addr
        .parse()
        .map_err(|_| "benchmark URL must contain only a loopback address and port")?;
    if socket.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        || socket.port() == 0
        || socket.port() == 7878
    {
        return Err("benchmark daemon must use 127.0.0.1 and a dedicated non-default port".into());
    }
    if addr != advertised_addr.trim() {
        return Err("benchmark URL does not match TRUSTY_DATA_DIR/http_addr".into());
    }
    Ok(())
}

pub fn validate_corpus(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("invalid corpus root: {e}"))?;
    if !root.join(".trusty-search-test-corpus").is_file() || root.join(".git").exists() {
        return Err(
            "corpus must be a disposable copy marked .trusty-search-test-corpus without .git"
                .into(),
        );
    }
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|e| format!("missing copied corpus {relative}: {e}"))?;
    if !path.starts_with(&root) || !path.is_dir() {
        return Err("benchmark corpus must be a directory inside the disposable copy".into());
    }
    Ok(path)
}

pub fn validate_data_dir(root: &Path) -> Result<(), String> {
    if !root.is_absolute() || !root.join(".trusty-search-test-daemon").is_file() {
        return Err("TRUSTY_DATA_DIR must be an absolute disposable directory marked .trusty-search-test-daemon".into());
    }
    Ok(())
}

/// Refuse a stale index registration pointing outside the requested copied root.
pub fn validate_registered_root(actual: &Path, expected: &Path) -> Result<(), String> {
    let actual = actual
        .canonicalize()
        .map_err(|e| format!("invalid registered root: {e}"))?;
    let expected = expected
        .canonicalize()
        .map_err(|e| format!("invalid expected root: {e}"))?;
    if actual != expected {
        return Err(format!(
            "registered root {} differs from copied corpus {}",
            actual.display(),
            expected.display()
        ));
    }
    Ok(())
}

/// Read back ownership before deleting or reindexing an existing benchmark index.
pub async fn assert_index_root(client: &reqwest::Client, index: &str, expected: &Path) {
    let base = daemon_url();
    let response = client
        .get(format!("{base}/indexes/{index}/status"))
        .send()
        .await
        .expect("read benchmark index ownership before mutation");
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return;
    }
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "cannot verify existing benchmark index ownership"
    );
    let body: serde_json::Value = response.json().await.expect("benchmark status JSON");
    let actual = body["root_path"]
        .as_str()
        .expect("benchmark status must identify root_path");
    validate_registered_root(Path::new(actual), expected).unwrap_or_else(|error| panic!("{error}"));
}

/// Resolve only an explicitly configured fixture, never the user's discovery file.
pub fn daemon_url() -> String {
    let url = std::env::var("TRUSTY_SEARCH_TEST_URL")
        .expect("set TRUSTY_SEARCH_TEST_URL to a dedicated isolated daemon; production fallback is disabled");
    let data_dir = std::env::var_os("TRUSTY_DATA_DIR")
        .expect("set TRUSTY_DATA_DIR to the dedicated daemon's disposable data directory");
    validate_data_dir(Path::new(&data_dir)).unwrap_or_else(|error| panic!("{error}"));
    let advertised = std::fs::read_to_string(PathBuf::from(data_dir).join("http_addr"))
        .expect("isolated daemon must publish TRUSTY_DATA_DIR/http_addr");
    validate_endpoint(&url, &advertised).unwrap_or_else(|error| panic!("{error}"));
    // Validate the fixture before even a health request, including read-only baselines.
    corpus_root("");
    url
}

/// Resolve a source directory in the fixture workspace, checking symlink escapes.
pub fn corpus_root(relative: &str) -> PathBuf {
    let root = std::env::var_os("TRUSTY_SEARCH_TEST_CORPUS_ROOT")
        .expect("set TRUSTY_SEARCH_TEST_CORPUS_ROOT to a marked disposable workspace copy");
    validate_corpus(Path::new(&root), relative).unwrap_or_else(|error| panic!("{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "trusty-benchmark-guard-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn rejects_production_nonlocal_and_mismatched_endpoints() {
        for (url, addr) in [
            ("http://127.0.0.1:7878", "127.0.0.1:7878"),
            ("http://example.com:17878", "example.com:17878"),
            ("http://127.0.0.1:17878", "127.0.0.1:17879"),
            ("http://127.0.0.1:0", "127.0.0.1:0"),
            ("https://127.0.0.1:17878", "127.0.0.1:17878"),
            ("http://127.0.0.1:17878/path", "127.0.0.1:17878/path"),
        ] {
            assert!(validate_endpoint(url, addr).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn accepts_matching_isolated_discovery_address() {
        assert!(validate_endpoint("http://127.0.0.1:17878", "127.0.0.1:17878\n").is_ok());
    }

    #[test]
    fn rejects_existing_index_registered_to_another_root() {
        let expected = Fixture::new();
        let unrelated = Fixture::new();
        assert!(validate_registered_root(&unrelated.0, &expected.0).is_err());
        assert!(validate_registered_root(&expected.0, &expected.0).is_ok());
    }

    #[test]
    fn rejects_unmarked_daemon_data_directory() {
        let fixture = Fixture::new();
        assert!(validate_data_dir(&fixture.0).is_err());
        std::fs::write(fixture.0.join(".trusty-search-test-daemon"), "").unwrap();
        assert!(validate_data_dir(&fixture.0).is_ok());
    }

    #[test]
    fn rejects_unmarked_corpus_and_real_checkout() {
        let fixture = Fixture::new();
        assert!(validate_corpus(&fixture.0, "").is_err());
        std::fs::write(fixture.0.join(".trusty-search-test-corpus"), "").unwrap();
        std::fs::write(fixture.0.join(".git"), "gitdir: checkout").unwrap();
        assert!(validate_corpus(&fixture.0, "").is_err());
    }

    #[test]
    fn accepts_existing_directory_inside_marked_copy() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join(".trusty-search-test-corpus"), "").unwrap();
        std::fs::create_dir(fixture.0.join("crates")).unwrap();
        assert_eq!(
            validate_corpus(&fixture.0, "crates").unwrap(),
            fixture.0.join("crates").canonicalize().unwrap()
        );
        assert!(validate_corpus(&fixture.0, "missing").is_err());
    }

    #[test]
    fn rejects_parent_escape() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join(".trusty-search-test-corpus"), "").unwrap();
        assert!(validate_corpus(&fixture.0, "..").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        std::fs::write(fixture.0.join(".trusty-search-test-corpus"), "").unwrap();
        std::os::unix::fs::symlink(&outside.0, fixture.0.join("crates")).unwrap();
        assert!(validate_corpus(&fixture.0, "crates").is_err());
    }
}
