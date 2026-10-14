//! Public constructors for the engine structs other crates build (#9278 A2a).
//!
//! Why: #9278 A2b marks these structs `#[non_exhaustive]`, which forbids a
//! struct literal outside trusty-memory. Each one needs a constructor a
//! dependent crate can call instead, and this file calls them from outside the
//! crate, the same way trusty-mpm and trusty-agents do.
//! What: one test per constructor. Each pins the defaults the constructor
//! documents and the fields its arguments set.
//! Test: this file.

use std::path::PathBuf;

use chrono::Utc;
use trusty_common::chat_attachments::{Attachment, ImageRef};
use trusty_memory::engine::palace::{Palace, PalaceId};
use trusty_memory::engine::retrieval::VerifyNotStored;
use trusty_memory::engine::store::chat_sessions::ChatMessage;
use trusty_memory::engine::store::kg::Triple;
use uuid::Uuid;

#[test]
fn palace_new_has_no_description_and_a_current_creation_time() {
    let before = Utc::now();
    let palace = Palace::new(PalaceId::new("p1"), "Palace one", PathBuf::from("/tmp/p1"));
    let after = Utc::now();

    assert_eq!(palace.id, PalaceId::new("p1"));
    assert_eq!(palace.name, "Palace one");
    assert_eq!(palace.data_dir, PathBuf::from("/tmp/p1"));
    assert_eq!(palace.description, None);
    assert!(
        before <= palace.created_at && palace.created_at <= after,
        "created_at {} is outside [{before}, {after}]",
        palace.created_at
    );

    let described = palace.with_description(Some("notes".to_string()));
    assert_eq!(described.description.as_deref(), Some("notes"));
    assert_eq!(described.name, "Palace one", "other fields are kept");
}

#[test]
fn triple_new_is_a_current_full_confidence_fact_without_provenance() {
    let before = Utc::now();
    let triple = Triple::new("engine", "lives_in", "trusty-memory");
    let after = Utc::now();

    assert_eq!(
        (
            triple.subject.as_str(),
            triple.predicate.as_str(),
            triple.object.as_str()
        ),
        ("engine", "lives_in", "trusty-memory")
    );
    assert!(before <= triple.valid_from && triple.valid_from <= after);
    assert_eq!(triple.valid_to, None);
    assert_eq!(triple.confidence, 1.0);
    assert_eq!(triple.provenance, None);
}

#[test]
fn chat_message_new_has_no_attachments_until_with_attachments() {
    let message = ChatMessage::new("user", "hello");
    assert_eq!(message.role, "user");
    assert_eq!(message.content, "hello");
    assert!(message.attachments.is_empty());

    let image = Attachment::Image {
        name: "fixture.png".into(),
        mime_type: "image/png".into(),
        image: ImageRef {
            asset_id: "asset-1".into(),
        },
    };
    let with = message.with_attachments(vec![image.clone()]);
    assert_eq!(with.attachments, vec![image]);
    assert_eq!(with.content, "hello", "other fields are kept");
}

#[test]
fn verify_not_stored_new_carries_the_drawer_and_reason() {
    let drawer = Uuid::new_v4();
    let err = VerifyNotStored::new(drawer, "kg write failed");

    assert_eq!(err.drawer, drawer);
    assert_eq!(err.reason, "kg write failed");
    let text = err.to_string();
    assert!(
        text.contains(&drawer.to_string()) && text.contains("kg write failed"),
        "{text}"
    );
}
