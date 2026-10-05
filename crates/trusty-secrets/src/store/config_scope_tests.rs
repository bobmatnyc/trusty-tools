//! Unit tests for config resolution and scope derivation.
//!
//! Test: itself.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use super::config::{
    MachineSecretsConfig, ProjectSecretsConfig, load_machine_at, load_project_at, resolve,
};
use super::*;
use crate::api::methods::ScopeKind;
use crate::api::{BackendId, OwnerName, RepoName, SecretsError, VaultName};

fn backend(id: &str) -> BackendId {
    BackendId::new(id).unwrap()
}

/// Why: DOC-74 §6.1 — project backend, else machine default, else
/// `keychain`; only the project names a vault override.
/// Test: itself.
#[test]
fn config_backend_precedence_table() {
    let machine = MachineSecretsConfig {
        default_backend: Some(backend("onepassword")),
    };
    let project = ProjectSecretsConfig {
        backend: Some(backend("keeper")),
        vault: Some(VaultName::new("trusty/acme/shared").unwrap()),
    };
    let bare_project = ProjectSecretsConfig::default();

    assert_eq!(resolve(None, None).backend.as_str(), "keychain");
    assert_eq!(
        resolve(None, Some(&machine)).backend.as_str(),
        "onepassword"
    );
    assert_eq!(
        resolve(Some(&bare_project), Some(&machine))
            .backend
            .as_str(),
        "onepassword"
    );
    let both = resolve(Some(&project), Some(&machine));
    assert_eq!(both.backend.as_str(), "keeper");
    assert_eq!(both.vault_override.unwrap().as_str(), "trusty/acme/shared");
    assert!(resolve(None, Some(&machine)).vault_override.is_none());

    assert_eq!(backend("KeyChain").as_str(), "keychain");
    assert!(BackendId::new("").is_err());
    assert!(BackendId::new("one password").is_err());
}

fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

/// Why: a config that will not parse must not read as "no config" — that
/// would send writes to a different backend or vault than the operator chose.
/// The error reports position only, never the offending text. Syntax and
/// shape errors break both loaders; a bad value breaks the loader whose
/// section owns that key (each section ignores the other's keys, §6.2).
/// Test: itself.
#[test]
fn config_corrupt_file_fails_closed() {
    let tmp = TempDir::new().unwrap();
    // (label, body, breaks project loader, breaks machine loader)
    let cases = [
        ("syntax", "secrets:\n  backend: [unclosed\n", true, true),
        ("top level", "- sk-leak\n", true, true),
        (
            "bad backend",
            "secrets:\n  backend: \"sk-leak value\"\n",
            true,
            false,
        ),
        (
            "bad vault",
            "secrets:\n  vault: \"trusty/../sk-leak\"\n",
            true,
            false,
        ),
        (
            "bad default",
            "secrets:\n  default_backend:\n    - sk-leak\n",
            false,
            true,
        ),
    ];
    for (label, body, breaks_project, breaks_machine) in cases {
        let path = write(tmp.path(), "config.yaml", body);
        let mut errors = Vec::new();
        if breaks_project {
            errors.push(load_project_at(&path).unwrap_err());
        }
        if breaks_machine {
            errors.push(load_machine_at(&path).unwrap_err());
        }
        for err in errors {
            assert!(
                matches!(err, SecretsError::Config { .. }),
                "{label}: {err:?}"
            );
            assert!(!err.to_string().contains("sk-leak"), "{label}: {err}");
        }
    }
}

/// Why: no file, an empty file, or a file with no `secrets:` key is the
/// documented "no configuration" answer; other top-level keys are ignored.
/// Test: itself.
#[test]
fn config_absent_section_is_none() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(
        load_project_at(&tmp.path().join("absent.yaml")).unwrap(),
        None
    );
    let empty = write(tmp.path(), "empty.yaml", "  \n");
    assert_eq!(load_machine_at(&empty).unwrap(), None);
    let other = write(tmp.path(), "other.yaml", "default_model: opus\nsecrets:\n");
    assert_eq!(load_machine_at(&other).unwrap(), None);

    let full = write(
        tmp.path(),
        "full.yaml",
        "default_model: opus\nsecrets:\n  backend: keychain\n  vault: trusty/Acme/Shared\n  onepassword:\n    account: x\n",
    );
    let project = load_project_at(&full).unwrap().unwrap();
    assert_eq!(project.backend.unwrap().as_str(), "keychain");
    assert_eq!(project.vault.unwrap().as_str(), "trusty/acme/shared");
}

/// Why: the owner and repository come from the remote URL; every common form
/// must parse, and a failure must never echo the URL, which can carry a token.
/// Test: itself.
#[test]
fn scope_remote_url_table() {
    for url in [
        "git@github.com:BobMatNyc/trusty-tools.git",
        "https://github.com/bobmatnyc/trusty-tools",
        "https://x-access-token:ghs_secret@github.com/bobmatnyc/trusty-tools.git",
        "ssh://git@github.com:22/bobmatnyc/trusty-tools/",
    ] {
        let (owner, repo) = parse_remote_identity(url).expect(url);
        assert_eq!(
            (owner.as_str(), repo.as_str()),
            ("bobmatnyc", "trusty-tools")
        );
    }
    for url in [
        "",
        "/srv/git/repo.git",
        "https://github.com/onlyowner",
        "https://gitlab.com/group/sub/repo.git",
        "https://ghs_secret@github.com/own er/repo",
    ] {
        let reason = parse_remote_identity(url).expect_err(url);
        assert!(!reason.contains("ghs_secret"), "{reason}");
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Why: the project and owner vaults come from the checkout's `origin`.
/// Test: itself.
#[test]
fn scope_derive_reads_the_origin_remote() {
    let tmp = TempDir::new().unwrap();
    git(tmp.path(), &["init", "-q"]);
    git(
        tmp.path(),
        &["remote", "add", "origin", "git@github.com:Acme/Web.git"],
    );
    let scopes = ScopeSet::derive(tmp.path(), None).unwrap();
    assert_eq!(scopes.project().as_str(), "trusty/acme/web");
    assert_eq!(scopes.owner().unwrap().as_str(), "trusty/acme");
}

/// Why: a directory with no remote and no override must fail with the reason,
/// never pick a vault; with an override it uses the override and no owner.
/// Test: itself.
#[test]
fn scope_derive_without_a_remote_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let err = ScopeSet::derive(tmp.path(), None).unwrap_err();
    assert!(
        matches!(err, SecretsError::ScopeUndetermined { .. }),
        "{err:?}"
    );

    let shared = VaultName::new("trusty/acme/shared").unwrap();
    let scopes = ScopeSet::derive(tmp.path(), Some(shared.clone())).unwrap();
    assert_eq!(scopes.project(), &shared);
    assert!(scopes.owner().is_none());
}

/// Why: DOC-74 §13 Q5 — `secrets.vault` replaces the project vault only; the
/// owner vault still follows the remote, and lookup stays project-first.
/// Test: itself.
#[test]
fn scope_override_replaces_only_the_project_vault() {
    let owner = OwnerName::new("acme").unwrap();
    let repo = RepoName::new("web").unwrap();
    let shared = VaultName::new("trusty/acme/shared").unwrap();
    let scopes = ScopeSet::from_identity(&owner, &repo, Some(shared));
    let order: Vec<&str> = scopes.lookup_order().map(VaultName::as_str).collect();
    assert_eq!(order, ["trusty/acme/shared", "trusty/acme"]);

    let response = ScopeSet::from_identity(&owner, &repo, None).to_response();
    let kinds: Vec<(ScopeKind, &str)> = response
        .scopes
        .iter()
        .map(|s| (s.kind, s.vault.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            (ScopeKind::Project, "trusty/acme/web"),
            (ScopeKind::Owner, "trusty/acme")
        ]
    );
}
