//! The fixed GitHub token behind the linked-issue fetches (#9197, B2b).
//!
//! Why: Architect ruling Q1: `fetch_linked_issues` reads issues with the token
//! that already read the PR diff (`DiffSource::Github.token`), so it adds no
//! auth path. The tickets backend treats a blank token as "use the host's
//! `gh` login" (`select_auth`), a credential switch and an unsupervised
//! subprocess, so a blank token must never reach it (B2b amendment 1).
//! What: [`FixedTicketToken`] answers every `IntentTokenResolver::token`
//! call with its token, or `IsrError::NoToken` when that token is blank.
//! Test: `blank_token_is_no_token_and_spawns_nothing`.

use async_trait::async_trait;
use trusty_common::intent_source::{IntentTokenResolver, IsrError};

/// A token resolver that always answers with one token (#9197, B2b).
///
/// Why: the linked-issue fetch reuses the diff read's token; the trusty-common
/// `BackendTicketFetcher` takes a resolver, not a token.
/// What: `token` returns the held token for any owner and repo; an empty or
/// whitespace-only token is `IsrError::NoToken`, so the GitHub backend never
/// falls back to `gh auth token`.
/// Test: `blank_token_is_no_token_and_spawns_nothing`,
/// `ticket_fetcher_for_picks_by_diff_source_and_the_seam_wins`.
pub(crate) struct FixedTicketToken(pub(crate) String);

#[async_trait]
impl IntentTokenResolver for FixedTicketToken {
    async fn token(&self, _owner: &str, _repo: &str) -> Result<String, IsrError> {
        if self.0.trim().is_empty() {
            return Err(IsrError::NoToken(
                "no GitHub token for the linked-issue fetch".to_string(),
            ));
        }
        Ok(self.0.clone())
    }
}

impl std::fmt::Debug for FixedTicketToken {
    // #9197: never print the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FixedTicketToken([redacted])")
    }
}
