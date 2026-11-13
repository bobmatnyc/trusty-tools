//! Tests for `tm secrets assign` (#11015), on the `tests.rs` harness.
//!
//! The harness's cwd checkout is `Acme/Web`; each test adds the checkouts it
//! names and maps `owner/repo` to them through [`FakeResolver`], standing in
//! for #10368's registry. Every run goes through `run_seam`, which scans
//! stdout, the error text and TRACE logs for the value (`assert_no_value`).

use std::sync::atomic::{AtomicBool, Ordering};

use trusty_secrets::ancestry::{ProcessError, ProcessTable, StartTime};

use super::*;
use crate::commands::secrets::assign::{
    AGENT_OTHER_PROJECT, CHECKOUT_MISMATCH, Caller, NOT_REGISTERED, OTHER_OWNER, PROJECT_FORM,
    ProjectName, ProjectResolver, REGISTRY_UNREADABLE, RegistryResolver, SRC_FORMS, SRC_UNPINNED,
    Seam, agent_in, caller_from, parse_source,
};
use crate::commands::session_project::{project_notice, registry_dir_under_home};

/// The project vault of the second same-owner checkout.
const API_VAULT: &str = "trusty/acme/api";

/// Maps `owner/repo` to a checkout; anything else is unregistered.
struct FakeResolver(Vec<(&'static str, PathBuf)>);

impl ProjectResolver for FakeResolver {
    fn checkout(&self, project: &ProjectName) -> anyhow::Result<PathBuf> {
        let name = format!("{}/{}", project.owner, project.repo);
        self.0
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, dir)| dir.clone())
            .ok_or_else(|| anyhow::anyhow!(NOT_REGISTERED))
    }
}

/// A git checkout `name` in the harness dir with `remote` as its origin.
fn checkout(h: &Harness, name: &str, remote: &str) -> PathBuf {
    let dir = h.tmp.path().join(name);
    std::fs::create_dir(&dir).expect("mkdir checkout");
    git(&dir, &["init", "-q"]);
    git(&dir, &["remote", "add", "origin", remote]);
    dir
}

/// The harness's own checkout plus `Acme/Api`, both registered.
fn resolver(h: &Harness) -> FakeResolver {
    let api = checkout(h, "api", "git@github.com:Acme/Api.git");
    FakeResolver(vec![("acme/web", h.repo.clone()), ("acme/api", api)])
}

fn person() -> Caller {
    Caller::Person
}

/// A PM or a subagent: any Claude Code session but the Architect's.
fn agent() -> Caller {
    Caller::Agent
}

fn architect() -> Caller {
    Caller::Architect
}

/// Run `tm secrets <args>` in the harness checkout with `resolver`.
async fn assign_run(
    h: &Harness,
    resolver: &dyn ProjectResolver,
    caller: fn() -> Caller,
    args: &[&str],
) -> Outcome {
    let seam = Seam { resolver, caller };
    run_seam(&h.client, &h.repo, (SHORT, SHORT), false, seam, args).await
}

#[test]
fn assign_parse_source_accepts_both_forms_and_refuses_bad_ones() {
    for (raw, vault, key) in [
        ("trusty/acme:GH_PAT", OWNER_VAULT, "GH_PAT"),
        ("trusty/acme/web:API_KEY", PROJECT_VAULT, "API_KEY"),
        ("secret://acme/GH_PAT", OWNER_VAULT, "GH_PAT"),
        ("secret://acme/web/API_KEY", PROJECT_VAULT, "API_KEY"),
    ] {
        let (v, k) = parse_source(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
        assert_eq!((v.as_str(), k.as_str()), (vault, key), "{raw}");
    }
    for raw in [
        "",
        "GH_PAT",
        "trusty/acme",
        "acme:GH_PAT",
        "trusty/acme:",
        "trusty/a/b/c:K",
        "secret://",
        "secret://acme//K",
        "secret://a/b/c/K",
        "/home/me/repo:K",
    ] {
        let err = parse_source(raw).expect_err(raw).to_string();
        assert!(err.starts_with(SRC_FORMS), "{raw}: {err}");
    }
    let unpinned = parse_source("secret://GH_PAT").expect_err("unpinned");
    assert_eq!(unpinned.to_string(), SRC_UNPINNED);
    // The refusal never quotes what was typed: it may be a pasted value. A
    // space makes it no key; an `Ok` is never printed.
    let typed = format!("trusty/acme:{VALUE} x");
    let err = parse_source(&typed).err().map(|e| e.to_string());
    let err = err.unwrap_or_default();
    assert!(err.starts_with(SRC_FORMS), "a pasted value parsed as a key");
    assert!(!err.contains(MIDDLE), "{err}");
}

#[test]
fn assign_project_name_accepts_owner_repo_only() {
    let name = ProjectName::parse("Acme/Web").expect("owner/repo");
    assert_eq!((name.owner.as_str(), name.repo.as_str()), ("acme", "web"));
    for raw in [
        "",
        "acme",
        "/abs/path",
        "acme/web/x",
        "../web",
        "acme/..",
        "~/web",
    ] {
        let err = ProjectName::parse(raw).expect_err(raw);
        assert_eq!(err.to_string(), PROJECT_FORM, "{raw}");
    }
}

/// A process table where every read fails.
struct Unreadable;

impl ProcessTable for Unreadable {
    fn parent(&self, pid: u32) -> Result<u32, ProcessError> {
        Err(ProcessError::Unreadable { pid })
    }
    fn start_time(&self, pid: u32) -> Result<StartTime, ProcessError> {
        Err(ProcessError::Unreadable { pid })
    }
}

#[test]
fn assign_agent_judgement_fails_closed() {
    assert!(agent_in(&Unreadable, 4242));
}

/// The production caller mapping, row by row: no agent ancestor is a person
/// without consulting the Architect check; with one, only `Ok(true)` is the
/// Architect, and `Ok(false)` or an error is an agent (fail closed).
#[test]
fn assign_caller_from_maps_every_row_and_fails_closed() {
    let unbound = || Ok(false);
    let errors = || Err(anyhow::anyhow!("no trusted home"));
    let unasked = || -> anyhow::Result<bool> { panic!("asked without an ancestor") };
    assert_eq!(caller_from(false, unasked), Caller::Person);
    assert_eq!(caller_from(true, || Ok(true)), Caller::Architect);
    assert_eq!(caller_from(true, unbound), Caller::Agent);
    assert_eq!(caller_from(true, errors), Caller::Agent);
}

#[tokio::test]
async fn assign_copies_vault_key_into_project_without_printing_value() {
    let h = harness().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let r = resolver(&h);
    let args = ["assign", "trusty/acme:GH_PAT", "--project", "acme/api"];
    let outcome = assign_run(&h, &r, person, &args).await;
    assert_eq!(outcome.err, None);
    assert_eq!(
        outcome.out,
        "assign to trusty/acme/api\ncopied GH_PAT\nkeychain -> keychain: 1 copied, 0 not copied\n"
    );
    for text in [
        &outcome.out,
        outcome.err.as_deref().unwrap_or(""),
        &outcome.logs,
    ] {
        assert!(!text.contains(VALUE), "{text}");
    }
    assert_eq!(
        stored(&h.keychain, API_VAULT, "GH_PAT").as_deref(),
        Some(VALUE)
    );
    // #11015: the cwd's own project is untouched.
    assert_eq!(stored(&h.keychain, PROJECT_VAULT, "GH_PAT"), None);
}

#[tokio::test]
async fn assign_accepts_secret_ref_and_vault_key_forms() {
    let h = harness().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let r = resolver(&h);
    for (src, dest) in [
        ("secret://acme/GH_PAT", "FROM_REF"),
        ("trusty/acme:GH_PAT", "FROM_VAULT"),
    ] {
        let args = ["assign", src, "--project", "acme/api", "--as", dest];
        let outcome = assign_run(&h, &r, person, &args).await;
        assert_eq!(outcome.err, None, "{src}");
        assert_eq!(stored(&h.keychain, API_VAULT, dest).as_deref(), Some(VALUE));
    }
    let bad = ["assign", "secret://GH_PAT", "--project", "acme/api"];
    let outcome = assign_run(&h, &r, person, &bad).await;
    assert!(outcome.err().starts_with(SRC_UNPINNED), "{}", outcome.err());
}

#[tokio::test]
async fn assign_defaults_dest_to_source_key_and_honours_as() {
    let h = harness().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let r = resolver(&h);
    let plain = ["assign", "trusty/acme:GH_PAT", "--project", "acme/api"];
    assert_eq!(assign_run(&h, &r, person, &plain).await.err, None);
    let renamed = [
        "assign",
        "trusty/acme:GH_PAT",
        "--project",
        "acme/api",
        "--as",
        "GITHUB_TOKEN",
    ];
    let outcome = assign_run(&h, &r, person, &renamed).await;
    assert_eq!(outcome.err, None);
    assert!(
        outcome.out.contains("copied GITHUB_TOKEN\n"),
        "{}",
        outcome.out
    );
    assert_eq!(
        stored(&h.keychain, API_VAULT, "GH_PAT").as_deref(),
        Some(VALUE)
    );
    assert_eq!(
        stored(&h.keychain, API_VAULT, "GITHUB_TOKEN").as_deref(),
        Some(VALUE)
    );
}

/// D3: a re-run is `copy`'s upsert — the destination takes the source's
/// current value and the run reports `copied`, with no `--force`.
#[tokio::test]
async fn assign_rerun_follows_copy_conflict_behaviour() {
    let h = harness().await;
    let r = resolver(&h);
    let args = ["assign", "trusty/acme:GH_PAT", "--project", "acme/api"];
    seed_owner(&h, "GH_PAT", SHORT).await;
    assert_eq!(assign_run(&h, &r, person, &args).await.err, None);
    seed_owner(&h, "GH_PAT", VALUE).await;
    let again = assign_run(&h, &r, person, &args).await;
    assert_eq!(again.err, None);
    assert!(again.out.contains("copied GH_PAT\n"), "{}", again.out);
    assert_eq!(
        stored(&h.keychain, API_VAULT, "GH_PAT").as_deref(),
        Some(VALUE)
    );
}

/// The registry resolver refuses a project no row holds; so does a fake
/// asked for a name it does not hold, and a path is never a project.
#[tokio::test]
async fn assign_unknown_project_is_a_fixed_error_and_writes_nothing() {
    let h = harness().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let args = ["assign", "trusty/acme:GH_PAT", "--project", "acme/api"];
    let empty = RegistryResolver {
        dir: Some(h.tmp.path().join("empty-registry")),
    };
    let pending = assign_run(&h, &empty, person, &args).await;
    assert!(
        pending.err().starts_with(NOT_REGISTERED),
        "{}",
        pending.err()
    );
    let r = resolver(&h);
    let unknown = ["assign", "trusty/acme:GH_PAT", "--project", "acme/nope"];
    let outcome = assign_run(&h, &r, person, &unknown).await;
    assert!(
        outcome.err().starts_with(NOT_REGISTERED),
        "{}",
        outcome.err()
    );
    let path = h.repo.to_str().expect("utf-8").to_owned();
    let by_path = ["assign", "trusty/acme:GH_PAT", "--project", &path];
    let outcome = assign_run(&h, &r, person, &by_path).await;
    assert!(outcome.err().starts_with(PROJECT_FORM), "{}", outcome.err());
    assert_eq!(pending.out, "");
    for vault in [API_VAULT, PROJECT_VAULT, "trusty/acme/nope"] {
        assert_eq!(stored(&h.keychain, vault, "GH_PAT"), None, "{vault}");
    }
}

/// A value source that records any read.
struct Tripwire(AtomicBool);

impl ValueSource for Tripwire {
    fn read(&self) -> anyhow::Result<String> {
        self.0.store(true, Ordering::SeqCst);
        Ok(SHORT.to_owned())
    }
}

#[tokio::test]
async fn assign_never_touches_clipboard() {
    let h = harness().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let r = resolver(&h);
    let (clipboard, stdin) = (
        Tripwire(AtomicBool::new(false)),
        Tripwire(AtomicBool::new(false)),
    );
    let ctx = Ctx {
        client: &h.client,
        project: &h.repo,
        clipboard: &clipboard,
        stdin: &stdin,
        ci: false,
    };
    let argv = [
        "tm",
        "secrets",
        "assign",
        "trusty/acme:GH_PAT",
        "--project",
        "acme/api",
    ];
    let Some(Command::Secrets { action }) = Cli::try_parse_from(argv).expect("parse").command
    else {
        panic!("not a secrets command");
    };
    let seam = Seam {
        resolver: &r,
        caller: person,
    };
    let mut out = Vec::new();
    dispatch_with(&ctx, seam, action, &mut out)
        .await
        .expect("assign");
    assert!(
        !clipboard.0.load(Ordering::SeqCst),
        "the clipboard was read"
    );
    assert!(!stdin.0.load(Ordering::SeqCst), "stdin was read");
    assert_eq!(
        stored(&h.keychain, API_VAULT, "GH_PAT").as_deref(),
        Some(VALUE)
    );
}

/// The server's per-key deny binds an agent's assign of a denied source.
#[tokio::test]
async fn assign_denied_source_is_refused_for_an_agent() {
    let h = harness_agent().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let owner = VaultName::new(OWNER_VAULT).expect("vault");
    let key = SecretKey::new("GH_PAT").expect("key");
    h.keychain
        .set_control_item(ControlItem::AgentDeny, &owner, Some(&key), true)
        .expect("deny");
    let r = resolver(&h);
    let args = [
        "assign",
        "trusty/acme:GH_PAT",
        "--project",
        "acme/web",
        "--as",
        "DEST",
    ];
    let outcome = assign_run(&h, &r, agent, &args).await;
    assert!(outcome.err().contains("per-key deny"), "{}", outcome.err());
    assert_eq!(stored(&h.keychain, PROJECT_VAULT, "DEST"), None);
}

/// D2, owner ruling Dfx: a PM or a subagent may assign into its own
/// session's project only, so `--project` cannot carry its write past the
/// checkout it runs in.
#[tokio::test]
async fn assign_by_an_agent_cannot_widen_its_write_reach() {
    let h = harness_agent().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let r = resolver(&h);
    let other = ["assign", "trusty/acme:GH_PAT", "--project", "acme/api"];
    let refused = assign_run(&h, &r, agent, &other).await;
    assert!(
        refused.err().starts_with(AGENT_OTHER_PROJECT),
        "{}",
        refused.err()
    );
    assert_eq!(stored(&h.keychain, API_VAULT, "GH_PAT"), None);
    let own = [
        "assign",
        "trusty/acme:GH_PAT",
        "--project",
        "acme/web",
        "--as",
        "DEST",
    ];
    assert_eq!(assign_run(&h, &r, agent, &own).await.err, None);
    assert_eq!(
        stored(&h.keychain, PROJECT_VAULT, "DEST").as_deref(),
        Some(VALUE)
    );
}

/// Owner ruling Dfx: the Architect session assigns across projects of the
/// source's owner; the server, judging it an agent, still applies the deny.
#[tokio::test]
async fn assign_by_the_architect_reaches_another_project() {
    let h = harness_agent().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let r = resolver(&h);
    let args = ["assign", "trusty/acme:GH_PAT", "--project", "acme/api"];
    let outcome = assign_run(&h, &r, architect, &args).await;
    assert_eq!(outcome.err, None);
    assert!(
        outcome.out.starts_with("assign to trusty/acme/api\n"),
        "{}",
        outcome.out
    );
    assert_eq!(
        stored(&h.keychain, API_VAULT, "GH_PAT").as_deref(),
        Some(VALUE)
    );
    assert_eq!(stored(&h.keychain, PROJECT_VAULT, "GH_PAT"), None);
}

/// D2: a project under another owner than the source vault's is refused for
/// every caller, the Architect included, and so is a checkout whose remote
/// is not the owner the name gave.
#[tokio::test]
async fn assign_to_another_owner_is_refused() {
    let h = harness().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let foreign = checkout(&h, "foreign", "git@github.com:Other/Thing.git");
    let r = FakeResolver(vec![
        ("other/thing", foreign.clone()),
        ("acme/thing", foreign),
    ]);
    let args = ["assign", "trusty/acme:GH_PAT", "--project", "other/thing"];
    for caller in [person as fn() -> Caller, architect] {
        let outcome = assign_run(&h, &r, caller, &args).await;
        assert!(outcome.err().starts_with(OTHER_OWNER), "{}", outcome.err());
    }
    let lying = ["assign", "trusty/acme:GH_PAT", "--project", "acme/thing"];
    let outcome = assign_run(&h, &r, person, &lying).await;
    assert!(
        outcome.err().starts_with(CHECKOUT_MISMATCH),
        "{}",
        outcome.err()
    );
    assert_eq!(stored(&h.keychain, "trusty/other/thing", "GH_PAT"), None);
}

/// A resolver answer the server cannot resolve — a directory with no
/// checkout — is the server's fixed refusal, and nothing is written.
#[tokio::test]
async fn assign_to_an_unresolvable_checkout_is_refused() {
    let h = harness().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let bare = h.tmp.path().join("bare");
    std::fs::create_dir(&bare).expect("mkdir bare");
    let r = FakeResolver(vec![("acme/bare", bare)]);
    let args = ["assign", "trusty/acme:GH_PAT", "--project", "acme/bare"];
    let outcome = assign_run(&h, &r, person, &args).await;
    assert!(
        outcome.err().starts_with("tm secrets: "),
        "{}",
        outcome.err()
    );
    assert_eq!(outcome.out, "");
    assert_eq!(stored(&h.keychain, "trusty/acme/bare", "GH_PAT"), None);
}

/// An agent whose cwd the server cannot resolve is refused: its own project
/// is unknown, so no `--project` can match it.
#[tokio::test]
async fn assign_by_an_agent_from_an_unresolvable_cwd_is_refused() {
    let h = harness_agent().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let r = resolver(&h);
    let bare = h.tmp.path().join("bare");
    std::fs::create_dir(&bare).expect("mkdir bare");
    let seam = Seam {
        resolver: &r,
        caller: agent,
    };
    let args = ["assign", "trusty/acme:GH_PAT", "--project", "acme/web"];
    let outcome = run_seam(&h.client, &bare, (SHORT, SHORT), false, seam, &args).await;
    assert!(outcome.err.is_some());
    assert_eq!(stored(&h.keychain, PROJECT_VAULT, "GH_PAT"), None);
}

/// #10368 regression: a session start in a checkout registers it, and
/// `assign --project` then resolves it through the real registry resolver
/// instead of refusing with `NOT_REGISTERED`. The registry lives under a
/// temp home, never the real one.
#[tokio::test]
async fn a_session_started_checkout_is_an_assign_target() {
    let h = harness().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let home = TempDir::new().expect("tmp home");
    let api = checkout(&h, "api", "git@github.com:Acme/Api.git");

    let notice = project_notice(&api, Some(home.path())).await;

    assert_eq!(notice, None, "the session start registered the checkout");
    let registry = RegistryResolver {
        dir: Some(registry_dir_under_home(home.path())),
    };
    let args = ["assign", "trusty/acme:GH_PAT", "--project", "acme/api"];
    let outcome = assign_run(&h, &registry, person, &args).await;
    assert_eq!(outcome.err, None);
    assert!(
        outcome.out.starts_with("assign to trusty/acme/api\n"),
        "{}",
        outcome.out
    );
    assert_eq!(
        stored(&h.keychain, API_VAULT, "GH_PAT").as_deref(),
        Some(VALUE)
    );
}

/// #10368 fail closed: a session start against a corrupt registry warns and
/// registers nothing, and `assign` refuses with a fixed error and writes
/// nothing.
#[tokio::test]
async fn a_failed_session_start_registration_leaves_assign_refusing() {
    let h = harness().await;
    seed_owner(&h, "GH_PAT", VALUE).await;
    let home = TempDir::new().expect("tmp home");
    let dir = registry_dir_under_home(home.path());
    std::fs::create_dir_all(&dir).expect("mkdir registry");
    std::fs::write(dir.join("projects.json"), "{ not json").expect("corrupt");
    let api = checkout(&h, "api", "git@github.com:Acme/Api.git");

    let notice = project_notice(&api, Some(home.path())).await;

    let notice = notice.expect("a failed registration is reported");
    assert!(notice.contains("was not registered"), "{notice}");
    let registry = RegistryResolver { dir: Some(dir) };
    let args = ["assign", "trusty/acme:GH_PAT", "--project", "acme/api"];
    let outcome = assign_run(&h, &registry, person, &args).await;
    assert!(
        outcome.err().starts_with(REGISTRY_UNREADABLE),
        "{}",
        outcome.err()
    );
    assert_eq!(outcome.out, "");
    assert_eq!(stored(&h.keychain, API_VAULT, "GH_PAT"), None);
    assert!(
        project_notice(&api, None).await.is_some(),
        "no home is reported"
    );
}

/// #10368: a session start in a second clone of a registered project names
/// both checkouts, adds no row, and leaves the registry bytes as they were.
#[tokio::test]
async fn a_session_start_reports_a_project_registered_to_another_clone() {
    let h = harness().await;
    let home = TempDir::new().expect("tmp home");
    let old = checkout(&h, "old-api", "git@github.com:Acme/Api.git");
    let api = checkout(&h, "api", "git@github.com:Acme/Api.git");
    assert_eq!(project_notice(&old, Some(home.path())).await, None);
    let registry = registry_dir_under_home(home.path()).join("projects.json");
    let before = std::fs::read(&registry).expect("registered");

    let notice = project_notice(&api, Some(home.path())).await;

    let notice = notice.expect("the other clone is reported");
    let old = old.canonicalize().expect("canonical old");
    let api = api.canonicalize().expect("canonical api");
    assert!(notice.contains(&old.display().to_string()), "{notice}");
    assert!(notice.contains(&api.display().to_string()), "{notice}");
    assert_eq!(std::fs::read(&registry).expect("still there"), before);
}
