//! Unit tests for the `api` feature: name validation, the reference grammar,
//! wire decoding, and value redaction.
//!
//! Test: itself.

use super::methods::{CopyRequest, DeleteRequest, ListRequest, SetRequest};
use super::*;

/// Stand-in value, asserted absent from every rendered surface.
const FAKE_VALUE: &str = "sk-fake-9f3e2d1c0b";

/// Why: the key is a keychain account and an index row; every invalid shape
/// must be refused as `InvalidKey`, never accepted or reported as a different
/// failure.
/// Test: itself.
#[test]
fn api_key_validation_table() {
    for ok in ["API_KEY", "_private", "a", "openai.key-2", "9LIVES"] {
        assert_eq!(SecretKey::new(ok).expect(ok).as_str(), ok, "case is kept");
    }
    let long = "K".repeat(MAX_KEY_LEN + 1);
    for bad in [
        "",
        "has space",
        "has/slash",
        "-leading-dash",
        ".leading-dot",
        "tab\tinside",
        "nul\0inside",
        "back\\slash",
        "émoji",
        long.as_str(),
    ] {
        let err = SecretKey::new(bad).expect_err("must be refused");
        assert!(
            matches!(err, SecretsError::InvalidKey { .. }),
            "{bad:?} gave {err:?}"
        );
    }
}

/// Why: a vault is a keychain service and an index file stem; the grammar is
/// closed so an override cannot escape it.
/// Test: itself.
#[test]
fn api_vault_validation_table() {
    assert_eq!(
        VaultName::new("trusty/BobMatNyc/Trusty-Tools")
            .unwrap()
            .as_str(),
        "trusty/bobmatnyc/trusty-tools"
    );
    assert_eq!(
        VaultName::new("trusty/acme").unwrap().as_str(),
        "trusty/acme"
    );
    for bad in [
        "",
        "trusty",
        "trusty/",
        "trusty//repo",
        "trusty/a/b/c",
        "other/a",
        "/trusty/a",
        "trusty/../etc",
        "trusty/./x",
        "trusty/a\nb",
        "trusty/a\\b",
        "trusty/a b",
        "trusty/a/",
    ] {
        let err = VaultName::new(bad).expect_err("must be refused");
        assert!(
            matches!(err, SecretsError::InvalidName { what: "vault", .. }),
            "{bad:?} gave {err:?}"
        );
    }
}

/// Why: GitHub owners and repositories are case-insensitive; two spellings
/// of one remote must not split into two vaults.
/// Test: itself.
#[test]
fn api_owner_and_repo_fold_to_lowercase() {
    let owner = OwnerName::new("BobMatNyc").unwrap();
    let repo = RepoName::new("Trusty-Tools").unwrap();
    assert_eq!(
        VaultName::project(&owner, &repo).as_str(),
        "trusty/bobmatnyc/trusty-tools"
    );
    assert_eq!(VaultName::owner(&owner).as_str(), "trusty/bobmatnyc");
    assert!(matches!(
        OwnerName::new("a/b"),
        Err(SecretsError::InvalidName { what: "owner", .. })
    ));
    assert!(matches!(
        RepoName::new(".."),
        Err(SecretsError::InvalidName {
            what: "repository",
            ..
        })
    ));
}

/// Why: DOC-74 §15.3 fixes three forms; everything else is refused with the
/// variant that names the broken part.
/// Test: itself.
#[test]
fn api_reference_grammar_table() {
    let r = SecretRef::parse("secret://API_KEY").unwrap();
    assert_eq!(r.key().as_str(), "API_KEY");
    assert_eq!(r.pinned_vault(), None);

    let r = SecretRef::parse("secret://BobMatNyc/API_KEY").unwrap();
    assert_eq!(r.pinned_vault().unwrap().as_str(), "trusty/bobmatnyc");

    let r = SecretRef::parse("secret://bobmatnyc/trusty-tools/API_KEY").unwrap();
    assert_eq!(
        r.pinned_vault().unwrap().as_str(),
        "trusty/bobmatnyc/trusty-tools"
    );

    for bad in [
        "API_KEY",
        "secret:/API_KEY",
        "SECRET://API_KEY",
        " secret://API_KEY",
        "secret://",
        "secret://a//KEY",
        "secret://API_KEY/",
        "secret://a/b/c/KEY",
        "secret://a/KEY\n",
    ] {
        let err = SecretRef::parse(bad).expect_err(bad);
        assert!(
            matches!(err, SecretsError::InvalidReference { .. }),
            "{bad:?} gave {err:?}"
        );
    }
    assert!(matches!(
        SecretRef::parse("secret://has space"),
        Err(SecretsError::InvalidKey { .. })
    ));
    assert!(matches!(
        SecretRef::parse("secret://../KEY"),
        Err(SecretsError::InvalidName { what: "owner", .. })
    ));
}

/// Why: a reference written back out (e.g. into a `.env`) must parse to the
/// same reference.
/// Test: itself.
#[test]
fn api_reference_display_round_trips() {
    for raw in [
        "secret://API_KEY",
        "secret://acme/API_KEY",
        "secret://acme/web/API_KEY",
    ] {
        let parsed: SecretRef = raw.parse().unwrap();
        assert_eq!(parsed.to_string(), raw);
    }
}

/// Why: an operator who pastes a value where a name belongs must not see the
/// value echoed back in an error.
/// Test: itself.
#[test]
fn api_errors_never_echo_rejected_input() {
    let pasted = format!("{FAKE_VALUE} ");
    let errors = [
        SecretKey::new(&pasted).unwrap_err(),
        VaultName::new(&format!("trusty/{pasted}")).unwrap_err(),
        OwnerName::new(&pasted).unwrap_err(),
        SecretRef::parse(&format!("secret://{pasted}")).unwrap_err(),
        BackendId::new(&pasted).unwrap_err(),
    ];
    for err in errors {
        let shown = format!("{err} / {err:?}");
        assert!(!shown.contains(FAKE_VALUE), "error echoed input: {shown}");
    }
}

/// Why: the wire is a trust boundary; a bad name or an unexpected field must
/// fail at decode rather than reach a handler.
/// Test: itself.
#[test]
fn api_requests_fail_closed_on_bad_names_and_unknown_fields() {
    let ok: ListRequest = serde_json::from_str(r#"{"vault":"trusty/acme/web"}"#).unwrap();
    assert_eq!(ok.vault.as_str(), "trusty/acme/web");

    assert!(serde_json::from_str::<ListRequest>(r#"{"vault":"trusty/../x"}"#).is_err());
    assert!(serde_json::from_str::<ListRequest>(r#"{"vault":"trusty/acme","extra":1}"#).is_err());
    assert!(
        serde_json::from_str::<DeleteRequest>(r#"{"vault":"trusty/acme","key":"a b"}"#).is_err()
    );
    assert!(
        serde_json::from_str::<CopyRequest>(r#"{"from_backend":"keychain","to_backend":"BAD ID"}"#)
            .is_err()
    );
    let copy: CopyRequest =
        serde_json::from_str(r#"{"from_backend":"keychain","to_backend":"onepassword"}"#).unwrap();
    assert!(copy.keys.is_empty(), "keys default to every indexed key");
}

/// Why: QA regression class from PR #2427 — `{:?}` of a value-carrying type
/// must never render the value.
/// Test: itself.
#[test]
fn api_debug_of_value_carrying_types_hides_the_value() {
    let value = SecretValue::new(FAKE_VALUE);
    let shown = format!("{value:?}");
    assert!(!shown.contains(FAKE_VALUE), "{shown}");
    assert!(shown.contains(&FAKE_VALUE.chars().count().to_string()));

    let request: SetRequest = serde_json::from_str(&format!(
        r#"{{"vault":"trusty/acme","key":"API_KEY","value":"{FAKE_VALUE}"}}"#
    ))
    .unwrap();
    assert_eq!(request.value.expose(), FAKE_VALUE, "the wire carries it");
    let shown = format!("{request:?} {request:#?}");
    assert!(!shown.contains(FAKE_VALUE), "{shown}");
}

/// Why: #9073 — callers build requests through constructors, so a
/// constructor's defaults must equal what the wire decodes when the optional
/// fields are omitted, and its output must decode on a server that denies
/// unknown fields.
/// Test: itself.
#[test]
fn api_request_constructors_match_the_wire_shape() {
    fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(request: &T) -> T {
        serde_json::from_value(serde_json::to_value(request).unwrap()).unwrap()
    }
    let vault = VaultName::new("trusty/acme/web").unwrap();
    let key = SecretKey::new("API_KEY").unwrap();
    let (keychain, other) = (
        BackendId::keychain(),
        BackendId::new("onepassword").unwrap(),
    );

    let list = ListRequest::new(vault.clone());
    assert_eq!(round_trip(&list), list);
    let delete = DeleteRequest::new(vault.clone(), key.clone());
    assert_eq!(round_trip(&delete), delete);
    let set = round_trip(&SetRequest::new(
        vault,
        key.clone(),
        SecretValue::new(FAKE_VALUE),
    ));
    assert_eq!((set.key, set.value.expose()), (key.clone(), FAKE_VALUE));

    let every: CopyRequest =
        serde_json::from_str(r#"{"from_backend":"keychain","to_backend":"onepassword"}"#).unwrap();
    let copy = CopyRequest::new(keychain, other);
    assert_eq!(
        copy, every,
        "an omitted `keys` and `new` both mean every key"
    );
    assert_eq!(round_trip(&copy), copy);
    let narrowed = copy.with_keys([key.clone()]);
    assert_eq!(narrowed.keys, vec![key]);
    assert_eq!(round_trip(&narrowed), narrowed);
}
