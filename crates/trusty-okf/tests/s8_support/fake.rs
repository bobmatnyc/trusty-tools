//! A scripted fake trusty-search daemon for the S8 feed criteria (#10746),
//! adapted from the S7 fake in `feed_criteria.rs`: an in-memory corpus that
//! answers the methods the feed, the probe, the scope check and the
//! reindex gate send, with or without D1, and records every call.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use trusty_okf::feed::{IndexFeed, SearchError};
use trusty_okf::state::IndexIdentity;

pub const INDEX: &str = "kb-s8";

pub fn hex(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn ident(instance: &str, generation: u64) -> IndexIdentity {
    IndexIdentity {
        instance_id: instance.into(),
        generation,
    }
}

/// What the fake does differently from a well-behaved daemon.
#[derive(Default)]
pub struct Script {
    /// Every call fails as a transport error.
    pub down: bool,
    /// A put echoes a hash that is not the content's (D1 only).
    pub wrong_hash: bool,
    /// The `search.index.config.get` reply.
    pub config: Option<Value>,
}

pub struct Fake {
    pub root: PathBuf,
    pub identity: Mutex<Option<IndexIdentity>>,
    pub corpus: Mutex<BTreeMap<String, Vec<String>>>,
    hashes: Mutex<BTreeMap<String, String>>,
    calls: Mutex<Vec<(String, Value)>>,
    pub script: Mutex<Script>,
}

impl Fake {
    pub fn new(root: &Path, d1: bool) -> Fake {
        Fake {
            root: root.to_path_buf(),
            identity: Mutex::new(d1.then(|| ident("inst-1", 1))),
            corpus: Mutex::default(),
            hashes: Mutex::default(),
            calls: Mutex::default(),
            script: Mutex::default(),
        }
    }

    pub fn script(&self, f: impl FnOnce(&mut Script)) {
        f(&mut self.script.lock().unwrap());
    }

    pub fn calls(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    pub fn clear_calls(&self) {
        self.calls.lock().unwrap().clear();
    }

    /// Every pushed path; none may lie under `_sources/` or `_state/`
    /// (criteria 129-135).
    pub fn assert_no_bookkeeping_puts(&self) {
        for p in self.calls("search.index.file.put") {
            let path = p["body"]["path"].as_str().unwrap_or_default();
            assert!(
                !path.starts_with("_sources") && !path.starts_with("_state"),
                "bookkeeping pushed: {path}"
            );
        }
    }

    pub fn seed(&self, wire: &str, text: &str) {
        self.corpus
            .lock()
            .unwrap()
            .entry(wire.to_string())
            .or_default()
            .push(text.to_string());
    }

    pub fn chunks_of(&self, wire: &str) -> Vec<String> {
        self.corpus
            .lock()
            .unwrap()
            .get(wire)
            .cloned()
            .unwrap_or_default()
    }

    fn under(&self, prefix: &str) -> Vec<Value> {
        let corpus = self.corpus.lock().unwrap();
        corpus
            .iter()
            .filter(|(p, _)| *p == prefix || p.starts_with(&format!("{prefix}/")))
            .flat_map(|(p, cs)| cs.iter().map(move |c| (p.clone(), c.clone())))
            .map(|(p, c)| {
                json!({"path": p, "file": self.root.join(&p), "content": c,
                       "start_line": 1, "end_line": 1, "score": 1.0})
            })
            .collect()
    }

    fn handle(&self, method: &str, p: &Value) -> Result<Value, SearchError> {
        let err = |code: Option<i64>, message: &str| SearchError {
            method: method.to_string(),
            code,
            message: message.to_string(),
        };
        let s = self.script.lock().unwrap();
        if s.down {
            return Err(err(None, "connection refused"));
        }
        let identity = self.identity.lock().unwrap().clone();
        let with_identity = |mut v: Value| {
            if let Some(i) = &identity {
                v["identity"] = json!(i);
            }
            v
        };
        let path = p["body"]["path"].as_str().unwrap_or_default().to_string();
        match method {
            "search.index.status" => Ok(with_identity(
                json!({"index_id": INDEX, "root_path": self.root}),
            )),
            "search.index.config.get" => Ok(s.config.clone().unwrap_or_else(|| {
                json!({"extensions": ["md"], "exclude_globs": [], "extra_skip_dirs": [],
                       "invalid_exclude_globs": []})
            })),
            "search.index.reindex" => Ok(json!({"queued": true})),
            "search.chunks.list" => {
                let all = self.under(p["path_prefix"].as_str().unwrap_or_default());
                let off = p["offset"].as_u64().unwrap_or(0) as usize;
                let lim = p["limit"].as_u64().unwrap_or(100) as usize;
                let page: Vec<Value> = all.iter().skip(off).take(lim).cloned().collect();
                Ok(json!({"total": all.len(), "chunks": page}))
            }
            "search.query" => {
                let prefix = p["body"]["path_prefix"].as_str().unwrap_or_default();
                let words: Vec<String> = p["body"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(str::to_lowercase)
                    .collect();
                let hits: Vec<Value> = self
                    .under(prefix)
                    .into_iter()
                    .filter(|c| {
                        let t = c["content"].as_str().unwrap_or_default().to_lowercase();
                        words.iter().any(|w| t.contains(w.as_str()))
                    })
                    .collect();
                Ok(json!({ "results": hits }))
            }
            "search.index.file.put" => {
                let content = p["body"]["content"].as_str().unwrap_or_default();
                let echoed = if s.wrong_hash {
                    hex("other")
                } else {
                    hex(content)
                };
                let paras: Vec<String> = content
                    .split("\n\n")
                    .filter(|x| !x.trim().is_empty())
                    .map(str::to_string)
                    .collect();
                self.corpus.lock().unwrap().insert(path.clone(), paras);
                self.hashes
                    .lock()
                    .unwrap()
                    .insert(path.clone(), hex(content));
                let mut reply = json!({"path": path, "indexed": true});
                if identity.is_some() {
                    reply["content_sha256"] = json!(echoed);
                }
                Ok(with_identity(reply))
            }
            "search.index.file.remove" => {
                self.corpus.lock().unwrap().remove(&path);
                self.hashes.lock().unwrap().remove(&path);
                Ok(json!({"path": path, "removed": true}))
            }
            "search.index.files.remove" => {
                let prefix = p["body"]["path_prefix"].as_str().unwrap_or_default();
                let mut corpus = self.corpus.lock().unwrap();
                corpus.retain(|k, _| !(k == prefix || k.starts_with(&format!("{prefix}/"))));
                Ok(json!({"removed": true}))
            }
            "search.index.file.status" if identity.is_some() => {
                let present = !self.chunks_of(&path).is_empty();
                let hash = self.hashes.lock().unwrap().get(&path).cloned();
                Ok(with_identity(json!({"path": path, "present": present,
                    "content_sha256": if present { json!(hash) } else { Value::Null },
                    "chunk_count": self.chunks_of(&path).len()})))
            }
            _ => Err(err(Some(-32601), "method not found")),
        }
    }
}

#[async_trait::async_trait]
impl IndexFeed for Fake {
    async fn call(&self, method: &str, params: Value) -> Result<Value, SearchError> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone()));
        self.handle(method, &params)
    }
}

/// Paths the fake was asked to remove one at a time.
pub fn removed_paths(f: &Fake) -> BTreeSet<String> {
    f.calls("search.index.file.remove")
        .iter()
        .map(|p| p["body"]["path"].as_str().unwrap_or_default().to_string())
        .collect()
}
