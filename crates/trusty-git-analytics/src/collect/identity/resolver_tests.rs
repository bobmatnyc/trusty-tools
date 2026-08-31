//! Tests for [`super::resolver::IdentityResolver`] and helpers.
//!
//! Why: collected here so the production file stays under the 500-SLOC cap
//! while tests can use the 1500-SLOC test-file budget.
//! What: unit tests for alias resolution, fuzzy matching, canonical-domain
//! policy, and the `from_config` constructor.
//! Test: run with `cargo test -p tga`.

use super::*;
use crate::core::config::{TeamConfig, TeamMember};
use std::collections::HashMap;

fn make_team() -> TeamConfig {
    let mut aliases = HashMap::new();
    aliases.insert("bobby".into(), "Bob Smith".into());
    TeamConfig {
        members: vec![TeamMember {
            name: "Bob Smith".into(),
            email: "bob@example.com".into(),
            aliases: vec!["bsmith@example.com".into()],
        }],
        aliases,
        canonical_domain: None,
    }
}

#[test]
fn exact_email_alias_match() {
    let r = IdentityResolver::new(Some(&make_team()));
    let (n, e) = r.resolve("Whoever", "bsmith@example.com");
    assert_eq!(n, "Bob Smith");
    assert_eq!(e, "bob@example.com");
}

#[test]
fn exact_name_alias_match() {
    let r = IdentityResolver::new(Some(&make_team()));
    let (n, e) = r.resolve("bobby", "x@y.com");
    assert_eq!(n, "Bob Smith");
    assert_eq!(e, "bob@example.com");
}

#[test]
fn fuzzy_match_canonical_name() {
    let r = IdentityResolver::new(Some(&make_team()));
    // Slightly different spelling should still match (jaro_winkler high)
    let (n, _e) = r.resolve("Bob Smyth", "unknown@elsewhere.com");
    assert_eq!(n, "Bob Smith");
}

#[test]
fn no_match_returns_input() {
    let r = IdentityResolver::new(Some(&make_team()));
    let (n, e) = r.resolve("Zelda Q", "zelda@nowhere.test");
    assert_eq!(n, "Zelda Q");
    assert_eq!(e, "zelda@nowhere.test");
}

#[test]
fn empty_team_passthrough() {
    let r = IdentityResolver::new(None);
    let (n, e) = r.resolve("Anyone", "anyone@x.com");
    assert_eq!(n, "Anyone");
    assert_eq!(e, "anyone@x.com");
}

/// All aliases — emails AND non-email login handles — must be indexed
/// in the lookup map so every variant resolves to the canonical name.
#[test]
fn all_aliases_registered() {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    map.insert(
        "Alice Smith".to_string(),
        vec![
            "alice@company.com".into(),
            "alice.smith@personal.com".into(),
            "asmith".into(), // non-email login handle
        ],
    );
    let r = IdentityResolver::from_alias_map(&map);

    // Primary email → canonical name + primary email.
    let (n, e) = r.resolve("whoever", "alice@company.com");
    assert_eq!(n, "Alice Smith");
    assert_eq!(e, "alice@company.com");

    // Secondary email → canonical name + canonical (first) email.
    let (n, e) = r.resolve("whoever", "alice.smith@personal.com");
    assert_eq!(n, "Alice Smith");
    assert_eq!(e, "alice@company.com");

    // Non-email handle as name → canonical name.
    let (n, e) = r.resolve("asmith", "noise@nowhere.test");
    assert_eq!(n, "Alice Smith");
    assert_eq!(e, "alice@company.com");
}

/// Email local-part fuzzy: a short raw name like `"Bob M"` plus an
/// email `<bob.matsuoka@co.com>` should resolve to `"Bob Matsuoka"`
/// via the normalized fuzzy pass even when raw Jaro-Winkler on the
/// short name falls below the strict 0.85 threshold.
#[test]
fn email_local_part_fuzzy_match() {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    map.insert(
        "Bob Matsuoka".to_string(),
        vec!["bob.matsuoka@duettoresearch.com".into()],
    );
    let r = IdentityResolver::from_alias_map(&map);

    // Different email + truncated name — only the email local-part
    // normalizes to "bob matsuoka" which matches the canonical name.
    let (n, e) = r.resolve("Bob M", "bob.matsuoka@otherdomain.com");
    assert_eq!(n, "Bob Matsuoka");
    assert_eq!(e, "bob.matsuoka@duettoresearch.com");
}

/// Email case must not affect lookup — `ALICE@COMPANY.COM` resolves
/// the same as `alice@company.com`.
#[test]
fn case_insensitive_email_lookup() {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    map.insert("Alice Smith".to_string(), vec!["alice@company.com".into()]);
    let r = IdentityResolver::from_alias_map(&map);

    let (n, e) = r.resolve("Whoever", "ALICE@COMPANY.COM");
    assert_eq!(n, "Alice Smith");
    assert_eq!(e, "alice@company.com");

    let (n2, e2) = r.resolve("WhoEver", "Alice@Company.Com");
    assert_eq!(n2, "Alice Smith");
    assert_eq!(e2, "alice@company.com");
}

/// Short truncated display names should still fuzzy-match the
/// canonical form when the email local-part backs them up.
#[test]
fn short_name_fuzzy() {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    map.insert(
        "Bob Matsuoka".to_string(),
        vec!["bob.matsuoka@co.com".into()],
    );
    let r = IdentityResolver::from_alias_map(&map);

    // The unknown email forces fuzzy. "bob m" alone is too short for
    // raw Jaro-Winkler to clear 0.85 against "bob matsuoka", but the
    // local-part `bobm` normalizes and the normalized threshold (0.82)
    // accepts the match.
    let (n, _e) = r.resolve("Bob M", "bobm@unknown.test");
    assert_eq!(n, "Bob Matsuoka");
}

/// A completely unknown identity returns the raw input unchanged.
#[test]
fn unknown_author_passthrough() {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    map.insert("Alice Smith".to_string(), vec!["alice@company.com".into()]);
    let r = IdentityResolver::from_alias_map(&map);

    let (n, e) = r.resolve("Zelda Q", "zelda@nowhere.test");
    assert_eq!(n, "Zelda Q");
    assert_eq!(e, "zelda@nowhere.test");
}

/// Multiple distinct emails for the same person all collapse onto a
/// single canonical name and email pair.
#[test]
fn multiple_emails_same_person() {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    map.insert(
        "Andre Ramos".to_string(),
        vec![
            "andre.ramos@duettoresearch.com".into(),
            "129991831+andreramosduetto@users.noreply.github.com".into(),
            "andre@personal.dev".into(),
        ],
    );
    let r = IdentityResolver::from_alias_map(&map);

    let (n1, e1) = r.resolve("Andre Ramos", "andre.ramos@duettoresearch.com");
    let (n2, e2) = r.resolve(
        "andreramosduetto",
        "129991831+andreramosduetto@users.noreply.github.com",
    );
    let (n3, e3) = r.resolve("A. Ramos", "andre@personal.dev");

    assert_eq!(n1, "Andre Ramos");
    assert_eq!(n2, "Andre Ramos");
    assert_eq!(n3, "Andre Ramos");
    // Canonical email is the first email-looking alias for each.
    assert_eq!(e1, "andre.ramos@duettoresearch.com");
    assert_eq!(e2, "andre.ramos@duettoresearch.com");
    assert_eq!(e3, "andre.ramos@duettoresearch.com");
}

/// Verify resolution end-to-end through `Config::load` + external
/// `aliases_file`, mirroring the shape of the deployed
/// `configs/duetto-contractors.yaml` setup. This guards against
/// regressions in YAML schema, path resolution, or resolver wiring.
///
/// The fixture is materialized into a `tempfile::TempDir` so the test is
/// hermetic: it does not depend on absolute paths outside the repo
/// (which would fail in CI). Previously this built its own `pid + nanos`
/// directory name, the same scheme that raced in the #4251 helper below —
/// `process::id()` is constant within a test binary and the `SystemTime`
/// remainder is coarser than a nanosecond, so the name is not actually
/// unique. `TempDir` also cleans up on panic, which the manual path did not.
#[test]
fn duetto_contractors_config_resolves() {
    let tmpdir = tempfile::TempDir::new().expect("create tmp");
    let tmp = tmpdir.path();

    // Aliases file with a subset of the real Duetto contractor map,
    // including the cases the assertions below exercise (case-folding,
    // fuzzy match on email-local-part, non-email handle alias).
    let aliases_yaml = r#"
developers:
  - name: "Andre Ramos"
    primary_email: "andre.ramos@duettoresearch.com"
    aliases:
      - "129991831+andreramosduetto@users.noreply.github.com"
  - name: "Akash Arora"
    primary_email: "akash.arora@duettoresearch.com"
    aliases:
      - "Akash.Arora-c@duettoresearch.com"
      - "akash-duetto"
  - name: "Janga Vinod Kumar Reddy"
    primary_email: "janga.reddy@duettoresearch.com"
    aliases:
      - "jangareddy-duetto"
      - "164324948+jangareddy-duetto@users.noreply.github.com"
"#;
    let aliases_path = tmp.join("aliases.yaml");
    std::fs::write(&aliases_path, aliases_yaml).expect("write aliases");

    let config_yaml = format!(
        "version: \"1.0\"\naliases_file: \"{}\"\n",
        aliases_path.to_string_lossy()
    );
    let config_path = tmp.join("duetto-contractors.yaml");
    std::fs::write(&config_path, config_yaml).expect("write config");

    let cfg =
        crate::core::config::Config::load(&config_path).expect("load duetto-contractors yaml");
    let r = IdentityResolver::from_config(&cfg);

    // Known mapping from the YAML (canonical email match).
    let (n, _) = r.resolve("whoever", "andre.ramos@duettoresearch.com");
    assert_eq!(n, "Andre Ramos");

    // Case-insensitive variant of an explicitly listed alias.
    let (n, _) = r.resolve("whoever", "Akash.Arora-c@duettoresearch.com");
    assert_eq!(n, "Akash Arora");

    // Non-email login handle alias.
    let (n, _) = r.resolve("jangareddy-duetto", "noise@nowhere.test");
    assert_eq!(n, "Janga Vinod Kumar Reddy");
}

#[test]
fn normalize_for_fuzzy_basic() {
    assert_eq!(normalize_for_fuzzy("Bob.Matsuoka"), "bob matsuoka");
    assert_eq!(normalize_for_fuzzy("alice_smith-c"), "alice smith c");
    assert_eq!(normalize_for_fuzzy("  Foo   Bar  "), "foo bar");
}

#[test]
fn email_local_part_basic() {
    assert_eq!(email_local_part("Bob@Example.COM"), "bob");
    assert_eq!(email_local_part("no-at-symbol"), "no-at-symbol");
}

#[test]
fn email_domain_matches_basic() {
    // Why: regression guard for the helper used by both the canonical-
    // email policy (#349) and the alias suggester (#347).
    assert!(email_domain_matches(
        "a@DUETTORESEARCH.COM",
        "duettoresearch.com"
    ));
    assert!(email_domain_matches(
        "a@duettoresearch.com",
        "@duettoresearch.com"
    ));
    assert!(!email_domain_matches("a@other.com", "duettoresearch.com"));
    assert!(!email_domain_matches("invalid-email", "duettoresearch.com"));
    assert!(!email_domain_matches("a@duettoresearch.com", ""));
}

#[test]
fn canonical_domain_prefers_org_email_for_team_member() {
    // Why: when team.members lists an org email and the incoming commit
    // uses a personal address, resolve() already returns the org email.
    // This guards the existing happy path.
    let team = TeamConfig {
        members: vec![TeamMember {
            name: "Alice Org".into(),
            email: "alice@duettoresearch.com".into(),
            aliases: vec!["alice@personal.com".into()],
        }],
        aliases: HashMap::new(),
        canonical_domain: Some("duettoresearch.com".into()),
    };
    let r = IdentityResolver::new(Some(&team));
    let (_, e) = r.resolve("Alice Org", "alice@personal.com");
    assert_eq!(e, "alice@duettoresearch.com");
    assert_eq!(r.canonical_domain(), Some("duettoresearch.com"));
}

#[test]
fn canonical_domain_routes_new_personal_email_to_existing_org_row() {
    use crate::core::db::Database;
    use rusqlite::params;

    // Why: #349 — when the first-seen commit produces a row with an
    // org-domain email, a subsequent commit by the same name under a
    // non-org email must merge into the existing org row instead of
    // creating a new personal-email identity.
    let team = TeamConfig {
        members: vec![],
        aliases: HashMap::new(),
        canonical_domain: Some("duettoresearch.com".into()),
    };
    let r = IdentityResolver::new(Some(&team));
    let db = Database::open_in_memory().expect("db");
    // Seed an existing identity at the org-domain address.
    let _ = r
        .upsert_author(&db, "Bob Matsuoka", "bob@duettoresearch.com")
        .expect("seed");

    // Now the same person commits from a personal address. With the
    // canonical-domain policy this must collapse onto the existing row,
    // not insert a second one.
    let id = r
        .upsert_author(&db, "Bob Matsuoka", "bob@personal.com")
        .expect("upsert");
    let stored_email: String = db
        .connection()
        .query_row(
            "SELECT canonical_email FROM authors WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .expect("lookup");
    assert_eq!(stored_email, "bob@duettoresearch.com");

    // Exactly one row for "Bob Matsuoka".
    let count: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM authors WHERE canonical_name = 'Bob Matsuoka'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(count, 1);
}

#[test]
fn canonical_domain_absent_falls_back_to_first_seen_email() {
    use crate::core::db::Database;

    // Why: without a configured canonical_domain we must preserve the
    // legacy first-seen behaviour so existing setups are unchanged.
    let r = IdentityResolver::new(None);
    assert_eq!(r.canonical_domain(), None);
    let db = Database::open_in_memory().expect("db");
    let _ = r
        .upsert_author(&db, "Carol", "carol@personal.com")
        .expect("seed");
    let _ = r
        .upsert_author(&db, "Carol", "carol@work.com")
        .expect("upsert");
    let count: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM authors WHERE canonical_name = 'Carol'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    // Without the policy, both emails become separate identities.
    assert_eq!(count, 2);
}

#[test]
fn email_domain_basic() {
    // Why: regression guard for the helper backing the Tier-3 domain gate
    // (#2253).
    assert_eq!(email_domain("ops+snyk@Duetto.COM"), "duetto.com");
    assert_eq!(email_domain("no-at-symbol"), "");
    // Only the last `@` delimits the domain.
    assert_eq!(email_domain("weird@name@example.org"), "example.org");
}

/// Issue #2253: two unrelated bots sharing a long common domain suffix must
/// NOT collapse into one identity via the Tier-3 fuzzy pass.
///
/// Before the fix, Tier 3 scored Jaro-Winkler on the FULL email string, so
/// `jaro_winkler("ops+snyk@duettoresearch.com", "jenkins@duettoresearch.com")`
/// = 0.857 ≥ 0.85 (the shared 18-char domain suffix alone cleared the bar),
/// misattributing every Snyk-bot commit to "Jenkins CI". Comparing
/// local-parts under a domain-equality gate keeps them distinct.
#[test]
fn tier3_does_not_merge_bots_sharing_domain_suffix() {
    let team = TeamConfig {
        members: vec![TeamMember {
            name: "Jenkins CI".into(),
            email: "jenkins@duettoresearch.com".into(),
            aliases: vec![],
        }],
        aliases: HashMap::new(),
        canonical_domain: None,
    };
    let r = IdentityResolver::new(Some(&team));

    // Sanity: the pre-fix root cause really did clear the threshold on the
    // full-string comparison, so this test is guarding a real regression.
    assert!(
        jaro_winkler("ops+snyk@duettoresearch.com", "jenkins@duettoresearch.com")
            >= DEFAULT_SIMILARITY_THRESHOLD,
        "precondition: full-string similarity should exceed the threshold"
    );

    let (name, email) = r.resolve("Snyk Bot", "ops+snyk@duettoresearch.com");
    assert_ne!(
        name, "Jenkins CI",
        "Snyk bot must not be misattributed to Jenkins CI (#2253)"
    );
    // The unrelated bot falls through unchanged.
    assert_eq!(name, "Snyk Bot");
    assert_eq!(email, "ops+snyk@duettoresearch.com");
}

/// The #2253 fix must not over-tighten: a genuine same-person match — same
/// domain, near-identical local-parts — must STILL resolve via Tier 3 even
/// when the display name is too different to carry the match on its own.
#[test]
fn tier3_still_matches_same_domain_near_identical_local_parts() {
    let team = TeamConfig {
        members: vec![TeamMember {
            name: "Alice Cooper".into(),
            email: "alice.cooper@acme.com".into(),
            aliases: vec![],
        }],
        aliases: HashMap::new(),
        canonical_domain: None,
    };
    let r = IdentityResolver::new(Some(&team));

    // Display name "acoopr" is far from "Alice Cooper"; the match rides on
    // the email local-part `alice.coopr` ~= `alice.cooper` under the shared
    // `acme.com` domain.
    let (name, email) = r.resolve("acoopr", "alice.coopr@acme.com");
    assert_eq!(name, "Alice Cooper");
    assert_eq!(email, "alice.cooper@acme.com");
}

// ---------------------------------------------------------------------------
// Issue #4251 — Tier-3/4 fuzzy fallback must not fire when a comprehensive
// `aliases_file` is supplied.
// ---------------------------------------------------------------------------

/// Roster excerpt from the production Duetto alias map (177 entries) — only the
/// members implicated in the #4251 misattributions, plus two entries whose
/// explicit aliases must keep resolving through Tiers 1/2.
const ISSUE_4251_ALIASES_YAML: &str = r#"
developers:
  - name: "Crislaine Tripoli"
    primary_email: "crislaine.tripoli@duettoresearch.com"
    aliases: []
  - name: "Ravi Pandey"
    primary_email: "ravi.pandey@duettoresearch.com"
    aliases: []
  - name: "Gaurav Sharma"
    primary_email: "gaurav.sharma@duettoresearch.com"
    aliases: []
  - name: "Joshua Lepage"
    primary_email: "joshua.lepage@duettoresearch.com"
    aliases: []
  - name: "Joshua McCartney"
    primary_email: "joshua.mccartney@duettoresearch.com"
    aliases: []
  - name: "Akash Arora"
    primary_email: "akash.arora@duettoresearch.com"
    aliases:
      - "Akash.Arora-c@duettoresearch.com"
      - "akash-duetto"
  - name: "Andre Ramos"
    primary_email: "andre.ramos@duettoresearch.com"
    aliases:
      - "129991831+andreramosduetto@users.noreply.github.com"
"#;

/// The `(author_name, author_email)` pairs observed in production `tga.db` that
/// #4251 reports as misattributed, paired with the roster member each was
/// wrongly collapsed onto.
const ISSUE_4251_MISATTRIBUTIONS: &[(&str, &str, &str)] = &[
    (
        "Cristian Dominguez",
        "cristian.dominguez@duettoresearch.com",
        "Crislaine Tripoli",
    ),
    (
        "Ravi Chandrasekaran",
        "ravi.chandrasekaran@duettoresearch.com",
        "Ravi Pandey",
    ),
    (
        "Gauri Saykar",
        "gauri.saykar@duettoresearch.com",
        "Gaurav Sharma",
    ),
    ("Josh Taylor", "josh@duettoresearch.com", "Joshua Lepage"),
    ("Joseph Ku", "joseph.ku@duettoresearch.com", "Joshua Lepage"),
];

/// Materialize a `config.yaml` + external `aliases_file` pair into a private
/// temp directory and load it through the real
/// [`crate::core::config::Config::load`] path, returning `(resolver, tmpdir)`.
/// The caller must keep `tmpdir` alive for the duration of the test.
///
/// Why: #4251 is a *config-shape* defect — it only manifests when the alias map
/// arrives via `aliases_file`. Constructing the resolver by hand would bypass
/// the very wiring under test.
///
/// Uses [`tempfile::TempDir`] rather than a hand-rolled `pid + nanos` name.
/// Tests in one binary share a process, so `process::id()` is constant, and the
/// `SystemTime` remainder is coarser than a nanosecond on macOS: hand-rolled
/// names collided across the parallel tests below, and each test's cleanup then
/// deleted a directory another test was still using. Because
/// `Config::resolved_aliases()` swallows alias-file load errors, the victim
/// silently got a resolver with ZERO members — which returns every input
/// unchanged and so satisfied the primary #4251 assertion *vacuously*.
/// `TempDir` gives a kernel-guaranteed unique name and RAII cleanup that also
/// runs on panic.
fn resolver_from_aliases_file(extra_config_yaml: &str) -> (IdentityResolver, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();

    let aliases_path = tmp.path().join("aliases.yaml");
    std::fs::write(&aliases_path, ISSUE_4251_ALIASES_YAML).unwrap();

    let config_yaml = format!(
        "version: \"1.0\"\naliases_file: \"{}\"\n{extra_config_yaml}",
        aliases_path.to_string_lossy()
    );
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let cfg = crate::core::config::Config::load(&config_path).unwrap();
    (IdentityResolver::from_config(&cfg), tmp)
}

/// Assert the roster actually loaded.
///
/// Why: a resolver built from a failed alias load has zero members and returns
/// **every** input unchanged — which is exactly what the #4251 pass-through
/// assertions check. Without this positive control those assertions can pass
/// against a resolver that never read the alias file. `fuzzy_fallback()` is not
/// a substitute: it is derived from config, not from load success.
fn assert_roster_loaded(r: &IdentityResolver) {
    assert_eq!(
        r.resolve("whoever", "crislaine.tripoli@duettoresearch.com"),
        (
            "Crislaine Tripoli".to_string(),
            "crislaine.tripoli@duettoresearch.com".to_string()
        ),
        "non-vacuity: the roster must actually be loaded, otherwise a \
         pass-through result proves nothing"
    );
    assert_eq!(
        r.resolve("akash-duetto", "noise@nowhere.test").0,
        "Akash Arora",
        "non-vacuity: declared login-handle aliases must be present"
    );
}

/// Issue #4251 (primary reproduction): with a comprehensive `aliases_file`
/// supplied, an author who has no alias entry must pass through UNCHANGED
/// rather than being fuzzy-guessed onto the nearest-spelled roster member.
///
/// Each pair below is a real production misattribution. The precondition
/// assertions prove the fuzzy tiers genuinely clear their thresholds on these
/// inputs, so the test cannot pass vacuously.
#[test]
fn aliases_file_disables_tier34_name_fuzzy() {
    let (r, _tmp) = resolver_from_aliases_file("");
    assert!(
        !r.fuzzy_fallback(),
        "supplying an aliases_file must disable the fuzzy fallback"
    );
    // MUST come first: everything below asserts pass-through, which a
    // zero-member resolver would also satisfy.
    assert_roster_loaded(&r);

    // Collect every resolution before asserting so a failure reports the whole
    // misattribution set, not just the first pair to trip.
    let mut observed: Vec<String> = Vec::new();
    let mut expected: Vec<String> = Vec::new();
    for (name, email, wrong_target) in ISSUE_4251_MISATTRIBUTIONS {
        // Precondition: at least one fuzzy tier really does score this pair
        // above its acceptance threshold, i.e. the defect is live in the
        // scoring functions and this assertion is guarding real behaviour.
        let raw_name = jaro_winkler(&name.to_lowercase(), &wrong_target.to_lowercase());
        let norm_local = jaro_winkler(
            &normalize_for_fuzzy(&email_local_part(email)),
            &normalize_for_fuzzy(wrong_target),
        );
        assert!(
            raw_name >= DEFAULT_SIMILARITY_THRESHOLD
                || norm_local >= NORMALIZED_SIMILARITY_THRESHOLD,
            "precondition: {name} vs {wrong_target} must be fuzzy-reachable \
             (tier3 name={raw_name:.4}, tier4 normalized={norm_local:.4})"
        );

        let (resolved_name, resolved_email) = r.resolve(name, email);
        observed.push(format!(
            "{name} <{email}> => {resolved_name} <{resolved_email}>"
        ));
        // Correct behaviour: an undeclared identity is returned untouched.
        expected.push(format!("{name} <{email}> => {name} <{email}>"));
    }
    assert_eq!(
        observed, expected,
        "#4251: undeclared authors must pass through, not be guessed onto the \
         nearest-spelled roster member"
    );
}

/// The #4251 gate must not cost us the alias-table resolutions that the
/// 130 → 177 entry expansion was made FOR. Every genuine correction rides on
/// Tiers 1/2 (exact alias / canonical email / login handle), which the gate
/// leaves untouched.
#[test]
fn aliases_file_gate_preserves_tier12_declared_resolutions() {
    let (r, _tmp) = resolver_from_aliases_file("");

    // Declared secondary email (the `-c` contractor variant) → canonical pair.
    let (n, e) = r.resolve("whoever", "Akash.Arora-c@duettoresearch.com");
    assert_eq!(n, "Akash Arora");
    assert_eq!(e, "akash.arora@duettoresearch.com");

    // Declared non-email login handle.
    let (n, e) = r.resolve("akash-duetto", "noise@nowhere.test");
    assert_eq!(n, "Akash Arora");
    assert_eq!(e, "akash.arora@duettoresearch.com");

    // Declared GitHub noreply alias.
    let (n, e) = r.resolve(
        "andreramosduetto",
        "129991831+andreramosduetto@users.noreply.github.com",
    );
    assert_eq!(n, "Andre Ramos");
    assert_eq!(e, "andre.ramos@duettoresearch.com");

    // Canonical email, case-folded.
    let (n, e) = r.resolve("whoever", "CRISLAINE.TRIPOLI@DUETTORESEARCH.COM");
    assert_eq!(n, "Crislaine Tripoli");
    assert_eq!(e, "crislaine.tripoli@duettoresearch.com");
}

/// `fuzzy_identity_fallback: true` is the documented escape hatch: a project
/// that wants the old guessing behaviour alongside its alias file can say so,
/// and then the reported misattributions come back.
#[test]
fn explicit_opt_in_reenables_fuzzy_with_aliases_file() {
    let (r, _tmp) = resolver_from_aliases_file("fuzzy_identity_fallback: true\n");
    assert!(
        r.fuzzy_fallback(),
        "explicit opt-in must re-enable Tier 3/4"
    );
    assert_roster_loaded(&r);

    // With fuzzy back on, `Gauri Saykar` collapses onto `Gaurav Sharma` again —
    // the exact defect #4251 reports, proving the flag is load-bearing and the
    // primary test above is not passing for some unrelated reason.
    let (n, _) = r.resolve("Gauri Saykar", "gauri.saykar@duettoresearch.com");
    assert_eq!(n, "Gaurav Sharma");
}

/// The non-vacuity control must actually reject a resolver that never loaded
/// the roster.
///
/// Why: `assert_roster_loaded` is the only thing standing between a zero-member
/// resolver and a vacuously-green #4251 proof. Measured on the pre-fix helper,
/// a zero-member resolver satisfied **every** assertion of
/// `aliases_file_disables_tier34_name_fuzzy`, including `!fuzzy_fallback()` —
/// pass-through is what an empty resolver does. A guard nobody proves is
/// load-bearing is not a guard, so this pins it.
#[test]
#[should_panic(expected = "non-vacuity")]
fn non_vacuity_control_rejects_zero_member_resolver() {
    let empty: HashMap<String, Vec<String>> = HashMap::new();
    let r = IdentityResolver::from_alias_map(&empty);
    // Sanity: this resolver passes everything through, exactly like the racing
    // zero-member resolver did.
    assert_eq!(
        r.resolve(
            "Cristian Dominguez",
            "cristian.dominguez@duettoresearch.com"
        ),
        (
            "Cristian Dominguez".to_string(),
            "cristian.dominguez@duettoresearch.com".to_string()
        )
    );
    assert_roster_loaded(&r); // must panic
}

/// A declared-but-unloadable `aliases_file` must NOT disable the fallback.
///
/// Why: `Config::resolved_aliases` swallows alias-file load errors. If the gate
/// keyed on the mere presence of the `aliases_file` key, a typo'd path would
/// produce an empty alias table AND a disabled fallback — every author
/// fragments to its raw identity and nothing in the resolution path says why.
/// Gating on a successful, non-empty load means a broken config degrades to the
/// documented pre-#4251 behaviour instead of to silent fragmentation.
#[test]
fn broken_aliases_file_keeps_fuzzy_enabled() {
    let tmp = tempfile::TempDir::new().unwrap();
    let missing = tmp.path().join("does-not-exist.yaml");
    let config_yaml = format!(
        "version: \"1.0\"\naliases_file: \"{}\"\n",
        missing.to_string_lossy()
    );
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let cfg = crate::core::config::Config::load(&config_path).unwrap();
    // Precondition: the alias map really did fail to load, and the failure
    // really is swallowed — this is the trap being guarded.
    assert!(cfg.resolved_alias_map(cfg.config_dir()).is_err());
    assert!(cfg.resolved_aliases().is_empty());

    let r = IdentityResolver::from_config(&cfg);
    assert!(
        r.fuzzy_fallback(),
        "a declared aliases_file that failed to load must not be treated as a \
         comprehensive alias table"
    );
}

/// An `aliases_file` that loads but declares no developers is equally not a
/// comprehensive table, and must not disable the fallback either.
#[test]
fn empty_aliases_file_keeps_fuzzy_enabled() {
    let tmp = tempfile::TempDir::new().unwrap();
    let aliases_path = tmp.path().join("aliases.yaml");
    std::fs::write(&aliases_path, "developers: []\n").unwrap();
    let config_yaml = format!(
        "version: \"1.0\"\naliases_file: \"{}\"\n",
        aliases_path.to_string_lossy()
    );
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let cfg = crate::core::config::Config::load(&config_path).unwrap();
    assert!(cfg.resolved_alias_map(cfg.config_dir()).unwrap().is_empty());

    let r = IdentityResolver::from_config(&cfg);
    assert!(r.fuzzy_fallback());
}

/// A non-empty INLINE `developer_aliases` map must not disable the fallback.
///
/// Why: this pins the gate to `aliases_file` specifically, which is what #4251
/// asks for. A gate written as `map.is_empty() && aliases_file.is_none()` would
/// silently flip every inline-alias deployment to fuzzy-off — a behaviour
/// change well outside the issue's scope. This test fails under that form.
#[test]
fn inline_developer_aliases_do_not_disable_fuzzy() {
    let yaml = r#"
version: "1.0"
developer_aliases:
  "Gaurav Sharma":
    - "gaurav.sharma@duettoresearch.com"
"#;
    let cfg: crate::core::config::Config = serde_yaml::from_str(yaml).unwrap();
    assert!(
        !cfg.resolved_aliases().is_empty(),
        "inline map must be loaded"
    );
    let r = IdentityResolver::from_config(&cfg);
    assert!(
        r.fuzzy_fallback(),
        "#4251 gates on aliases_file, not on any non-empty alias map"
    );
    // Historical fuzzy behaviour is intact for these deployments.
    let (n, _) = r.resolve("Gauri Saykar", "gauri.saykar@duettoresearch.com");
    assert_eq!(n, "Gaurav Sharma");
}

/// `fuzzy_identity_fallback: false` turns the fallback off even when no
/// `aliases_file` is present (inline `developer_aliases` deployments).
#[test]
fn explicit_opt_out_disables_fuzzy_without_aliases_file() {
    let yaml = r#"
version: "1.0"
fuzzy_identity_fallback: false
developer_aliases:
  "Gaurav Sharma":
    - "gaurav.sharma@duettoresearch.com"
"#;
    let cfg: crate::core::config::Config = serde_yaml::from_str(yaml).unwrap();
    let r = IdentityResolver::from_config(&cfg);
    assert!(!r.fuzzy_fallback());
    let (n, e) = r.resolve("Gauri Saykar", "gauri.saykar@duettoresearch.com");
    assert_eq!(n, "Gauri Saykar");
    assert_eq!(e, "gauri.saykar@duettoresearch.com");
}

/// No `aliases_file` and no explicit flag → historical behaviour is preserved
/// exactly. This is the compatibility guarantee for every existing deployment
/// that relies on fuzzy resolution.
#[test]
fn no_aliases_file_keeps_fuzzy_enabled_by_default() {
    let yaml = r#"
version: "1.0"
developer_aliases:
  "Bob Matsuoka":
    - "bob.matsuoka@duettoresearch.com"
"#;
    let cfg: crate::core::config::Config = serde_yaml::from_str(yaml).unwrap();
    let r = IdentityResolver::from_config(&cfg);
    assert!(r.fuzzy_fallback());
    let (n, _) = r.resolve("Bob M", "bob.matsuoka@otherdomain.com");
    assert_eq!(n, "Bob Matsuoka");
}

/// The builder switch works for callers that never touch `Config`
/// (`trusty-review`'s profile selector, `feed_azdo_users`, benches).
#[test]
fn with_fuzzy_fallback_false_suppresses_tier34() {
    let r = IdentityResolver::new(Some(&make_team()));
    // Baseline: fuzzy on, "Bob Smyth" collapses onto "Bob Smith".
    assert_eq!(
        r.resolve("Bob Smyth", "unknown@elsewhere.com").0,
        "Bob Smith"
    );

    let r = IdentityResolver::new(Some(&make_team())).with_fuzzy_fallback(false);
    let (n, e) = r.resolve("Bob Smyth", "unknown@elsewhere.com");
    assert_eq!(n, "Bob Smyth");
    assert_eq!(e, "unknown@elsewhere.com");
    // Exact alias resolution is untouched by the switch.
    assert_eq!(r.resolve("bobby", "x@y.com").0, "Bob Smith");
}

/// Issue #2253 regression guard, re-asserted under #4251: the email-domain
/// gate is orthogonal to the new switch and must still hold on a resolver with
/// fuzzy ENABLED (no `aliases_file`), which is the configuration #2253 fixed.
#[test]
fn issue_2253_domain_gate_intact_when_fuzzy_enabled() {
    let team = TeamConfig {
        members: vec![
            TeamMember {
                name: "Jenkins CI".into(),
                email: "jenkins@duettoresearch.com".into(),
                aliases: vec![],
            },
            TeamMember {
                name: "Alice Cooper".into(),
                email: "alice.cooper@acme.com".into(),
                aliases: vec![],
            },
        ],
        aliases: HashMap::new(),
        canonical_domain: None,
    };
    let r = IdentityResolver::new(Some(&team));
    assert!(r.fuzzy_fallback(), "no aliases_file → fuzzy stays on");

    // Shared domain suffix alone must NOT merge two unrelated bots (#2253).
    assert!(
        jaro_winkler("ops+snyk@duettoresearch.com", "jenkins@duettoresearch.com")
            >= DEFAULT_SIMILARITY_THRESHOLD,
        "precondition: full-string similarity clears the threshold"
    );
    let (n, e) = r.resolve("Snyk Bot", "ops+snyk@duettoresearch.com");
    assert_eq!(n, "Snyk Bot");
    assert_eq!(e, "ops+snyk@duettoresearch.com");

    // ...while a genuine same-domain, near-identical local-part still matches.
    let (n, e) = r.resolve("acoopr", "alice.coopr@acme.com");
    assert_eq!(n, "Alice Cooper");
    assert_eq!(e, "alice.cooper@acme.com");
}

#[test]
fn canonical_domain_read_from_config() {
    // Why: confirms YAML deserialization wires the new key end-to-end.
    let yaml = r#"
team:
  canonical_domain: "duettoresearch.com"
  members:
    - name: "Alice"
      email: "alice@duettoresearch.com"
"#;
    let cfg: crate::core::config::Config = serde_yaml::from_str(yaml).expect("parse");
    let r = IdentityResolver::from_config(&cfg);
    assert_eq!(r.canonical_domain(), Some("duettoresearch.com"));
}

// ---------------------------------------------------------------------------
// Issue #4293 — identity-resolution determinism.
// ---------------------------------------------------------------------------

/// Why: #4293 — `from_alias_map` pushed members in `HashMap` iteration order,
/// which std randomises per map instance, so two resolvers built from the same
/// roster in the same process held different `members` slices.
/// What: rebuilds the map and the resolver from scratch and asserts the
/// `members` slice, and one fuzzy resolution over it, are identical every time.
#[test]
fn member_order_is_deterministic_across_rebuilds() {
    fn build() -> IdentityResolver {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for (name, email) in [
            ("Joshua Lepage", "joshua.lepage@acme.com"),
            ("Joshua Mccartney", "joshua.mccartney@acme.com"),
            ("Joshua Renner", "joshua.renner@acme.com"),
            ("Joshua Vance", "joshua.vance@acme.com"),
            ("Joshua Whitlock", "joshua.whitlock@acme.com"),
            ("Joshua Ackley", "joshua.ackley@acme.com"),
        ] {
            map.insert(name.to_string(), vec![email.to_string()]);
        }
        IdentityResolver::from_alias_map(&map)
    }

    // Sorted by (canonical_email, canonical_name), not by declaration order.
    let expected_members: Vec<(String, String)> = [
        ("Joshua Ackley", "joshua.ackley@acme.com"),
        ("Joshua Lepage", "joshua.lepage@acme.com"),
        ("Joshua Mccartney", "joshua.mccartney@acme.com"),
        ("Joshua Renner", "joshua.renner@acme.com"),
        ("Joshua Vance", "joshua.vance@acme.com"),
        ("Joshua Whitlock", "joshua.whitlock@acme.com"),
    ]
    .iter()
    .map(|(n, e)| ((*n).to_string(), (*e).to_string()))
    .collect();

    let expected_resolution = build().resolve("josh", "josh@unaffiliated.test");
    for i in 0..100 {
        let r = build();
        assert_eq!(
            r.members, expected_members,
            "members order differs on rebuild {i}"
        );
        assert_eq!(
            r.resolve("josh", "josh@unaffiliated.test"),
            expected_resolution,
            "resolution differs on rebuild {i}"
        );
    }
}

/// Why: #4293 — both fuzzy tiers kept the incumbent on an exact score tie, so
/// two roster members scoring identically were separated by declaration order.
/// What: two members share a canonical name and differ only by email, which
/// makes their Jaro-Winkler scores exactly equal; the resolver must pick the
/// lower `(canonical_email, canonical_name)` key in either declaration order.
#[test]
fn fuzzy_tie_breaks_on_stable_key() {
    fn team(order: [(&str, &str); 2]) -> TeamConfig {
        TeamConfig {
            members: order
                .iter()
                .map(|(n, e)| TeamMember {
                    name: (*n).to_string(),
                    email: (*e).to_string(),
                    aliases: vec![],
                })
                .collect(),
            aliases: HashMap::new(),
            canonical_domain: None,
        }
    }

    let a = ("Sam Taylor", "a.taylor@acme.com");
    let b = ("Sam Taylor", "b.taylor@acme.com");
    let inbound_name = "Sam Taylorr";
    let inbound_email = "sam@unaffiliated.test";

    // Precondition: the two members score an EXACT tie for this inbound pair.
    // An exact tie is the trigger condition for the defect.
    let score_a = jaro_winkler(&inbound_name.to_lowercase(), &a.0.to_lowercase());
    let score_b = jaro_winkler(&inbound_name.to_lowercase(), &b.0.to_lowercase());
    assert_eq!(score_a, score_b, "precondition: scores must tie exactly");
    assert!(
        score_a >= DEFAULT_SIMILARITY_THRESHOLD,
        "precondition: the tied score must clear the Tier-3 threshold"
    );

    for order in [[a, b], [b, a]] {
        let r = IdentityResolver::new(Some(&team(order)));
        let (n, e) = r.resolve(inbound_name, inbound_email);
        assert_eq!(n, "Sam Taylor");
        assert_eq!(
            e, "a.taylor@acme.com",
            "tie must go to the lowest (email, name) key; declared order was {order:?}"
        );
    }
}

/// Why: #4293 — an alias claimed by two canonical identities was resolved
/// last-writer-wins over `HashMap` iteration order, so it mapped to a different
/// person between two resolvers built from identical input.
/// What: rebuilds a map in which both identities declare `jr` and asserts the
/// same canonical identity every time — the first claimant in sorted
/// canonical-name order keeps the alias.
#[test]
fn alias_collision_resolves_deterministically() {
    fn build() -> IdentityResolver {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        map.insert(
            "Jane Roe".to_string(),
            vec!["jane.roe@acme.com".into(), "jr".into()],
        );
        map.insert(
            "John Roe".to_string(),
            vec!["john.roe@acme.com".into(), "jr".into()],
        );
        IdentityResolver::from_alias_map(&map)
    }

    for i in 0..200 {
        let (n, e) = build().resolve("jr", "jr@unaffiliated.test");
        assert_eq!(n, "Jane Roe", "alias collision flipped on rebuild {i}");
        assert_eq!(
            e, "jane.roe@acme.com",
            "alias collision flipped on rebuild {i}"
        );
    }
}
