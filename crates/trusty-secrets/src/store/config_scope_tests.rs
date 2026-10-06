//! Unit tests for config resolution and scope derivation.
//!
//! Test: itself.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use super::config::{
    MachineSecretsConfig, ProjectSecretsConfig, ResolvedConfig, check_project_backend,
    check_project_backend_for, load_machine_at, load_project_at, resolve,
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
        ..MachineSecretsConfig::default()
    };
    let project = ProjectSecretsConfig {
        backend: Some(backend("keeper")),
        vault: Some(VaultName::new("trusty/acme/shared").unwrap()),
    };
    let bare_project = ProjectSecretsConfig::default();

    // #9326: the build default — `keychain` on macOS, `file` elsewhere.
    assert_eq!(resolve(None, None).backend, crate::store::default_backend());
    let default = ResolvedConfig::default();
    assert_eq!(default.backend, crate::store::default_backend());
    #[cfg(target_os = "macos")]
    assert_eq!(default.backend.as_str(), "keychain");
    assert!(default.vault_override.is_none());
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

/// Why: #9326, Architect ruling (basis ruling 06 R2) — on a Keychain build
/// only the untracked machine config may select `file`; the refusal names
/// the machine key and never echoes the tracked file.
/// Red when `check_project_backend_for` lets the project `file` through.
/// Test: itself.
#[test]
fn config_tracked_file_backend_is_refused_on_a_keychain_build() {
    let tmp = TempDir::new().unwrap();
    let path = write(
        tmp.path(),
        "trusty-secrets.yaml",
        "# SENTINEL-9326-repo-content\nsecrets:\n  backend: file\n",
    );
    let tracked = load_project_at(&path).unwrap().unwrap();
    let err = check_project_backend_for(Some(&tracked), &path, true).unwrap_err();
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains("SENTINEL-9326"), "{shown}");
    assert!(shown.contains("secrets.default_backend"), "{shown}");
    match err {
        SecretsError::TrackedBackendRefused { path: refused } => assert_eq!(refused, path),
        other => panic!("expected TrackedBackendRefused: {other:?}"),
    }

    // Off a Keychain build `file` stays allowed; `keychain` is allowed on both.
    check_project_backend_for(Some(&tracked), &path, false).unwrap();
    let keychain = ProjectSecretsConfig {
        backend: Some(backend("keychain")),
        ..ProjectSecretsConfig::default()
    };
    check_project_backend_for(Some(&keychain), &path, true).unwrap();
    check_project_backend_for(None, &path, true).unwrap();
    // The machine config is never checked: it may select `file` anywhere.
    let machine = MachineSecretsConfig {
        default_backend: Some(backend("file")),
        ..MachineSecretsConfig::default()
    };
    assert_eq!(resolve(None, Some(&machine)).backend.as_str(), "file");
    #[cfg(target_os = "macos")]
    check_project_backend(Some(&tracked), &path).unwrap_err();
    #[cfg(not(target_os = "macos"))]
    check_project_backend(Some(&tracked), &path).unwrap();
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
        // #9328: git reads both spellings as `ssh`.
        "git+ssh://git@github.com/bobmatnyc/trusty-tools.git",
        "ssh+git://git@github.com/bobmatnyc/trusty-tools.git",
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
        let reason = parse_remote_identity(url).expect_err(url).reason();
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
    let scopes = ScopeSet::derive(tmp.path(), None, None).unwrap();
    assert_eq!(scopes.project().as_str(), "trusty/acme/web");
    assert_eq!(scopes.owner().unwrap().as_str(), "trusty/acme");
}

/// Why: a directory with no remote must fail with the reason, never pick a
/// vault. #9328: with no remote there is no owner to check an override
/// against, so a tracked or machine override does not rescue it.
/// Test: itself.
#[test]
fn scope_derive_without_a_remote_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let shared = VaultName::new("trusty/acme/shared").unwrap();
    let machine = MachineSecretsConfig {
        project_vaults: [("acme/web".to_string(), shared.clone())].into(),
        ..MachineSecretsConfig::default()
    };
    for (tracked, machine) in [
        (None, None),
        (Some(shared.clone()), None),
        (None, Some(&machine)),
    ] {
        let err = ScopeSet::derive(tmp.path(), tracked, machine).unwrap_err();
        assert!(
            matches!(err, SecretsError::ScopeUndetermined { .. }),
            "{err:?}"
        );
    }
}

/// Why: DOC-74 §13 Q5 — `secrets.vault` replaces the project vault only; the
/// owner vault still follows the remote, and lookup stays project-first.
/// Test: itself.
#[test]
fn scope_override_replaces_only_the_project_vault() {
    let owner = OwnerName::new("acme").unwrap();
    let repo = RepoName::new("web").unwrap();
    let shared = VaultName::new("trusty/acme/shared").unwrap();
    let scopes =
        ScopeSet::from_identity(&owner, &repo, Some(VaultOverride::Tracked(shared))).unwrap();
    let order: Vec<&str> = scopes.lookup_order().map(VaultName::as_str).collect();
    assert_eq!(order, ["trusty/acme/shared", "trusty/acme"]);

    let response = ScopeSet::from_identity(&owner, &repo, None)
        .unwrap()
        .to_response();
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

/// A temp checkout whose `origin` is `url`.
fn checkout_with_origin(url: &str) -> TempDir {
    let tmp = TempDir::new().unwrap();
    git(tmp.path(), &["init", "-q"]);
    git(tmp.path(), &["remote", "add", "origin", url]);
    tmp
}

/// Why: #9328 vector (c), owner ruling 06 R3 — the remote host was dropped,
/// so `evil.example/acme/app` mapped to `github.com/acme/app`'s vaults. Any
/// host but github.com is now refused with fixed text that names neither the
/// host nor the URL; github.com in https and ssh forms still derives. Only
/// the https, ssh and scp forms are accepted, so github.com over `file`,
/// `git`, `http` or a `<helper>::` prefix is the same refusal.
/// Red on the unfixed code: `parse_remote_identity` returns `Ok` for every
/// URL in the first table.
/// Test: itself.
#[test]
fn scope_non_github_remote_is_refused_with_fixed_text() {
    for url in [
        "https://evil.example/acme/app.git",
        "git@evil.example:acme/app.git",
        "ssh://git@evil.example:22/acme/app",
        "https://github.com.evil.example/acme/app",
        "https://evil.example#@github.com/acme/app",
        "https://gitlab.com/acme/app.git",
        // #9328: github.com over a scheme DOC-74 §15.3 does not accept.
        "file://github.com/acme/app",
        "git://github.com/acme/app",
        "http://github.com/acme/app",
        "x::https://github.com/acme/app",
    ] {
        let refusal = parse_remote_identity(url).expect_err(url);
        assert_eq!(refusal, RemoteRefusal::UnsupportedHost, "{url}");
        assert_eq!(refusal.reason(), "the origin remote is not on github.com");
    }

    let evil = checkout_with_origin("https://evil.example/acme/app.git");
    let err = ScopeSet::derive(evil.path(), None, None).unwrap_err();
    assert!(
        matches!(err, SecretsError::UnsupportedRemoteHost { .. }),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(
        text.contains("only github.com remotes are supported"),
        "{text}"
    );
    assert!(!text.contains("evil.example"), "{text}");

    for url in [
        "https://github.com/acme/app.git",
        "https://GitHub.com/Acme/App",
        "git@github.com:acme/app.git",
        "ssh://git@github.com/acme/app.git",
    ] {
        let checkout = checkout_with_origin(url);
        let scopes = ScopeSet::derive(checkout.path(), None, None).expect(url);
        assert_eq!(scopes.project().as_str(), "trusty/acme/app", "{url}");
        assert_eq!(scopes.owner().unwrap().as_str(), "trusty/acme", "{url}");
    }
}

/// Why: #9328 vector (b), owner ruling 06 R2 — the tracked repo file's
/// `secrets.vault` could name any vault, so a PR could point a checkout at
/// another project's secrets. A tracked override outside
/// `trusty/<remote-owner>/*` is refused (never replaced by the derived
/// vault); the same vault from the untracked machine config is honoured.
/// Red on the unfixed code: `derive` returns the victim vault as the
/// project vault.
/// Test: itself.
#[test]
fn scope_tracked_override_outside_the_owner_is_refused() {
    let checkout = checkout_with_origin("git@github.com:Acme/App.git");
    let vault = |name: &str| VaultName::new(name).unwrap();

    for wide in [
        "trusty/victim/prod-repo",
        "trusty/victim",
        "trusty/acme",
        "trusty/acmex/app",
    ] {
        let err = ScopeSet::derive(checkout.path(), Some(vault(wide)), None).unwrap_err();
        match &err {
            SecretsError::VaultOutOfScope { vault, .. } => assert_eq!(vault, wide),
            other => panic!("{wide}: expected VaultOutOfScope, got {other:?}"),
        }
    }
    let same_owner =
        ScopeSet::derive(checkout.path(), Some(vault("trusty/acme/shared")), None).unwrap();
    assert_eq!(same_owner.project().as_str(), "trusty/acme/shared");

    let tmp = TempDir::new().unwrap();
    let path = write(
        tmp.path(),
        "machine.yaml",
        "secrets:\n  project_vaults:\n    Acme/App: trusty/victim/prod-repo\n    other/repo: trusty/x/y\n",
    );
    let machine = load_machine_at(&path).unwrap().unwrap();
    let scopes = ScopeSet::derive(
        checkout.path(),
        Some(vault("trusty/acme/ignored")),
        Some(&machine),
    )
    .unwrap();
    let order: Vec<&str> = scopes.lookup_order().map(VaultName::as_str).collect();
    assert_eq!(order, ["trusty/victim/prod-repo", "trusty/acme"]);

    let unrelated = MachineSecretsConfig {
        project_vaults: [("other/repo".to_string(), vault("trusty/x/y"))].into(),
        ..MachineSecretsConfig::default()
    };
    let err = ScopeSet::derive(
        checkout.path(),
        Some(vault("trusty/victim/prod-repo")),
        Some(&unrelated),
    )
    .unwrap_err();
    assert!(
        matches!(err, SecretsError::VaultOutOfScope { .. }),
        "{err:?}"
    );
}
