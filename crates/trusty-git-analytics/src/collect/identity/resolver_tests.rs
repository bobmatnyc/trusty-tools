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
/// The fixture is materialized into a temp dir so the test is
/// hermetic: it does not depend on absolute paths outside the repo
/// (which would fail in CI).
#[test]
fn duetto_contractors_config_resolves() {
    let unique = format!(
        "tga-duetto-contractors-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&tmp).expect("create tmp");

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

    let _ = std::fs::remove_dir_all(&tmp);
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
