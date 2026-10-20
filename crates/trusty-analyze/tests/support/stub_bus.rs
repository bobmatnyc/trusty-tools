//! A stub trusty-events ingest socket for the LSP producer tests (#9078, L4).
//!
//! Why: the producer must be proven against the real push wire (hardened UDS,
//! newline-delimited `HarnessEvent` frames) without a dev-dependency on
//! `trusty-events`, which the workspace forbids.
//! What: binds a hardened `UnixListener` in its own tempdir and appends every
//! byte each connection writes, to EOF, to one buffer. [`StubBus::frames`]
//! parses that buffer back into `HarnessEvent`s; [`StubBus::raw`] keeps the
//! bytes for the privacy assertions.
//! Test: used by the `analyze.lsp_*` event tests in `tests/lsp_rpc.rs`.

#![allow(dead_code)] // each test binary compiles this module and uses a subset

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use trusty_common::control_bus::HarnessEvent;

/// One stub bus; its accept loop stops when this is dropped.
pub struct StubBus {
    _dir: tempfile::TempDir,
    path: PathBuf,
    bytes: Arc<Mutex<Vec<u8>>>,
    task: tokio::task::JoinHandle<()>,
}

impl StubBus {
    /// Bind in a fresh tempdir and record everything pushed to it.
    pub fn bind() -> Self {
        let dir = tempfile::tempdir().expect("tempdir for the stub bus");
        let path = dir.path().join("bus.sock");
        let listener = trusty_common::uds::bind_hardened(&path).expect("bind the stub bus");
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&bytes);
        let task = tokio::spawn(async move {
            while let Ok((mut conn, _)) = listener.accept().await {
                let sink = Arc::clone(&sink);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let _ = conn.read_to_end(&mut buf).await;
                    sink.lock().expect("sink").extend_from_slice(&buf);
                });
            }
        });
        Self {
            _dir: dir,
            path,
            bytes,
            task,
        }
    }

    /// A socket path in a fresh tempdir that nothing listens on.
    pub fn absent_path(dir: &Path) -> PathBuf {
        dir.join("no-bus.sock")
    }

    /// The socket the producer dials.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every byte received so far.
    pub fn raw(&self) -> Vec<u8> {
        self.bytes.lock().expect("bytes").clone()
    }

    /// Every frame received so far, parsed.
    pub fn frames(&self) -> Vec<HarnessEvent> {
        let raw = self.raw();
        String::from_utf8(raw)
            .expect("frames are UTF-8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("each line is one HarnessEvent"))
            .collect()
    }

    /// Wait (bounded) until at least `n` frames have arrived.
    pub async fn wait_for(&self, n: usize) -> Vec<HarnessEvent> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let frames = self.frames();
            if frames.len() >= n || tokio::time::Instant::now() >= deadline {
                return frames;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for StubBus {
    fn drop(&mut self) {
        self.task.abort();
    }
}
