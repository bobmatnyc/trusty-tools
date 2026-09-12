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
    async fn relative(&self, index: &str, path: &str) -> anyhow::Result<String> {
        let root = self
            .index_root(index)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Index root missing"))?;
        let path = Path::new(path);
        let relative = path.strip_prefix(&root)?;
        anyhow::ensure!(
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "Invalid entity path"
        );
        Ok(relative.to_string_lossy().into_owned())
    }
    async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        trusty_common::search_rpc::call_at(&self.socket, method, params, PUSH_TIMEOUT).await
    }
}
#[async_trait]
impl IndexFeed for RpcIndexFeed {
    async fn index_file(&self, index: &str, path: &str, content: &str) -> anyhow::Result<()> {
        let relative = self.relative(index, path).await?;
        self.remove_file(index, path).await?;
        self.call(
            "search.index.file.put",
            json!({"index_id":index,"body":{"path":relative,"content":content}}),
        )
        .await?;
        Ok(())
    }
    async fn remove_file(&self, index: &str, path: &str) -> anyhow::Result<()> {
        let relative = self.relative(index, path).await?;
        // Withdraw legacy absolute IDs as well as the walker-compatible relative identity.
        self.call(
            "search.index.file.remove",
            json!({"index_id":index,"body":{"path":path}}),
        )
        .await?;
        self.call(
            "search.index.file.remove",
            json!({"index_id":index,"body":{"path":relative}}),
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
        let put = calls
            .iter()
            .find(|(m, _)| m == "search.index.file.put")
            .unwrap();
        assert_eq!(put.1["body"]["content"], "fixture");
        assert_eq!(put.1["body"]["path"], "note.md");
        assert!(
            calls
                .iter()
                .any(|(m, p)| m == "search.index.file.remove" && p["body"]["path"] == "note.md")
        );
        assert!(calls.iter().any(
            |(m, p)| m == "search.index.file.remove" && p["body"]["path"] == "/fixture/note.md"
        ));
    }
}
