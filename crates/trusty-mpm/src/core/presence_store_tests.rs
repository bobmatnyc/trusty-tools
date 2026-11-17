//! Tests for the shared presence file helpers (#8878 PR2b-1a).

use std::os::unix::fs::PermissionsExt as _;

use super::*;

#[test]
fn an_owner_only_read_refuses_links_modes_and_size() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path();
    let file = base.join("f");
    crate::core::architect_session::write_owner_only(&file, b"hello").expect("write");
    assert_eq!(read_owner_only(&[base], &file, 5), Ok(b"hello".to_vec()));
    assert_eq!(
        read_owner_only(&[base], &file, 4),
        Err(FileFault::Oversized)
    );
    assert_eq!(
        read_owner_only(&[base], &base.join("none"), 5),
        Err(FileFault::Missing)
    );
    let link = base.join("link");
    std::os::unix::fs::symlink(&file, &link).expect("symlink");
    assert_eq!(
        read_owner_only(&[base], &link, 5),
        Err(FileFault::NotRegularFile)
    );
    let linked_dir = base.join("linked-dir");
    std::os::unix::fs::symlink(base, &linked_dir).expect("symlink dir");
    assert_eq!(
        read_owner_only(&[&linked_dir], &linked_dir.join("f"), 5),
        Err(FileFault::Unreadable)
    );
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).expect("chmod");
    assert_eq!(
        read_owner_only(&[base], &file, 5),
        Err(FileFault::UnsafeMode)
    );
}

#[test]
fn sealed_messages_are_unambiguous() {
    assert_ne!(
        sealed_message("t", &["ab", "c"]),
        sealed_message("t", &["a", "bc"])
    );
    assert_ne!(sealed_message("t", &["a"]), sealed_message("u", &["a"]));
    assert_eq!(sealed_message("t", &["ab"]), b"t\x002:ab".to_vec());
}

#[test]
fn tokens_with_path_characters_are_refused() {
    assert!(valid_token("cred-A_9", 16));
    for bad in ["", "../x", "a/b", "a.b", "a b", "aaaaaaaaaaaaaaaaa"] {
        assert!(!valid_token(bad, 16), "{bad:?}");
    }
    assert_eq!(random_hex::<16>().expect("random").len(), 32);
}
