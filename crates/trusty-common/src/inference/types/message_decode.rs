//! Typed text/image decoding corresponding to ChatMessage's provider serialization (#7370).
use super::{ChatMessage, ToolCall};
use crate::chat_attachments::ImageContent;
use serde::{Deserialize, Deserializer};

impl<'de> Deserialize<'de> for ChatMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            role: String,
            content: Option<serde_json::Value>,
            #[serde(default)]
            images: Vec<ImageContent>,
            tool_calls: Option<Vec<ToolCall>>,
            tool_call_id: Option<String>,
            name: Option<String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let mut images = wire.images;
        let content = match wire.content {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(text)) => Some(text),
            Some(serde_json::Value::Array(parts)) => {
                let mut text = Vec::new();
                for part in parts {
                    match part["type"].as_str() {
                        Some("text") => text.push(
                            part["text"]
                                .as_str()
                                .ok_or_else(|| serde::de::Error::custom("Missing text content"))?
                                .to_owned(),
                        ),
                        Some("image_url") => {
                            let url = part
                                .pointer("/image_url/url")
                                .and_then(|v| v.as_str())
                                .ok_or_else(|| serde::de::Error::custom("Missing image URL"))?;
                            images.push(
                                ImageContent::from_data_url(url)
                                    .map_err(serde::de::Error::custom)?,
                            );
                        }
                        _ => {
                            return Err(serde::de::Error::custom(
                                "Unsupported message content part",
                            ));
                        }
                    }
                }
                (!text.is_empty()).then(|| text.join("\n"))
            }
            _ => return Err(serde::de::Error::custom("Unsupported message content")),
        };
        if !images.is_empty() && wire.role != "user" {
            return Err(serde::de::Error::custom("Images require a user message"));
        }
        Ok(Self {
            role: wire.role,
            content,
            images,
            tool_calls: wire.tool_calls,
            tool_call_id: wire.tool_call_id,
            name: wire.name,
            cache_control: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_image_message_roundtrips_without_text_loss() {
        let mut message = ChatMessage::user("Describe this");
        message.images.push(ImageContent {
            mime_type: "image/png".into(),
            data_base64: "YWJj".into(),
        });
        let wire = serde_json::to_value(&message).unwrap();
        assert_eq!(
            wire["content"][1]["image_url"]["url"],
            "data:image/png;base64,YWJj"
        );
        assert_eq!(
            serde_json::from_value::<ChatMessage>(wire).unwrap(),
            message
        );
        assert!(
            serde_json::from_value::<ChatMessage>(
                serde_json::json!({"role":"user","content":[{"type":"audio"}]})
            )
            .is_err()
        );
    }
}
