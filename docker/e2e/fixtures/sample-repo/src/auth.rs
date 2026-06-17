// Sample authentication module for trusty-search indexing fixture.
// This file exists so the smoke test has something to search for.

use std::collections::HashMap;

/// Authenticate a user by checking the session token.
/// Returns the user id if authentication succeeds.
pub fn authenticate(token: &str, sessions: &HashMap<String, u64>) -> Option<u64> {
    sessions.get(token).copied()
}

/// Verify that a token meets minimum length requirements.
pub fn verify_token(token: &str) -> bool {
    token.len() >= 32
}

/// Generate a new session token from user credentials.
/// In production, use a cryptographically secure random generator.
pub fn generate_token(user_id: u64, secret: &str) -> String {
    format!("{user_id}:{secret}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticate_finds_valid_session() {
        let mut sessions = HashMap::new();
        sessions.insert("tok123".to_string(), 42u64);
        assert_eq!(authenticate("tok123", &sessions), Some(42));
    }

    #[test]
    fn authenticate_returns_none_for_unknown_token() {
        let sessions = HashMap::new();
        assert_eq!(authenticate("unknown", &sessions), None);
    }

    #[test]
    fn verify_token_rejects_short_tokens() {
        assert!(!verify_token("tooshort"));
    }
}
