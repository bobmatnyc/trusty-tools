//! Unit tests for [`super::resolve`] and [`super::parse_dotenv`]: lookup
//! order, the "agents may use" gate, the fail-closed env map, and the `.env`
//! subset. Every test runs against [`MemoryBackend`] with a temp-dir index.
//!
//! Test: itself.

use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::api::{SecretKey, SecretRef, SecretValue, SecretsError, VaultName};

/// A value no output type may ever render.
const SENTINEL: &str = "sk-sentinel-7525-do-not-print";

fn project() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn owner() -> VaultName {
    VaultName::new("trusty/acme").unwrap()
}

fn other_project() -> VaultName {
    VaultName::new("trusty/acme/api").unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

fn reference(raw: &str) -> SecretRef {
    SecretRef::parse(raw).unwrap()
}

fn scopes() -> ScopeSet {
    ScopeSet::new(project(), Some(owner()))
}

fn fixture() -> (TempDir, Arc<MemoryBackend>, SecretStore) {
    let tmp = TempDir::new().unwrap();
    let backend = Arc::new(MemoryBackend::new());
    let store = SecretStore::new(
        Arc::clone(&backend) as Arc<dyn SecretBackend>,
        NamesIndex::at(tmp.path().join("index")),
    );
    (tmp, backend, store)
}

fn set(store: &SecretStore, vault: &VaultName, name: &str, value: &str) {
    store
        .set(vault, &key(name), &SecretValue::new(value))
        .unwrap();
}

fn resolve(store: &SecretStore, raw: &str, agent_parent: bool) -> Result<String, SecretsError> {
    resolve_reference(store, &scopes(), &reference(raw), agent_parent)
        .map(|value| value.expose().to_string())
}

/// Why: DOC-74 §15.3 — an unscoped reference tries the project vault, then
/// the owner vault.
/// Test: itself.
#[test]
fn resolve_unscoped_prefers_project_over_owner() {
    let (_tmp, _backend, store) = fixture();
    set(&store, &owner(), "SHARED", "owner-value");
    set(&store, &project(), "SHARED", "project-value");
    set(&store, &owner(), "ONLY_OWNER", "owner-only");

    assert_eq!(
        resolve(&store, "secret://SHARED", false).unwrap(),
        "project-value"
    );
    assert_eq!(
        resolve(&store, "secret://ONLY_OWNER", false).unwrap(),
        "owner-only"
    );
}

/// Why: an explicit `<owner>/KEY` or `<owner>/<repo>/KEY` names exactly one
/// of the caller's vaults; it must not fall back to the other one.
/// Test: itself.
#[test]
fn resolve_explicit_reference_reads_only_its_vault() {
    let (_tmp, backend, store) = fixture();
    set(&store, &project(), "SHARED", "project-value");
    set(&store, &owner(), "SHARED", "owner-value");
    set(&store, &owner(), "OWNER_ONLY", "owner-only");

    assert_eq!(
        resolve(&store, "secret://acme/SHARED", false).unwrap(),
        "owner-value"
    );
    assert_eq!(
        resolve(&store, "secret://acme/web/SHARED", false).unwrap(),
        "project-value"
    );
    let reads = backend.reads();
    match resolve(&store, "secret://acme/web/OWNER_ONLY", false) {
        Err(SecretsError::NotFound { key, searched }) => {
            assert_eq!(key, "OWNER_ONLY");
            assert_eq!(searched, "trusty/acme/web", "only the pinned vault");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert_eq!(backend.reads(), reads, "a miss never reads the backend");
}

/// Why: #9328 vector (a), owner ruling 06 R1 — a pinned
/// `secret://<owner>[/<repo>]/KEY` was honoured for any vault, so a `.env`
/// edited in a PR to `DB=secret://victim/prod-repo/DB_URL` injected another
/// project's value. A pinned vault outside the caller's lookup order is now
/// refused before the index or backend is read, with or without an agent
/// parent, through `locate`, `resolve_reference` and `resolve_env`, and the
/// error carries no value. A sibling repo of the same owner is out of scope
/// too. Red on the unfixed code: every reference below resolves to its
/// value.
/// Test: itself.
#[test]
fn resolve_pinned_reference_outside_the_scopes_is_refused() {
    let (_tmp, backend, store) = fixture();
    let victim_project = VaultName::new("trusty/victim/prod-repo").unwrap();
    let victim_owner = VaultName::new("trusty/victim").unwrap();
    set(&store, &victim_project, "DB_URL", SENTINEL);
    set(&store, &victim_owner, "DB_URL", SENTINEL);
    set(&store, &other_project(), "DB_URL", SENTINEL);
    let reads = backend.reads();

    for (raw, vault) in [
        (
            "secret://victim/prod-repo/DB_URL",
            "trusty/victim/prod-repo",
        ),
        ("secret://victim/DB_URL", "trusty/victim"),
        ("secret://acme/api/DB_URL", "trusty/acme/api"),
    ] {
        for agent_parent in [false, true] {
            let err = resolve(&store, raw, agent_parent).unwrap_err();
            match &err {
                SecretsError::VaultOutOfScope { vault: named, .. } => assert_eq!(named, vault),
                other => panic!("{raw}: expected VaultOutOfScope, got {other:?}"),
            }
            assert!(!format!("{err} {err:?}").contains(SENTINEL), "{raw}");
        }
        assert!(matches!(
            store.locate(&reference(raw), &scopes()),
            Err(SecretsError::VaultOutOfScope { .. })
        ));

        let entries = [EnvEntry::new("DB", raw)];
        let err = resolve_env(&store, &scopes(), &entries, false).unwrap_err();
        match &err {
            SecretsError::EnvResolution { source, .. } => assert!(
                matches!(**source, SecretsError::VaultOutOfScope { .. }),
                "{raw}: {source:?}"
            ),
            other => panic!("{raw}: expected EnvResolution, got {other:?}"),
        }
        assert!(!format!("{err} {err:?}").contains(SENTINEL), "{raw}");
    }
    assert_eq!(backend.reads(), reads, "a refusal never reads the backend");
}

/// Why: a miss is `SecretsError::NotFound`, never an empty value.
/// Test: itself.
#[test]
fn resolve_miss_is_not_found() {
    let (_tmp, _backend, store) = fixture();
    match resolve(&store, "secret://NOPE", false) {
        Err(SecretsError::NotFound { key, searched }) => {
            assert_eq!(key, "NOPE");
            assert_eq!(searched, "trusty/acme/web, trusty/acme");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// Why: DOC-74 §15.8 — under a Claude Code parent, a key whose flag is OFF
/// is refused, and the Keychain is never touched for it. This is the
/// fails-on-old proof for the gate.
/// Test: itself.
#[test]
fn resolve_agent_gate_refuses_flag_off_before_any_read() {
    let (_tmp, backend, store) = fixture();
    set(&store, &project(), "DEPLOY_TOKEN", SENTINEL);

    match resolve(&store, "secret://DEPLOY_TOKEN", true) {
        Err(SecretsError::AgentUseRefused { key, vault }) => {
            assert_eq!(key, "DEPLOY_TOKEN");
            assert_eq!(vault, "trusty/acme/web");
        }
        other => panic!("expected AgentUseRefused, got {other:?}"),
    }
    assert_eq!(backend.reads(), 0, "a refused key is never read");

    assert_eq!(
        resolve(&store, "secret://DEPLOY_TOKEN", false).unwrap(),
        SENTINEL
    );
    assert_eq!(
        backend.reads(),
        1,
        "without an agent parent the key is read"
    );
}

/// Why: the flag defaults OFF, and a key with no index row has no flag to
/// check — it must fail without the backend being read, even when the
/// backend holds an entry the index never listed.
/// Test: itself.
#[test]
fn resolve_agent_gate_refuses_a_key_with_no_row_without_reading() {
    let (_tmp, backend, store) = fixture();
    backend
        .set(&project(), &key("ORPHAN"), &SecretValue::new(SENTINEL))
        .unwrap();

    for raw in ["secret://ORPHAN", "secret://acme/web/ORPHAN"] {
        let err = resolve(&store, raw, true).unwrap_err();
        assert!(
            matches!(err, SecretsError::NotFound { .. }),
            "{raw}: {err:?}"
        );
    }
    assert_eq!(backend.reads(), 0, "an unindexed key is never read");

    // A freshly set key has the flag OFF.
    set(&store, &owner(), "NEW_KEY", "v");
    assert!(matches!(
        resolve(&store, "secret://NEW_KEY", true),
        Err(SecretsError::AgentUseRefused { .. })
    ));
    assert_eq!(backend.reads(), 0);
}

/// Why: a key flagged "agents may use" resolves under an agent parent, and
/// the gate judges the vault the lookup actually chose.
/// Test: itself.
#[test]
fn resolve_agent_gate_allows_a_flagged_key() {
    let (_tmp, backend, store) = fixture();
    set(&store, &owner(), "SHARED", "owner-value");
    set(&store, &project(), "SHARED", "project-value");
    store
        .set_agents_may_use(&owner(), &key("SHARED"), true)
        .unwrap();

    // The project row wins the lookup and its flag is OFF.
    assert!(matches!(
        resolve(&store, "secret://SHARED", true),
        Err(SecretsError::AgentUseRefused { vault, .. }) if vault == "trusty/acme/web"
    ));
    assert_eq!(backend.reads(), 0);
    assert_eq!(
        resolve(&store, "secret://acme/SHARED", true).unwrap(),
        "owner-value"
    );

    store
        .set_agents_may_use(&project(), &key("SHARED"), true)
        .unwrap();
    assert_eq!(
        resolve(&store, "secret://SHARED", true).unwrap(),
        "project-value"
    );
}

/// Rewrite `vault`'s index file with std::fs so `name`'s row claims
/// `"agents_may_use": true`, as any same-uid process can (#9070).
fn hand_flag_on(store: &SecretStore, vault: &VaultName, name: &str) {
    let path = store.index().path_for(vault);
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["keys"][name]["agents_may_use"] = serde_json::Value::Bool(true);
    std::fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
}

/// Why: #9070 — the index JSON is a 0600 file any same-uid process can
/// edit, so a hand-set `"agents_may_use": true` must not open the agent
/// gate. Red before the flag moved to the backend: the edit resolved.
/// Test: itself.
#[test]
fn resolve_agent_gate_ignores_a_hand_edited_index_flag() {
    let (_tmp, backend, store) = fixture();
    set(&store, &project(), "DEPLOY_TOKEN", SENTINEL);
    hand_flag_on(&store, &project(), "DEPLOY_TOKEN");

    match resolve(&store, "secret://DEPLOY_TOKEN", true) {
        Err(SecretsError::AgentUseRefused { key, vault }) => {
            assert_eq!(key, "DEPLOY_TOKEN");
            assert_eq!(vault, "trusty/acme/web");
        }
        other => panic!("expected AgentUseRefused, got {other:?}"),
    }
    assert_eq!(backend.reads(), 0, "a refused key is never read");
}

/// Why: #9070 — list and get report the flag the backend holds, so the same
/// hand edit cannot make them show a key as agent-usable. Red before the
/// fix: both reported the edited `true`.
/// Test: itself.
#[test]
fn store_list_and_get_ignore_a_hand_edited_index_flag() {
    let (_tmp, _backend, store) = fixture();
    set(&store, &project(), "DEPLOY_TOKEN", SENTINEL);
    hand_flag_on(&store, &project(), "DEPLOY_TOKEN");

    let listed = store.list(&project()).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].agents_may_use, "list reports the flag OFF");
    let row = store
        .index()
        .get(&project(), &key("DEPLOY_TOKEN"))
        .unwrap()
        .unwrap();
    assert!(!row.agents_may_use, "get reports the flag OFF");
}

/// A [`MemoryBackend`] whose flag lookups fail, standing in for a Keychain
/// that cannot answer for the flag item (#9070).
#[derive(Debug, Default)]
struct FlagFaultBackend(MemoryBackend);

impl SecretBackend for FlagFaultBackend {
    fn id(&self) -> crate::api::BackendId {
        self.0.id()
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        self.0.get(vault, key)
    }
    fn set(&self, vault: &VaultName, key: &SecretKey, v: &SecretValue) -> Result<(), SecretsError> {
        self.0.set(vault, key, v)
    }
    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        self.0.delete(vault, key)
    }
    fn agents_may_use(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        Err(SecretsError::Backend {
            backend: "memory".to_string(),
            vault: vault.to_string(),
            key: key.to_string(),
            reason: "flag lookup failed".to_string(),
        })
    }
    fn set_agents_may_use(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        allowed: bool,
    ) -> Result<(), SecretsError> {
        self.0.set_agents_may_use(vault, key, allowed)
    }
}

/// Why: #9070 Fail-Open Check — a flag lookup that fails must never read as
/// ON. With the flag item present but unreadable, the agent gate returns
/// the backend error before the value is read, and list fails rather than
/// reporting the key.
/// Test: itself.
#[test]
fn resolve_agent_gate_fails_closed_when_the_flag_read_fails() {
    let tmp = TempDir::new().unwrap();
    let backend = Arc::new(FlagFaultBackend::default());
    let store = SecretStore::new(
        Arc::clone(&backend) as Arc<dyn SecretBackend>,
        NamesIndex::at(tmp.path().join("index")),
    );
    set(&store, &project(), "DEPLOY_TOKEN", SENTINEL);
    store
        .set_agents_may_use(&project(), &key("DEPLOY_TOKEN"), true)
        .unwrap();

    let err = resolve(&store, "secret://DEPLOY_TOKEN", true).unwrap_err();
    assert!(matches!(err, SecretsError::Backend { .. }), "{err:?}");
    assert!(!format!("{err} {err:?}").contains(SENTINEL));
    assert_eq!(backend.0.reads(), 0, "the value is never read");
    assert!(matches!(
        store.list(&project()),
        Err(SecretsError::Backend { .. })
    ));
    // Without an agent parent the flag is not consulted.
    assert_eq!(
        resolve(&store, "secret://DEPLOY_TOKEN", false).unwrap(),
        SENTINEL
    );
}

/// Why: tier 1 and 2 — entries that are not references reach the child
/// unchanged, in order; references resolve.
/// Test: itself.
#[test]
fn resolve_env_passes_plain_values_through() {
    let (_tmp, _backend, store) = fixture();
    set(&store, &project(), "API_KEY", SENTINEL);
    let entries = [
        EnvEntry::new("PLAIN", "hello world"),
        EnvEntry::new("API_KEY", "secret://API_KEY"),
        EnvEntry::new("URL", "https://x.test/?q=secret://API_KEY"),
        EnvEntry::new("EMPTY", ""),
    ];
    let resolved = resolve_env(&store, &scopes(), &entries, false).unwrap();
    let shown: Vec<(&str, &str, bool)> = resolved
        .iter()
        .map(|var| {
            let is_ref = matches!(var.source, VarSource::Reference(_));
            (var.name.as_str(), var.value.expose(), is_ref)
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("PLAIN", "hello world", false),
            ("API_KEY", SENTINEL, true),
            ("URL", "https://x.test/?q=secret://API_KEY", false),
            ("EMPTY", "", false),
        ]
    );
}

/// Why: one unresolvable, malformed, or refused reference fails the whole
/// map — the child never starts with a partial env — and the error names
/// the env name and reference, never a value.
/// Test: itself.
#[test]
fn resolve_env_fails_closed_on_one_bad_reference() {
    let (_tmp, backend, store) = fixture();
    set(&store, &project(), "GOOD", SENTINEL);
    set(&store, &project(), "LOCKED", SENTINEL);
    store
        .set_agents_may_use(&project(), &key("GOOD"), true)
        .unwrap();

    let cases = [
        ("secret://MISSING", "secret://MISSING", false),
        ("secret://LOCKED", "secret://LOCKED", true),
        (
            "secret://a//b",
            "an unparsable `secret://` reference",
            false,
        ),
    ];
    for (raw, shown_ref, agent_parent) in cases {
        let entries = [
            EnvEntry::new("FIRST", "secret://GOOD"),
            EnvEntry::new("BAD", raw),
            EnvEntry::new("LAST", "plain"),
        ];
        let err = resolve_env(&store, &scopes(), &entries, agent_parent).unwrap_err();
        match &err {
            SecretsError::EnvResolution {
                name, reference, ..
            } => {
                assert_eq!(name, "BAD");
                assert_eq!(reference, shown_ref);
            }
            other => panic!("{raw}: expected EnvResolution, got {other:?}"),
        }
        assert!(!format!("{err} {err:?}").contains(SENTINEL));
    }
    // The malformed case failed in validation, before any read.
    let reads = backend.reads();
    let entries = [
        EnvEntry::new("FIRST", "secret://GOOD"),
        EnvEntry::new("BAD", "secret://"),
    ];
    assert!(resolve_env(&store, &scopes(), &entries, false).is_err());
    assert_eq!(backend.reads(), reads, "validation precedes every read");
}

/// Why: a near-miss reference (`SECRET://`, a leading space) must fail to
/// parse, not pass through as a literal `secret://…` string.
/// Test: itself.
#[test]
fn resolve_env_fails_closed_on_near_miss_references() {
    let (_tmp, _backend, store) = fixture();
    set(&store, &project(), "K", "v");
    for raw in ["SECRET://K", " secret://K", "Secret://K"] {
        let entries = [EnvEntry::new("X", raw)];
        assert!(EnvEntry::new("X", raw).is_reference(), "{raw}");
        let err = resolve_env(&store, &scopes(), &entries, false).unwrap_err();
        assert!(
            matches!(err, SecretsError::EnvResolution { .. }),
            "{raw}: {err:?}"
        );
    }
}

/// Why: a bad env name would split or truncate the child's env entry.
/// Test: itself.
#[test]
fn resolve_env_rejects_invalid_names() {
    let (_tmp, _backend, store) = fixture();
    for name in ["", "1ABC", "A=B", "A B", "A\0B", "Ä", SENTINEL] {
        let entries = [EnvEntry::new("OK", "x"), EnvEntry::new(name, "x")];
        match resolve_env(&store, &scopes(), &entries, false) {
            Err(err @ SecretsError::InvalidEnvEntry { position: 2, .. }) => {
                assert!(!err.to_string().contains(SENTINEL));
            }
            other => panic!("{name:?}: expected InvalidEnvEntry, got {other:?}"),
        }
    }
    let entries = [EnvEntry::new("NUL_VALUE", "a\0b")];
    assert!(matches!(
        resolve_env(&store, &scopes(), &entries, false),
        Err(SecretsError::InvalidEnvEntry { position: 1, .. })
    ));
}

fn parsed(text: &str) -> Vec<(String, String)> {
    parse_dotenv(text)
        .unwrap()
        .iter()
        .map(|e| (e.name().to_string(), e.raw().to_string()))
        .collect()
}

/// Why: S7 — the documented `.env` subset parses to entries in file order.
/// Test: itself.
#[test]
fn dotenv_accepts_the_documented_subset() {
    let text = "# header\n\
                \n\
                PLAIN=value\n\
                export EXPORTED=secret://API_KEY\n\
                \tINDENTED=x # trailing comment\n\
                EMPTY=\n\
                SINGLE='a $b \\c # d'\n\
                DOUBLE=\"two words\" # note\n\
                SPACED=  hello world  \r\n\
                HASH=a#b\n";
    let want = [
        ("PLAIN", "value"),
        ("EXPORTED", "secret://API_KEY"),
        ("INDENTED", "x"),
        ("EMPTY", ""),
        ("SINGLE", "a $b \\c # d"),
        ("DOUBLE", "two words"),
        ("SPACED", "hello world"),
        ("HASH", "a#b"),
    ];
    let got = parsed(text);
    let got: Vec<(&str, &str)> = got.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    assert_eq!(got, want);
}

/// Why: unsupported syntax is an error naming the line, never a guess.
/// Test: itself.
#[test]
fn dotenv_rejects_unsupported_syntax_by_line() {
    let cases = [
        "NO_EQUALS",
        "KEY = value",
        "1KEY=value",
        "KEY='unclosed",
        "KEY=\"line one\nline two\"",
        "KEY=\"esc\\n\"",
        "KEY=\"${HOME}/x\"",
        "KEY=${HOME}",
        "KEY=$HOME",
        "KEY=a\"b",
        "KEY=a\\b",
        "KEY=#value",
        "KEY='a'b",
        "KEY=a\u{7}b",
        "KEY=a\nKEY=b",
    ];
    for case in cases {
        let text = format!("# comment\nOK=1\n{case}\n");
        match parse_dotenv(&text) {
            Err(SecretsError::DotenvSyntax { line, .. }) => {
                assert!((3..=4).contains(&line), "{case:?}: line {line}");
            }
            other => panic!("{case:?}: expected DotenvSyntax, got {other:?}"),
        }
    }
}

/// Why: a malformed line usually holds a value; the error names the line
/// number only.
/// Test: itself.
#[test]
fn dotenv_errors_never_echo_the_line() {
    let lines = [
        format!("TOKEN=\"{SENTINEL}"),
        format!("TOKEN={SENTINEL}$"),
        format!("{SENTINEL}=x"),
        format!("TOKEN {SENTINEL}"),
    ];
    for line in lines {
        let err = parse_dotenv(&line).unwrap_err();
        let shown = format!("{err} / {err:?}");
        assert!(shown.contains("line 1"), "{shown}");
        assert!(!shown.contains(SENTINEL), "error echoed the line: {shown}");
    }
}

/// Why: no value may appear in the `Display` or `Debug` of any error or
/// result type this module produces.
/// Test: itself.
#[test]
fn resolve_sentinel_never_appears_in_output_types() {
    let (_tmp, _backend, store) = fixture();
    set(&store, &project(), "TOKEN", SENTINEL);
    let entries = parse_dotenv(&format!("LIT={SENTINEL}\nREF=secret://TOKEN\n")).unwrap();
    let resolved = resolve_env(&store, &scopes(), &entries, false).unwrap();
    let value = resolve_reference(&store, &scopes(), &reference("secret://TOKEN"), false).unwrap();

    let errors = [
        resolve_reference(&store, &scopes(), &reference("secret://TOKEN"), true).unwrap_err(),
        resolve_env(&store, &scopes(), &entries, true).unwrap_err(),
        resolve_env(
            &store,
            &scopes(),
            &[EnvEntry::new("BAD", format!("secret://{SENTINEL}/x/y/z"))],
            false,
        )
        .unwrap_err(),
        resolve_env(&store, &scopes(), &[EnvEntry::new(SENTINEL, "x")], false).unwrap_err(),
        parse_dotenv(&format!("A='{SENTINEL}")).unwrap_err(),
    ];
    let mut shown = format!("{entries:?} {resolved:?} {value:?}");
    for err in &errors {
        shown.push_str(&format!(" {err} {err:?}"));
    }
    assert!(!shown.contains(SENTINEL), "a value leaked: {shown}");
    assert!(shown.contains("AgentUseRefused") && shown.contains("EnvResolution"));
}
