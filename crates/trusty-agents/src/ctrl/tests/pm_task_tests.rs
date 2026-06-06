//! Tests for the `pm_task` conversational helpers (`extract_name_from_input`,
//! `match_any_glob`).

use super::super::pm_task::{extract_name_from_input, match_any_glob};

#[test]
fn match_any_glob_handles_suffix_wildcard() {
    let patterns = vec!["mcp_*".to_string(), "git_log".to_string()];
    assert!(match_any_glob("mcp_list", &patterns));
    assert!(match_any_glob("mcp_enable", &patterns));
    assert!(match_any_glob("mcp_", &patterns));
    assert!(match_any_glob("git_log", &patterns));
    assert!(!match_any_glob("git_status", &patterns));
    assert!(!match_any_glob("shell_exec", &patterns));
    assert!(!match_any_glob("anything", &[]));
}

#[test]
fn extract_name_from_input_im_bob() {
    assert_eq!(extract_name_from_input("I'm Bob"), Some("Bob".to_string()));
}

#[test]
fn extract_name_from_input_my_name_is_alice() {
    assert_eq!(
        extract_name_from_input("My name is Alice"),
        Some("Alice".to_string())
    );
}

#[test]
fn extract_name_from_input_bare_name() {
    assert_eq!(extract_name_from_input("Bob"), Some("Bob".to_string()));
    assert_eq!(extract_name_from_input("bob"), Some("Bob".to_string()));
}

#[test]
fn extract_name_from_input_call_me_sam() {
    assert_eq!(
        extract_name_from_input("call me Sam"),
        Some("Sam".to_string())
    );
}

#[test]
fn extract_name_from_input_im_alice_lower() {
    assert_eq!(
        extract_name_from_input("im alice"),
        Some("Alice".to_string())
    );
}

#[test]
fn extract_name_from_input_rejects_task_requests() {
    assert_eq!(extract_name_from_input("write me code"), None);
    assert_eq!(extract_name_from_input("build a python script"), None);
}

#[test]
fn extract_name_from_input_rejects_greetings() {
    assert_eq!(extract_name_from_input("Hello"), None);
    assert_eq!(extract_name_from_input("hi"), None);
    assert_eq!(extract_name_from_input("hey"), None);
    assert_eq!(extract_name_from_input("thanks"), None);
}

#[test]
fn extract_name_from_input_rejects_im_filler() {
    assert_eq!(extract_name_from_input("I'm here"), None);
    assert_eq!(extract_name_from_input("I'm fine"), None);
}
