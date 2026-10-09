//! #9214: trusty-analyze reaches trusty-search over its Unix socket, at the path
//! `TRUSTY_SEARCH_SOCKET` names.
//!
//! Why a binary-level test: the env override is read where the CLI builds its
//! client, so only a real `trusty-analyze` process proves the override reaches
//! the call. Before #9214 the binary dialled `http://127.0.0.1:7878` and never
//! looked at the variable.
//! Test: this *is* the test file.

#[path = "support/fake_search.rs"]
mod fake_search;

use fake_search::FakeSearchSocket;

/// `trusty-analyze health` probes trusty-search at `TRUSTY_SEARCH_SOCKET`.
///
/// Why: the analyzer must honour the same socket override every other
/// trusty-search client does, so a test rig or an operator can point it at a
/// daemon on a non-standard path.
/// What: serves a fake socket in a tempdir, runs `health` with the env var set
/// and the analyzer's own socket and update check isolated, then asserts the
/// fake saw `search.health` and the CLI reported trusty-search as up.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_reaches_trusty_search_at_the_socket_the_env_names() {
    let search = FakeSearchSocket::healthy();
    let socket = search.path().to_path_buf();
    let dir = tempfile::tempdir().expect("tempdir");
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_trusty-analyze"))
            .arg("--facts-path")
            .arg(dir.path().join("facts.redb"))
            .arg("health")
            .env("TRUSTY_SEARCH_SOCKET", &socket)
            .env_remove("TRUSTY_SEARCH_URL")
            .env("TRUSTY_ANALYZE_SOCKET", dir.path().join("analyze.sock"))
            .env("TRUSTY_NO_UPDATE_CHECK", "1")
            .output()
            .expect("run trusty-analyze health")
    })
    .await
    .expect("join the health run");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        search.methods().iter().any(|m| m == "search.health"),
        "the fake socket at TRUSTY_SEARCH_SOCKET must receive search.health; \
         it saw {:?}; stdout: {stdout}; stderr: {}",
        search.methods(),
        String::from_utf8_lossy(&out.stderr)
    );
    let line = stdout
        .lines()
        .find(|l| l.starts_with("trusty-search"))
        .unwrap_or_default();
    assert!(
        line.ends_with(": OK") && line.contains(&search.path().display().to_string()),
        "health must report trusty-search up at the socket path, got: {line:?}"
    );
}
