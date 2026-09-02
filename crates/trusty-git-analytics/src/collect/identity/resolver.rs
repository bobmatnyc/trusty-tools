//! Developer identity resolution.
//!
//! Given a raw `(name, email)` tuple observed in a git commit, resolve
//! it to a canonical identity using a three-tier strategy:
//! 1. Exact alias match against the configured aliases map.
//! 2. Fuzzy match against team member canonical emails/names using
//!    Jaro-Winkler similarity above a configurable threshold.
//! 3. Fall through and return the raw pair unchanged.
//!
//! The fuzzy tiers are a *fallback for undeclared identities*, not a
//! supplement to a declared roster: when the project supplies a comprehensive
//! alias table they are switched off (issue #4251). See
//! [`IdentityResolver::fuzzy_fallback`].

use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::HashMap;

use rusqlite::params;
use strsim::jaro_winkler;
use tracing::{debug, error, info, warn};

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

/// Extract the domain-part (after the last `@`) of an email address,
/// lowercased. Returns an empty string when no `@` is present.
///
/// Why: Tier-3 fuzzy matching (issue #2253) must gate email similarity on
/// domain equality, and it needs the two domains to compare. `email_local_part`
/// covers the complementary half; this pairs with it.
/// What: splits on the last `@` and lowercases the remainder.
/// Test: see `resolver_tests::email_domain_basic`.
fn email_domain(email: &str) -> String {
    match email.rfind('@') {
        Some(i) => email[i + 1..].to_lowercase(),
        None => String::new(),
    }
}

/// The configured canonical email domain, trimmed, `@`-stripped, lowercased.
///
/// Why: three call sites need this value in the same normalised form — the
/// resolver's own construction, `tga aliases suggest`, and the DD audit's
/// authorship pass (#6142 review). Three copies of `trim / strip / lowercase`
/// is exactly the drift the "one implementation" rule exists to prevent.
/// What: `None` when no team block, no `canonical_domain`, or an empty one.
/// Test: `resolver_tests::configured_canonical_domain_normalises_the_value`.
pub fn configured_canonical_domain(config: &crate::core::config::Config) -> Option<String> {
    config
        .team
        .as_ref()
        .and_then(|t| t.canonical_domain.as_deref())
        .and_then(normalize_domain)
}

/// [`configured_canonical_domain`]'s normalisation, for a bare string.
fn normalize_domain(raw: &str) -> Option<String> {
    let d = raw.trim().trim_start_matches('@').to_lowercase();
    if d.is_empty() {
        None
    } else {
        Some(d)
    }
}

/// The `authors.id` that already absorbed `email` as a CONFIRMED alias.
///
/// Why (#6142 review): `tga aliases merge` reassigns the source's commits,
/// appends the source email to the destination's `aliases` array, and DELETES
/// the source row. Only the alias array survives — so a later collect that
/// observes the merged-away email on a new commit re-creates the deleted row
/// and relinks to it, undoing the merge in the database. Asking this question
/// before writing keeps an accepted merge accepted.
/// What: matches the JSON array with `LIKE` to narrow the scan, then confirms
/// an exact case-insensitive element match, because `LIKE` alone matches a
/// substring of a longer address. Malformed JSON yields no match.
/// Test: `resolver_tests::a_merged_away_email_routes_to_its_canonical_row`.
///
/// # Errors
///
/// Returns [`crate::core::TgaError::DbError`] on SQL failure.
fn confirmed_alias_owner(
    conn: &rusqlite::Connection,
    email: &str,
) -> crate::core::Result<Option<i64>> {
    if email.is_empty() {
        return Ok(None);
    }
    let mut stmt = conn.prepare("SELECT id, aliases FROM authors WHERE aliases LIKE ?1")?;
    let pattern = format!("%\"{email}\"%");
    let mut rows = stmt.query(params![pattern])?;
    let needle = email.to_lowercase();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let aliases_json: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
        let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
        if aliases.iter().any(|a| a.to_lowercase() == needle) {
            return Ok(Some(id));
        }
    }
    Ok(None)
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

/// Why: every member scan — both fuzzy tiers and
/// [`IdentityResolver::find_member_by_name`] — walks `members` in slice order,
/// so that order decides the winner. Building it by pushing in `HashMap`
/// iteration order made the resolved identity differ between two resolvers
/// constructed from identical input (issue #4293).
/// What: total order over a canonical member — canonical email first, canonical
/// name second, both compared as raw bytes.
/// Test: see `resolver_tests::member_order_is_deterministic_across_rebuilds`.
fn member_key(m: &(String, String)) -> (&str, &str) {
    (m.1.as_str(), m.0.as_str())
}

/// Why: both fuzzy tiers kept the incumbent on an exact score tie, so the
/// winner was whichever member the scan reached first (issue #4293).
/// What: returns `true` when `(score, candidate)` should displace `best`,
/// deciding an exact tie on [`member_key`] — lowest key wins — instead of on
/// encounter order. Callers gate on the similarity threshold first, which
/// already rejects a `NaN` score.
/// Test: see `resolver_tests::fuzzy_tie_breaks_on_stable_key`.
fn is_better(
    score: f64,
    candidate: &(String, String),
    best: Option<(f64, &(String, String))>,
) -> bool {
    match best {
        None => true,
        Some((incumbent, m)) => match score.partial_cmp(&incumbent) {
            Some(Ordering::Greater) => true,
            Some(Ordering::Equal) => member_key(candidate) < member_key(m),
            _ => false,
        },
    }
}

/// Why: two canonical identities may declare the same alias. Last-writer-wins
/// over a `HashMap` iteration made that mapping differ between runs (#4293).
/// What: claims `alias → canonical` only while the alias is unclaimed, so the
/// FIRST writer wins under the caller's deterministic iteration order, and
/// warns when a second identity is turned away.
/// Test: see `resolver_tests::alias_collision_resolves_deterministically`.
fn register_alias(aliases: &mut HashMap<String, String>, alias: &str, canonical: &str) {
    match aliases.entry(alias.to_lowercase()) {
        Entry::Vacant(slot) => {
            slot.insert(canonical.to_string());
        }
        Entry::Occupied(slot) => {
            if slot.get() != canonical {
                warn!(
                    alias = %alias,
                    kept = %slot.get(),
                    rejected = %canonical,
                    "alias claimed by two canonical identities; keeping the first"
                );
            }
        }
    }
}

/// The first email-looking entry of an alias list, or the empty string.
fn first_email(alias_list: &[String]) -> String {
    alias_list
        .iter()
        .find(|a| a.contains('@'))
        .cloned()
        .unwrap_or_default()
}

/// Resolves observed author identities to canonical `(name, email)` pairs.
pub struct IdentityResolver {
    /// Mapping of alias (lowercased name or email) → canonical name.
    aliases: HashMap<String, String>,
    /// Canonical members: `(canonical_name, canonical_email)`.
    ///
    /// Kept sorted by [`member_key`] at all times (issue #4293) — the fuzzy
    /// tiers and [`Self::find_member_by_name`] scan this slice in order, so the
    /// order is part of the resolver's contract, not an artefact of how the
    /// caller's config happened to be laid out.
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
    /// Whether the Tier-3/4 Jaro-Winkler fuzzy fallback may run (issue #4251).
    ///
    /// `true` preserves the historical behaviour. `false` stops resolution
    /// after the exact Tier-1/2 alias lookups, so an identity that is not
    /// declared in the alias table passes through unchanged instead of being
    /// guessed onto the nearest-spelled roster member.
    fuzzy_fallback: bool,
}

impl IdentityResolver {
    /// Construct a resolver from a [`TeamConfig`].
    ///
    /// Carries the same alias contract as [`Self::from_alias_map`] (issue
    /// #4293): members are consumed in `(email, name)` order, an alias claimed
    /// by two identities goes to the first claimant, and the three alias sources
    /// are registered in a fixed precedence — member canonical emails, then
    /// member declared aliases, then the free-form `team.aliases` map. So a
    /// member always owns its own address, and a member alias still outranks a
    /// free-form one, as it did before.
    pub fn new(team: Option<&TeamConfig>) -> Self {
        let mut aliases: HashMap<String, String> = HashMap::new();
        let mut members: Vec<(String, String)> = Vec::new();
        let mut canonical_domain: Option<String> = None;
        if let Some(team) = team {
            // #4293: walk members in member_key order, not declaration order, so
            // a contested alias and a case-colliding canonical name both resolve
            // by a stated rule.
            let mut roster: Vec<&crate::core::config::TeamMember> = team.members.iter().collect();
            roster.sort_by(|a, b| {
                (a.email.as_str(), a.name.as_str()).cmp(&(b.email.as_str(), b.name.as_str()))
            });
            for m in &roster {
                members.push((m.name.clone(), m.email.clone()));
                register_alias(&mut aliases, &m.email, &m.name);
            }
            for m in &roster {
                for a in &m.aliases {
                    register_alias(&mut aliases, a, &m.name);
                }
            }
            // #4293: `team.aliases` is a HashMap, so walk it in sorted key order
            // — two keys differing only by case otherwise raced for the same
            // lowercased slot.
            let mut free_aliases: Vec<(&String, &String)> = team.aliases.iter().collect();
            free_aliases.sort_unstable();
            for (k, v) in free_aliases {
                register_alias(&mut aliases, k, v);
            }
            canonical_domain = team.canonical_domain.as_deref().and_then(normalize_domain);
        }
        Self {
            aliases,
            members,
            threshold: DEFAULT_SIMILARITY_THRESHOLD,
            canonical_domain,
            fuzzy_fallback: true,
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
    ///
    /// The caller's `HashMap` is consumed in sorted canonical-name order and an
    /// alias claimed by two identities goes to the first claimant, so the same
    /// map always produces a byte-identical resolver (issue #4293). Self-aliases
    /// — a canonical name and its canonical email — are registered ahead of the
    /// declared alias lists, so an identity always owns its own name and address.
    pub fn from_alias_map(map: &HashMap<String, Vec<String>>) -> Self {
        let mut aliases: HashMap<String, String> = HashMap::new();
        let mut members: Vec<(String, String)> = Vec::new();
        // #4293: HashMap iteration order is per-instance randomised — sort first.
        let mut canon_names: Vec<&String> = map.keys().collect();
        canon_names.sort_unstable();
        for canon_name in &canon_names {
            let canon_email = map
                .get(*canon_name)
                .map(|l| first_email(l))
                .unwrap_or_default();
            members.push(((*canon_name).clone(), canon_email.clone()));
            register_alias(&mut aliases, canon_name, canon_name);
            if !canon_email.is_empty() {
                register_alias(&mut aliases, &canon_email, canon_name);
            }
        }
        for canon_name in &canon_names {
            for a in map.get(*canon_name).into_iter().flatten() {
                register_alias(&mut aliases, a, canon_name);
            }
        }
        members.sort_by(|a, b| member_key(a).cmp(&member_key(b)));
        Self {
            aliases,
            members,
            threshold: DEFAULT_SIMILARITY_THRESHOLD,
            canonical_domain: None,
            fuzzy_fallback: true,
        }
    }

    /// Construct a resolver from a [`crate::core::config::Config`], preferring
    /// the Python-compatible `developer_aliases` map when present, falling
    /// back to `team.members`.
    ///
    /// Also decides whether the Tier-3/4 fuzzy fallback runs (issue #4251):
    /// an explicit `fuzzy_identity_fallback` in the config wins; otherwise the
    /// fallback is disabled only when a declared `aliases_file` actually
    /// resolved to a non-empty alias table. A declared-but-unloadable file
    /// leaves the fallback ON and logs at `error!`.
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
            resolver.canonical_domain = configured_canonical_domain(config);
        }
        // Gate on whether the alias table actually LOADED, not merely on whether
        // one was declared. `Config::resolved_aliases` deliberately swallows
        // alias-file load errors, so keying on `aliases_file.is_some()` alone
        // would let a typo'd or unreadable file produce the worst possible
        // state: an empty alias table AND a disabled fallback, fragmenting every
        // author with nothing but a debug line to show for it.
        let alias_table_loaded = match &config.aliases_file {
            None => false,
            Some(_) => match config.resolved_alias_map(config.config_dir()) {
                Ok(m) if !m.is_empty() => true,
                Ok(_) => {
                    error!(
                        "aliases_file is configured but resolved to an EMPTY alias table; \
                         keeping the Tier-3/4 fuzzy fallback enabled"
                    );
                    false
                }
                Err(e) => {
                    error!(
                        error = %e,
                        "aliases_file is configured but could not be loaded; identity \
                         resolution will fall back to fuzzy matching — fix the config"
                    );
                    false
                }
            },
        };
        resolver.fuzzy_fallback = config
            .fuzzy_identity_fallback
            .unwrap_or(!alias_table_loaded);
        if !resolver.fuzzy_fallback {
            // Announce at info!, not debug!: this flips resolution behaviour for
            // every existing `aliases_file` deployment on upgrade, and an
            // operator comparing two runs needs to see why the numbers moved.
            info!(
                members = resolver.members.len(),
                "Tier-3/4 fuzzy identity fallback DISABLED (alias table supplied); \
                 authors with no declared alias will be reported under their raw \
                 name. Set `fuzzy_identity_fallback: true` to restore guessing."
            );
        }
        resolver
    }

    /// Override the fuzzy-match threshold (0.0–1.0).
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }

    /// Enable or disable the Tier-3/4 Jaro-Winkler fuzzy fallback (issue #4251).
    ///
    /// Why: [`Self::from_config`] derives the flag from config, but resolvers
    /// built from a bare alias map ([`Self::from_alias_map`]) or a
    /// [`crate::core::config::TeamConfig`] have no config to read. Those
    /// callers — currently `trusty-review`'s profile selector and the benches —
    /// keep the historical fuzzy-on default and can opt out through this
    /// builder if they ever need to. No caller sets it today; it exists so the
    /// switch is reachable without routing through `Config`.
    /// What: sets the flag consulted by [`Self::resolve`] after the exact
    /// Tier-1/2 alias lookups.
    /// Test: see `resolver_tests::with_fuzzy_fallback_false_suppresses_tier34`.
    pub fn with_fuzzy_fallback(mut self, enabled: bool) -> Self {
        self.fuzzy_fallback = enabled;
        self
    }

    /// Report whether the Tier-3/4 fuzzy fallback is active on this resolver.
    pub fn fuzzy_fallback(&self) -> bool {
        self.fuzzy_fallback
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
            // #4293: insert in sorted position so a runtime-registered identity
            // does not reintroduce arrival-order dependence into the scans.
            let entry = (canonical.to_string(), canonical_email);
            let at = self
                .members
                .partition_point(|m| member_key(m) < member_key(&entry));
            self.members.insert(at, entry);
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

        // Issue #4251: Tiers 3 and 4 guess. When the project has declared a
        // comprehensive alias table, an inbound pair that reached this point is
        // by definition NOT on the roster, and every additional roster entry
        // only widens the set of near-spellings it can be captured by
        // (`Cristian Dominguez` → `Crislaine Tripoli`). Stop here and let the
        // identity pass through unresolved — a visibly-unmapped author is
        // recoverable by adding an alias; a silently-misattributed one is not.
        if !self.fuzzy_fallback {
            return (name.to_string(), email.to_string());
        }

        // 3. Fuzzy match against member names/emails (Jaro-Winkler).
        //    Name similarity uses the raw names. Email similarity compares
        //    LOCAL-PARTS only, and only when both addresses share the same
        //    domain (issue #2253). Comparing full email strings let a long
        //    shared domain suffix alone clear the 0.85 threshold — e.g.
        //    jaro_winkler("ops+snyk@duettoresearch.com",
        //    "jenkins@duettoresearch.com") = 0.857 — merging unrelated bots.
        //    Gating on domain equality + local-part comparison removes that
        //    false-positive class while keeping genuine same-domain,
        //    near-identical-local-part matches.
        let inbound_domain = email_domain(email);
        let mut best: Option<(f64, &(String, String))> = None;
        for m in &self.members {
            let s_name = jaro_winkler(&name_lc, &m.0.to_lowercase());
            let member_domain = email_domain(&m.1);
            let s_email = if !inbound_domain.is_empty() && inbound_domain == member_domain {
                jaro_winkler(&email_local_part(email), &email_local_part(&m.1))
            } else {
                0.0
            };
            let score = s_name.max(s_email);
            // #4293: total comparison — an exact tie goes to the lowest member_key.
            if score >= self.threshold && is_better(score, m, best) {
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
            // #4293: same total comparison as Tier 3.
            if score >= NORMALIZED_SIMILARITY_THRESHOLD && is_better(score, m, best_norm) {
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
    /// What: resolves the inbound pair to a canonical form, returns the owning
    /// identity when the resolved email is a CONFIRMED alias of one (#6142
    /// review), applies the canonical-email policy (issue #349) when a
    /// configured [`Self::canonical_domain`] is set, and otherwise writes the
    /// row keyed on `canonical_email`.
    ///
    /// The alias check comes first because `tga aliases merge` DELETES the
    /// source row and records the merge only in the destination's `aliases`
    /// array. Without it, the next collect to observe the merged-away email
    /// re-creates the row the operator deleted and relinks commits to it,
    /// undoing the merge in the database rather than only in the report.
    /// Test: see `resolver_tests::canonical_domain_prefers_org_email`,
    /// `resolver_tests::a_merged_away_email_routes_to_its_canonical_row` and
    /// `crate::report::authorship_tests::a_confirmed_merge_survives_a_recollect`.
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

        // #6142 review: a confirmed merge outranks every policy below it. The
        // merge deleted the source row, so re-creating it here would undo the
        // operator's decision on the next collect.
        if let Some(id) = confirmed_alias_owner(db.connection(), &canon_email)? {
            debug!(
                observed_email = %canon_email,
                author_id = id,
                "routed commit to the identity that already absorbed this email as a confirmed alias"
            );
            return Ok(id);
        }

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

    /// First member whose canonical name matches `name`, ignoring ASCII case.
    ///
    /// #4293: "first" is well-defined because `members` is kept sorted by
    /// [`member_key`], so two canonical names differing only by case always
    /// resolve to the same one.
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
