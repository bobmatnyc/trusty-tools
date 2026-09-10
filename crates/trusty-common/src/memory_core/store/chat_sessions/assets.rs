//! Memory-owned image assets, scoped to a palace database and chat session (#7370).
use super::{ChatMessage, ChatSessionStore};
use crate::chat_attachments::{Attachment, decode_image, validate_metadata};
use redb::{ReadableDatabase, TableDefinition};
use serde::{Deserialize, Serialize};

const ASSETS: TableDefinition<&str, &[u8]> = TableDefinition::new("chat_image_assets");
#[derive(Clone, Serialize, Deserialize)]
pub struct ChatImageAsset {
    pub session_id: String,
    pub name: String,
    pub mime_type: String,
    pub data_base64: String,
}
impl ChatSessionStore {
    /// Why: image history must survive restarts without repeating bytes in every history page.
    /// What: validate and atomically store an image under a generated opaque ID in this palace.
    /// Test: `asset_ownership_and_restart_are_enforced`.
    pub fn put_chat_asset(&self, asset: ChatImageAsset) -> anyhow::Result<String> {
        anyhow::ensure!(
            !asset.session_id.is_empty() && asset.session_id.len() <= 256,
            "Invalid session ID"
        );
        anyhow::ensure!(
            self.get_session(&asset.session_id)?.is_some(),
            "Chat session does not exist"
        );
        validate_metadata(&[Attachment::Image {
            name: asset.name.clone(),
            mime_type: asset.mime_type.clone(),
            image: (),
        }])?;
        decode_image(&asset.mime_type, &asset.data_base64)?;
        let id = uuid::Uuid::new_v4().to_string();
        let bytes = serde_json::to_vec(&asset)?;
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(ASSETS)?;
            table.insert(id.as_str(), bytes.as_slice())?;
        }
        write.commit()?;
        Ok(id)
    }
    /// Why: an opaque ID alone must not authorize another session's image.
    /// What: require a live session, valid ID and matching stored owner; reject missing or oversized records.
    /// Test: `asset_ownership_and_restart_are_enforced`.
    pub fn get_chat_asset(&self, session: &str, id: &str) -> anyhow::Result<ChatImageAsset> {
        anyhow::ensure!(uuid::Uuid::parse_str(id).is_ok(), "Invalid asset ID");
        anyhow::ensure!(
            self.get_session(session)?.is_some(),
            "Chat session does not exist"
        );
        let read = self.db.begin_read()?;
        let table = read.open_table(ASSETS)?;
        let value = table
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("Chat asset not found"))?;
        anyhow::ensure!(
            value.value().len() <= 8 * 1024 * 1024,
            "Stored image exceeds bound"
        );
        let asset: ChatImageAsset = serde_json::from_slice(value.value())?;
        anyhow::ensure!(
            asset.session_id == session,
            "Chat asset does not belong to this session"
        );
        Ok(asset)
    }
    pub(super) fn validate_chat_attachments(
        &self,
        session: &str,
        messages: &[ChatMessage],
    ) -> anyhow::Result<()> {
        for message in messages {
            validate_metadata(&message.attachments)?;
            anyhow::ensure!(
                message.attachments.is_empty() || message.role == "user",
                "Only user messages may attach files"
            );
            let mut total = 0usize;
            for item in &message.attachments {
                if let Attachment::Image {
                    name,
                    mime_type,
                    image,
                } = item
                {
                    let stored = self.get_chat_asset(session, &image.asset_id)?;
                    use base64::Engine;
                    total += base64::engine::general_purpose::STANDARD
                        .decode(&stored.data_base64)?
                        .len();
                    anyhow::ensure!(
                        total <= crate::chat_attachments::MAX_TOTAL_BYTES,
                        "Chat turn images exceed 10 MiB"
                    );
                    anyhow::ensure!(
                        &stored.name == name && &stored.mime_type == mime_type,
                        "Asset metadata mismatch"
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asset_ownership_and_restart_are_enforced() {
        use base64::Engine;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sessions.redb");
        let store = ChatSessionStore::open(&path).unwrap();
        store.upsert_session("one", &[]).unwrap();
        store.upsert_session("two", &[]).unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let asset = ChatImageAsset {
            session_id: "one".into(),
            name: "test.png".into(),
            mime_type: "image/png".into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(png.into_inner()),
        };
        let id = store.put_chat_asset(asset).unwrap();
        assert!(store.get_chat_asset("two", &id).is_err());
        let message = ChatMessage {
            role: "user".into(),
            content: "Describe".into(),
            attachments: vec![Attachment::Image {
                name: "test.png".into(),
                mime_type: "image/png".into(),
                image: crate::chat_attachments::ImageRef {
                    asset_id: id.clone(),
                },
            }],
        };
        store.append_message("one", message.clone()).unwrap();
        assert!(store.append_message("two", message).is_err());
        drop(store);
        let store = ChatSessionStore::open(&path).unwrap();
        assert_eq!(store.get_chat_asset("one", &id).unwrap().name, "test.png");
        assert_eq!(
            store.get_session("one").unwrap().unwrap().history[0]
                .attachments
                .len(),
            1
        );
        assert!(store.get_chat_asset("one", "../test.png").is_err());
    }
}
