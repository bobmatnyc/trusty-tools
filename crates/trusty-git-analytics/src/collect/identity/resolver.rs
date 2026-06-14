//! Developer identity resolution.
//!
//! Given a raw `(name, email)` tuple observed in a git commit, resolve
//! it to a canonical identity using a three-tier strategy:
//! 1. Exact alias match against the configured aliases map.
//! 2. Fuzzy match against team member canonical emails/names using
//!    Jaro-Winkler similarity above a configurable threshold.
//! 3. Fall through and return the raw pair unchanged.

use std::collections::HashMap;

use rusqlite::params;
use strsim::jaro_winkler;
use tracing::debug;

use crate::core::config::TeamConfig;
use crate::core::db::Database;

/// Default Jaro-Winkler threshold for fuzzy identity matching.
pub const DEFAULT_SIMILARITY_THRESHOLD: f64 = 0.85;

/// Lower fuzzy-match threshold applied to *normalized* comparisons (email
/// local-part vs canonical name with punctuation stripped). The normalization
/// step removes a lot of cosmetic differences, so we accept a slightly
/// lower raw similarity score when matching on the normalized form.
pub const NORMALIZED_SIMILARITY_THRESHOLD: f64 = 0.82;

/// Normalize a string for fuzzy comparison by:
/// 1. Lowercasing
/// 2. Replacing `.`, `-`, `_` with spaces (common email/login separators)
/// 3. Collapsing repeated whitespace
///
/// Examples:
/// - `"Bob.Matsuoka"` → `"bob matsuoka"`
/// - `"alice_smith-c"` → `"alice smith c"`
/// - `"Bob   M"`       → `"bob m"`
fn normalize_for_fuzzy(s: &str) -> String {
    s.to_lowercase()
        .replace(['.', '-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract the local-part (before `@`) of an email address, lowercased.
/// Returns the whole input lowercased if no `@` is present.
fn email_local_part(email: &str) -> String {
    match email.find('@') {
        Some(i) => email[..i].to_lowercase(),
        None => email.to_lowercase(),
    }
}

/// Why: `IdentityResolver::upsert_author` and the suggester both need to ask
/// "does this email live under the configured canonical_domain?". Centralising
/// the check avoids subtle case- or `@`-prefix bugs at the two call sites.
/// What: returns `true` when `email`'s domain portion equals `domain`
/// (case-insensitive). Both inputs may include or omit a leading `@`.
/// Test: see `resolver_tests::email_domain_matches_basic`.
pub fn email_domain_matches(email: &str, domain: &str) -> bool {
    let needle = domain.trim().trim_start_matches('@').to_lowercase();
    if needle.is_empty() {
        return false;
    }
    match email.rfind('@') {
        Some(i) => email[i + 1..].to_lowercase() == needle,
        None => false,
    }
}

/// Resolves observed author identities to canonical `(name, email)` pairs.
pub struct IdentityResolver {
    /// Mapping of alias (lowercased name or email) → canonical name.
    aliases: HashMap<String, String>,
    /// Canonical members: `(canonical_name, canonical_email)`.
    members: Vec<(String, String)>,
    /// Threshold for accepting a fuzzy match.
    threshold: f64,
    /// Preferred email domain for canonical email selection (issue #349).
    ///
    /// When set, an inbound `(name, email)` pair that hashes to a new
    /// identity but observes another email under the same canonical name
    /// in the `authors` table will prefer the domain-matching variant as
    /// the stored canonical email. See [`Self::upsert_author`] for the
    /// selection policy.
    canonical_domain: Option<String>,
}

impl IdentityResolver {
    /// Construct a resolver from a [`TeamConfig`].
    pub fn new(team: Option<&TeamConfig>) -> Self {
        let mut aliases: HashMap<String, String> = HashMap::new();
        let mut members: Vec<(String, String)> = Vec::new();
        let mut canonical_domain: Option<String> = None;
        if let Some(team) = team {
            for (k, v) in &team.aliases {
                aliases.insert(k.to_lowercase(), v.clone());
            }
            for m in &team.members {
                members.push((m.name.clone(), m.email.clone()));
                for a in &m.aliases {
                    aliases.insert(a.to_lowercase(), m.name.clone());
                }
                // Also auto-register the canonical email as an alias to itself.
                aliases.insert(m.email.to_lowercase(), m.name.clone());
            }
            canonical_domain = team
                .canonical_domain
                .as_ref()
                .map(|d| d.trim().trim_start_matches('@').to_lowercase())
                .filter(|d| !d.is_empty());
        }
        Self {
            aliases,
            members,
            threshold: DEFAULT_SIMILARITY_THRESHOLD,
            canonical_domain,
        }
    }

    /// Construct a resolver from a flat `canonical_name → [aliases]` map.
    ///
    /// This is the format produced by [`crate::core::config::Config::resolved_aliases`]
    /// and matches the Python predecessor's `developer_aliases` YAML key.
    ///
    /// The first entry in each alias list (if any looks like an email — i.e.
    /// contains `@`) is treated as the canonical email; otherwise the
    /// canonical email is left blank.
    pub fn from_alias_map(map: &HashMap<String, Vec<String>>) -> Self {
        let mut aliases: HashMap<String, String> = HashMap::new();
        let mut members: Vec<(String, String)> = Vec::new();
        for (canon_name, alias_list) in map {
            // Pick the first email-looking alias as canonical email.
            let canon_email = alias_list
                .iter()
                .find(|a| a.contains('@'))
                .cloned()
                .unwrap_or_default();
            members.push((canon_name.clone(), canon_email.clone()));
            // Register canonical name + canonical email as self-aliases.
            aliases.insert(canon_name.to_lowercase(), canon_name.clone());
            if !canon_email.is_empty() {
                aliases.insert(canon_email.to_lowercase(), canon_name.clone());
            }
            for a in alias_list {
                aliases.insert(a.to_lowercase(), canon_name.clone());
            }
        }
        Self {
            aliases,
            members,
            threshold: DEFAULT_SIMILARITY_THRESHOLD,
            canonical_domain: None,
        }
    }

    /// Construct a resolver from a [`crate::core::config::Config`], preferring
    /// the Python-compatible `developer_aliases` map when present, falling
    /// back to `team.members`.
    pub fn from_config(config: &crate::core::config::Config) -> Self {
        let map = config.resolved_aliases();
        let mut resolver = if !map.is_empty() {
            Self::from_alias_map(&map)
        } else {
            Self::new(config.team.as_ref())
        };
        // Pull canonical_domain from team config even when developer_aliases
        // map is the primary identity source (the two YAML keys are
        // orthogonal — the domain policy belongs under team:).
        if resolver.canonical_domain.is_none() {
            if let Some(team) = config.team.as_ref() {
                resolver.canonical_domain = team
                    .canonical_domain
                    .as_ref()
                    .map(|d| d.trim().trim_start_matches('@').to_lowercase())
                    .filter(|d| !d.is_empty());
            }
        }
        resolver
    }

    /// Override the fuzzy-match threshold (0.0–1.0).
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }

    /// Register an alias → canonical-name mapping after construction.
    ///
    /// Used by external-system ingestion helpers (e.g.
    /// [`crate::collect::azdo::feed_azdo_users`]) to seed the resolver with
    /// directory-derived identities discovered at runtime. Aliases are
    /// stored lowercased; subsequent [`Self::resolve`] calls treat the
    /// canonical name as authoritative.
    ///
    /// If `canonical_name` matches an existing canonical name on a member
    /// in `members`, `resolve()` will return that member's
    /// canonical email. Otherwise the canonical name is preserved but no
    /// canonical email is registered (callers can resolve by name only).
    ///
    /// Empty `alias` or `canonical_name` values are ignored.
    ///
    /// If `canonical_name` is not already known as a member, a synthetic
    /// member entry is registered with the alias as its canonical email
    /// (if the alias looks like an email — i.e. contains `@`) so that
    /// [`Self::resolve`] can return the canonical pair. If no existing
    /// member is found and the alias is not an email, the synthetic
    /// member is registered with an empty email.
    pub fn add_alias(&mut self, alias: &str, canonical_name: &str) {
        let alias = alias.trim();
        let canonical = canonical_name.trim();
        if alias.is_empty() || canonical.is_empty() {
            return;
        }
        self.aliases
            .insert(alias.to_lowercase(), canonical.to_string());
        if self.find_member_by_name(canonical).is_none() {
            let canonical_email = if alias.contains('@') {
                alias.to_string()
            } else {
                String::new()
            };
            self.members.push((canonical.to_string(), canonical_email));
        }
    }

    /// Resolve a raw `(name, email)` pair to canonical form.
    ///
    /// Returns the input unchanged if no rule matches.
    pub fn resolve(&self, name: &str, email: &str) -> (String, String) {
        let email_lc = email.to_lowercase();
        let name_lc = name.to_lowercase();

        // 1. Exact alias on email
        if let Some(canon_name) = self.aliases.get(&email_lc) {
            if let Some((cn, ce)) = self.find_member_by_name(canon_name) {
                return (cn, ce);
            }
        }
        // 2. Exact alias on name
        if let Some(canon_name) = self.aliases.get(&name_lc) {
            if let Some((cn, ce)) = self.find_member_by_name(canon_name) {
                return (cn, ce);
            }
        }

        // 3. Fuzzy match against member names/emails (raw Jaro-Winkler).
        let mut best: Option<(f64, &(String, String))> = None;
        for m in &self.members {
            let s_name = jaro_winkler(&name_lc, &m.0.to_lowercase());
            let s_email = jaro_winkler(&email_lc, &m.1.to_lowercase());
            let score = s_name.max(s_email);
            if score >= self.threshold && best.map(|(b, _)| score > b).unwrap_or(true) {
                best = Some((score, m));
            }
        }
        if let Some((score, m)) = best {
            debug!(score, member = %m.0, "fuzzy identity match");
            return (m.0.clone(), m.1.clone());
        }

        // 4. Normalized fuzzy: compare the email local-part and the raw name
        //    against canonical names and member emails after stripping
        //    punctuation (`.`, `-`, `_`). This catches cases like
        //    `"Bob M" <bob.matsuoka@co.com>` → `"Bob Matsuoka"`, where the
        //    raw name is too short for Jaro-Winkler to clear 0.85 but the
        //    email local-part `bob.matsuoka` normalizes to `bob matsuoka`,
        //    which is an exact match for the canonical name.
        let name_norm = normalize_for_fuzzy(name);
        let local_norm = normalize_for_fuzzy(&email_local_part(email));
        let mut best_norm: Option<(f64, &(String, String))> = None;
        for m in &self.members {
            let canon_name_norm = normalize_for_fuzzy(&m.0);
            let canon_local_norm = normalize_for_fuzzy(&email_local_part(&m.1));
            // Try all pairings; take the best score for this member.
            let candidates = [
                jaro_winkler(&local_norm, &canon_name_norm),
                jaro_winkler(&local_norm, &canon_local_norm),
                jaro_winkler(&name_norm, &canon_name_norm),
                jaro_winkler(&name_norm, &canon_local_norm),
            ];
            let score = candidates.iter().cloned().fold(0.0_f64, f64::max);
            if score >= NORMALIZED_SIMILARITY_THRESHOLD
                && best_norm.map(|(b, _)| score > b).unwrap_or(true)
            {
                best_norm = Some((score, m));
            }
        }
        if let Some((score, m)) = best_norm {
            debug!(score, member = %m.0, "normalized fuzzy identity match");
            return (m.0.clone(), m.1.clone());
        }

        // 5. Fallback: return as-is.
        (name.to_string(), email.to_string())
    }

    /// Upsert an author into the `authors` table, returning the row id.
    ///
    /// Why: `tga collect` calls this once per observed `(name, email)` pair;
    /// it both registers new identities and routes commits to existing rows.
    /// What: resolves the inbound pair to a canonical form, applies the
    /// canonical-email policy (issue #349) when a configured
    /// [`Self::canonical_domain`] is set, and writes the row keyed on
    /// `canonical_email`.
    /// Test: see `resolver_tests::canonical_domain_prefers_org_email` and
    /// `resolver_tests::canonical_domain_routes_new_personal_email_to_existing_org_row`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::core::TgaError::DbError`] on SQL failure.
    pub fn upsert_author(
        &self,
        db: &Database,
        name: &str,
        email: &str,
    ) -> crate::core::Result<i64> {
        let (canon_name, mut canon_email) = self.resolve(name, email);

        // Issue #349 canonical-email policy:
        // 1. If `resolve()` already produced an email under the configured
        //    canonical_domain, we are done (team.members already mapped it).
        // 2. Otherwise, look for an existing authors row with the same
        //    `canonical_name` whose email lives under canonical_domain and
        //    reuse that as the canonical email (so all future commits route
        //    to the org-domain row instead of creating a personal-email
        //    duplicate).
        // 3. Failing that, fall back to the resolved email (first-seen).
        let conn = db.connection();
        if let Some(domain) = &self.canonical_domain {
            if !email_domain_matches(&canon_email, domain) {
                let alt: Option<String> = conn
                    .query_row(
                        "SELECT canonical_email FROM authors \
                         WHERE LOWER(canonical_name) = LOWER(?1) \
                           AND LOWER(SUBSTR(canonical_email, INSTR(canonical_email, '@') + 1)) = ?2 \
                         LIMIT 1",
                        params![canon_name, domain],
                        |row| row.get::<_, String>(0),
                    )
                    .ok();
                if let Some(found) = alt {
                    debug!(
                        prior_email = %canon_email,
                        chosen_email = %found,
                        domain = %domain,
                        "canonical_domain policy routed commit to existing org-domain identity"
                    );
                    canon_email = found;
                }
            }
        }

        conn.execute(
            "INSERT INTO authors (canonical_name, canonical_email, aliases) \
             VALUES (?1, ?2, '[]') \
             ON CONFLICT(canonical_email) DO UPDATE SET canonical_name = excluded.canonical_name",
            params![canon_name, canon_email],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM authors WHERE canonical_email = ?1",
            params![canon_email],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// Expose the configured canonical email domain, if any.
    ///
    /// Why: callers (e.g. `tga aliases suggest`) need the same policy to
    /// compute confidence scores without re-parsing the config.
    /// What: returns the lowercased, leading-`@`-stripped domain.
    /// Test: covered indirectly via `resolver_tests::canonical_domain_*`.
    pub fn canonical_domain(&self) -> Option<&str> {
        self.canonical_domain.as_deref()
    }

    fn find_member_by_name(&self, name: &str) -> Option<(String, String)> {
        self.members
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .cloned()
    }
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod tests;
