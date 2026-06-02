//! Shared Atlassian credentials for the JIRA + Confluence live sources (#550).
//!
//! Why: JIRA and Confluence are both Atlassian Cloud products that share one
//! credential set (email + API token + site base URL).  Resolving them once,
//! here, keeps the two sources DRY and — critically — matches the env-var names
//! code-intelligence already uses so an existing deployment migrates drop-in
//! (RESOLVED decision on #550: reuse `ATLASSIAN_API_TOKEN` et al.).
//!
//! ## Env-var contract (matched to code-intelligence / Duetto cto reference)
//!
//! Token  (any of, first non-empty wins):
//!   - `ATLASSIAN_API_TOKEN`   (canonical; what extract_confluence_authors.py reads)
//!   - `ATLASSIAN_PAT`         (alias used in the cto `.env.local.example`)
//!   - per-product `JIRA_API_TOKEN` / `CONFLUENCE_API_TOKEN` (product override)
//!
//! Email  (any of):
//!   - `ATLASSIAN_EMAIL`       (canonical)
//!   - per-product `JIRA_EMAIL` / `CONFLUENCE_EMAIL`
//!
//! Base URL (any of):
//!   - `ATLASSIAN_URL`         (canonical site root, e.g. https://acme.atlassian.net)
//!   - per-product `JIRA_BASE_URL` / `JIRA_URL` / `CONFLUENCE_BASE_URL` / `CONFLUENCE_URL`
//!
//! Auth is HTTP Basic (email:token), exactly as the reference Python uses
//! `HTTPBasicAuth(ATLASSIAN_EMAIL, ATLASSIAN_API_TOKEN)`.  JIRA REST lives at
//! `{base}/rest/api/3/...`; Confluence REST at `{base}/wiki/rest/api/...`.
//!
//! No secret values are read from any file — only env-var *names* were matched
//! against the cto reference; values come from the process environment.
//!
//! What: `AtlassianCreds::from_env` (canonical-only) and
//! `AtlassianCreds::from_env_for` (canonical + product fallbacks) resolve the
//! three pieces and expose `basic_auth_header` for the `Authorization` header.
//!
//! Test: `creds_resolve_from_canonical`, `creds_product_fallback`,
//! `creds_missing_when_no_token`, `basic_auth_header_encodes` in this module.

use base64::Engine;

/// Which Atlassian product a credential lookup is for (selects fallback env vars).
///
/// Why: JIRA and Confluence allow product-specific env overrides (`JIRA_*` /
/// `CONFLUENCE_*`) layered under the shared `ATLASSIAN_*` canon; the product tag
/// selects the right fallback names.
/// What: a two-variant enum used only by `from_env_for`.
/// Test: `creds_product_fallback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlassianProduct {
    /// JIRA — fallback env prefix `JIRA_`.
    Jira,
    /// Confluence — fallback env prefix `CONFLUENCE_`.
    Confluence,
}

/// Resolved Atlassian credentials (email + token + base URL).
///
/// Why: both sources need the same triple to build the basic-auth header and
/// the REST base; bundling them keeps the source code small and makes the
/// "missing creds → skip" decision a single `Option` check.
/// What: all three fields are non-empty by construction (`from_env*` returns
/// `None` if any is missing).  `base_url` has its trailing slash trimmed.
/// Test: `creds_resolve_from_canonical`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtlassianCreds {
    /// Account email (basic-auth username).
    pub email: String,
    /// API token (basic-auth password).
    pub token: String,
    /// Site base URL, e.g. `https://acme.atlassian.net` (no trailing slash).
    pub base_url: String,
}

impl AtlassianCreds {
    /// Resolve from the canonical `ATLASSIAN_*` env vars only.
    ///
    /// Why: a convenience used by tests and any caller that does not need the
    /// product-specific fallbacks.
    /// What: reads `ATLASSIAN_EMAIL`, `ATLASSIAN_API_TOKEN` (or `ATLASSIAN_PAT`),
    /// and `ATLASSIAN_URL`; returns `None` if any is missing/empty.
    /// Test: `creds_resolve_from_canonical`, `creds_missing_when_no_token`.
    pub fn from_env() -> Option<Self> {
        Self::build(
            first_env(&["ATLASSIAN_EMAIL"]),
            first_env(&["ATLASSIAN_API_TOKEN", "ATLASSIAN_PAT"]),
            first_env(&["ATLASSIAN_URL"]),
        )
    }

    /// Resolve for a specific product with canonical + product-specific fallbacks.
    ///
    /// Why: matches code-intelligence, where some deployments set only the
    /// product-scoped vars (`JIRA_BASE_URL`, `CONFLUENCE_EMAIL`, …).  Canonical
    /// `ATLASSIAN_*` wins; the product vars fill any gap.
    /// What: builds the lookup order from the product tag, then resolves each of
    /// email/token/base from the first non-empty env var in its chain.
    /// Test: `creds_product_fallback`.
    pub fn from_env_for(product: AtlassianProduct) -> Option<Self> {
        let (email_keys, token_keys, base_keys): (&[&str], &[&str], &[&str]) = match product {
            AtlassianProduct::Jira => (
                &["ATLASSIAN_EMAIL", "JIRA_EMAIL"],
                &["ATLASSIAN_API_TOKEN", "ATLASSIAN_PAT", "JIRA_API_TOKEN"],
                &["ATLASSIAN_URL", "JIRA_BASE_URL", "JIRA_URL"],
            ),
            AtlassianProduct::Confluence => (
                &["ATLASSIAN_EMAIL", "CONFLUENCE_EMAIL"],
                &[
                    "ATLASSIAN_API_TOKEN",
                    "ATLASSIAN_PAT",
                    "CONFLUENCE_API_TOKEN",
                ],
                &["ATLASSIAN_URL", "CONFLUENCE_BASE_URL", "CONFLUENCE_URL"],
            ),
        };
        Self::build(
            first_env(email_keys),
            first_env(token_keys),
            first_env(base_keys),
        )
    }

    /// Build from three optional values; `None` if any is missing.
    ///
    /// Why: shared construction for both `from_env` variants, including the
    /// trailing-slash trim so callers can always `format!("{base}/...")`.
    /// What: returns `Some` only when all three are present and non-empty.
    /// Test: covered by `creds_resolve_from_canonical`, `creds_missing_when_no_token`.
    fn build(email: Option<String>, token: Option<String>, base: Option<String>) -> Option<Self> {
        let email = email?;
        let token = token?;
        let base = base?;
        Some(Self {
            email,
            token,
            base_url: base.trim_end_matches('/').to_string(),
        })
    }

    /// Produce the HTTP `Authorization: Basic …` header value.
    ///
    /// Why: Atlassian Cloud REST authenticates with HTTP Basic over
    /// `email:api_token` (NOT a bearer token); centralising the encoding avoids
    /// per-source mistakes.
    /// What: base64-encodes `"{email}:{token}"` and prefixes `Basic `.
    /// Test: `basic_auth_header_encodes`.
    pub fn basic_auth_header(&self) -> String {
        let raw = format!("{}:{}", self.email, self.token);
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
        format!("Basic {encoded}")
    }
}

/// Return the value of the first env var in `keys` that is set and non-empty.
///
/// Why: the canonical-then-fallback resolution order is the same for email,
/// token, and base URL; one helper keeps it consistent.
/// What: iterates `keys`, returns the first non-empty trimmed value, else `None`.
/// Test: covered by `creds_product_fallback`.
fn first_env(keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Ok(v) = std::env::var(k) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear() {
        unsafe {
            for k in [
                "ATLASSIAN_EMAIL",
                "ATLASSIAN_API_TOKEN",
                "ATLASSIAN_PAT",
                "ATLASSIAN_URL",
                "JIRA_EMAIL",
                "JIRA_API_TOKEN",
                "JIRA_BASE_URL",
                "JIRA_URL",
                "CONFLUENCE_EMAIL",
                "CONFLUENCE_API_TOKEN",
                "CONFLUENCE_BASE_URL",
                "CONFLUENCE_URL",
            ] {
                std::env::remove_var(k);
            }
        }
    }

    #[test]
    #[serial]
    fn creds_resolve_from_canonical() {
        clear();
        unsafe {
            std::env::set_var("ATLASSIAN_EMAIL", "bob@acme.com");
            std::env::set_var("ATLASSIAN_API_TOKEN", "tok123"); // pragma: allowlist secret
            std::env::set_var("ATLASSIAN_URL", "https://acme.atlassian.net/");
        }
        let creds = AtlassianCreds::from_env().expect("creds present");
        assert_eq!(creds.email, "bob@acme.com");
        assert_eq!(creds.token, "tok123");
        // Trailing slash trimmed.
        assert_eq!(creds.base_url, "https://acme.atlassian.net");
        clear();
    }

    #[test]
    #[serial]
    fn creds_pat_alias_resolves_token() {
        clear();
        unsafe {
            std::env::set_var("ATLASSIAN_EMAIL", "bob@acme.com");
            std::env::set_var("ATLASSIAN_PAT", "pat999"); // pragma: allowlist secret
            std::env::set_var("ATLASSIAN_URL", "https://acme.atlassian.net");
        }
        let creds = AtlassianCreds::from_env().expect("creds present via PAT alias");
        assert_eq!(creds.token, "pat999");
        clear();
    }

    #[test]
    #[serial]
    fn creds_product_fallback() {
        clear();
        // No canonical email/base; only JIRA-scoped vars present.
        unsafe {
            std::env::set_var("JIRA_EMAIL", "jira@acme.com");
            std::env::set_var("ATLASSIAN_API_TOKEN", "tok"); // pragma: allowlist secret
            std::env::set_var("JIRA_BASE_URL", "https://acme.atlassian.net");
        }
        let creds = AtlassianCreds::from_env_for(AtlassianProduct::Jira).expect("jira creds");
        assert_eq!(creds.email, "jira@acme.com");
        assert_eq!(creds.base_url, "https://acme.atlassian.net");
        // Canonical-only resolution would fail (no ATLASSIAN_EMAIL/URL).
        assert!(AtlassianCreds::from_env().is_none());
        clear();
    }

    #[test]
    #[serial]
    fn creds_canonical_beats_product() {
        clear();
        unsafe {
            std::env::set_var("ATLASSIAN_URL", "https://canon.atlassian.net");
            std::env::set_var("CONFLUENCE_URL", "https://product.atlassian.net");
            std::env::set_var("ATLASSIAN_EMAIL", "bob@acme.com");
            std::env::set_var("ATLASSIAN_API_TOKEN", "t"); // pragma: allowlist secret
        }
        let creds =
            AtlassianCreds::from_env_for(AtlassianProduct::Confluence).expect("creds present");
        assert_eq!(creds.base_url, "https://canon.atlassian.net");
        clear();
    }

    #[test]
    #[serial]
    fn creds_missing_when_no_token() {
        clear();
        unsafe {
            std::env::set_var("ATLASSIAN_EMAIL", "bob@acme.com");
            std::env::set_var("ATLASSIAN_URL", "https://acme.atlassian.net");
            // No token set.
        }
        assert!(AtlassianCreds::from_env().is_none());
        clear();
    }

    #[test]
    fn basic_auth_header_encodes() {
        let creds = AtlassianCreds {
            email: "user@x.com".to_string(),
            token: "secret".to_string(), // pragma: allowlist secret
            base_url: "https://x.atlassian.net".to_string(),
        };
        let header = creds.basic_auth_header();
        assert!(header.starts_with("Basic "));
        // "user@x.com:secret" base64 == "dXNlckB4LmNvbTpzZWNyZXQ="
        assert_eq!(header, "Basic dXNlckB4LmNvbTpzZWNyZXQ=");
    }
}
