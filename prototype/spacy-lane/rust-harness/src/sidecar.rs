//! Resident spaCy sidecar client — spawn once, reuse for every extraction.
//!
//! Why: the owner's premise for lane B is that keeping the model resident
//! amortises its load cost. This type is what makes that premise testable: the
//! child process is spawned once and its handle lives as long as the caller, so
//! `analyze` after the first pays only wire + parse cost.
//! What: spawns `<venv>/bin/python -m kg_pos_sidecar`, then speaks
//! newline-JSON-RPC-2.0 over its piped stdin/stdout. Blocking, single-flight —
//! the real daemon would need the reader/worker split `trusty_embed_sidecar`
//! already has, which this prototype deliberately does not reimplement.
//! Test: `harness eval` and `harness bench` both drive it end to end.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

/// One spaCy token as the sidecar reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct Token {
    #[allow(dead_code)]
    pub i: usize,
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub pos: String,
    #[allow(dead_code)]
    pub tag: String,
    /// spaCy's `is_oov`. Reported for completeness; see the report — with
    /// `en_core_web_sm` (no word vectors) this is `true` for EVERY token,
    /// including `the` and `is`, so it carries no signal.
    #[allow(dead_code)]
    pub oov: bool,
}

/// One `doc.noun_chunks` span.
#[derive(Debug, Clone, Deserialize)]
pub struct NounChunk {
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// Token index of the chunk's syntactic head — the head-noun re-walk target.
    pub root: usize,
    pub root_pos: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Doc {
    pub tokens: Vec<Token>,
    pub noun_chunks: Vec<NounChunk>,
}

#[derive(Deserialize)]
struct AnalyzeResult {
    docs: Vec<Doc>,
}

#[derive(Deserialize)]
struct Frame {
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
}

pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Sidecar {
    /// Spawn the sidecar and block until its first reply proves the model is loaded.
    ///
    /// Why: readiness must mean "can serve", not "process exists" — otherwise a
    /// cold-start measurement stops the clock before the model is usable.
    /// What: spawns the venv interpreter, then issues one `ping`, which the
    /// server can only answer after `_load_nlp()` returns.
    pub fn spawn(python: &str, project_dir: &str) -> Result<Self> {
        let mut child = Command::new(python)
            .arg("-m")
            .arg("kg_pos_sidecar")
            .current_dir(project_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning spaCy sidecar via {python}"))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin pipe"))?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| anyhow!("no stdout pipe"))?,
        );
        let mut sc = Sidecar {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        sc.request("ping", serde_json::json!({}))?;
        Ok(sc)
    }

    /// Issue an arbitrary method/params pair — used by the failure-mode probe
    /// to send something the Python handler will raise on.
    pub fn request_raw(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.request(method, params)
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        let req = serde_json::json!({
            "jsonrpc": "2.0", "method": method, "params": params, "id": id
        });
        writeln!(self.stdin, "{req}")?;
        self.stdin.flush()?;

        let mut line = String::new();
        let n = self.stdout.read_line(&mut line)?;
        if n == 0 {
            return Err(anyhow!("sidecar closed stdout (died) during {method}"));
        }
        let frame: Frame = serde_json::from_str(&line)
            .with_context(|| format!("decoding sidecar frame: {line}"))?;
        if let Some(err) = frame.error {
            return Err(anyhow!("sidecar error during {method}: {err}"));
        }
        frame
            .result
            .ok_or_else(|| anyhow!("frame had neither result nor error"))
    }

    /// Parse `texts`, returning one [`Doc`] each, in order.
    pub fn analyze(&mut self, texts: &[&str]) -> Result<Vec<Doc>> {
        let raw = self.request("analyze", serde_json::json!({ "texts": texts }))?;
        let parsed: AnalyzeResult = serde_json::from_value(raw)?;
        Ok(parsed.docs)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
