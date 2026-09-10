//! Current trusty-search socket transport for the shared OKG index feed.
use super::index_feed::{IndexFeed, PUSH_TIMEOUT};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
pub struct RpcIndexFeed {
    socket: PathBuf,
}
impl RpcIndexFeed {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }
    async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        trusty_common::search_rpc::call_at(&self.socket, method, params, PUSH_TIMEOUT).await
    }
}
#[async_trait]
impl IndexFeed for RpcIndexFeed {
    async fn index_file(&self, index: &str, path: &str, content: &str) -> anyhow::Result<()> {
        self.call(
            "search.index.file.put",
            json!({"index_id":index,"body":{"path":path,"content":content}}),
        )
        .await?;
        Ok(())
    }
    async fn remove_file(&self, index: &str, path: &str) -> anyhow::Result<()> {
        self.call(
            "search.index.file.remove",
            json!({"index_id":index,"body":{"path":path}}),
        )
        .await?;
        Ok(())
    }
    async fn index_root(&self, index: &str) -> anyhow::Result<Option<PathBuf>> {
        let value = self
            .call(
                trusty_common::search_rpc::METHOD_INDEX_STATUS,
                json!({"index_id":index}),
            )
            .await?;
        let root = value
            .get("root_path")
            .and_then(Value::as_str)
            .filter(|s| Path::new(s).is_absolute())
            .ok_or_else(|| anyhow::anyhow!("Search did not return an absolute root for {index}"))?;
        Ok(Some(PathBuf::from(root)))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn socket_feed_uses_index_scoped_bodies() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let saved = calls.clone();
        let daemon = crate::uds_mock::spawn(move |method, params| {
            saved.lock().unwrap().push((method.to_owned(), params));
            Box::pin(async { Ok(json!({"root_path":"/fixture"})) })
        })
        .await;
        let feed = RpcIndexFeed::new(daemon.socket().to_path_buf());
        assert_eq!(
            feed.index_root("bound").await.unwrap(),
            Some(PathBuf::from("/fixture"))
        );
        feed.index_file("bound", "/fixture/note.md", "fixture")
            .await
            .unwrap();
        feed.remove_file("bound", "/fixture/note.md").await.unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls[1].0, "search.index.file.put");
        assert_eq!(calls[1].1["body"]["content"], "fixture");
        assert_eq!(calls[2].0, "search.index.file.remove");
        assert_eq!(calls[2].1["index_id"], "bound");
    }
}
