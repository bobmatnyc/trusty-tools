//! The one IPC command this shell exposes, and the shapes it hands the window.
//!
//! Why: DOC-68 §11 (`SPEC-AUDITPKG-11~draft`) fixes the shell as "a view over
//! `Session::execute`, never a second place a capability can live". Two things
//! follow, and both are visible in this file. The command calls
//! `Session::execute` IN-PROCESS — there is no HTTP endpoint and no `taudit`
//! subprocess — and it adds no capability of its own: it names
//! `Command::Guided`, which already exists and already has a CLI arm.
//!
//! What: [`GuidedView`] and its members are serialisable mirrors of
//! `trusty_audit::session::GuidedStatus`. They exist because that type is not
//! `Serialize` — it is an API shape, not a wire format — and because the window
//! needs the values rather than prose. This module copies fields and nothing
//! else; the Svelte side chooses the words, which is what leaves the CLI free to
//! phrase the same states as shell commands.
//!
//! Every type it reads is `#[non_exhaustive]`, so a later milestone's field or
//! variant cannot be silently dropped here: a new `NextStep` variant reaches
//! [`next_view`]'s fallback and surfaces in the window as an error naming
//! itself, rather than rendering as one of the four states it is not.
//!
//! Test: `super::view_tests`. `Session::execute`'s own behaviour is proven by
//! `trusty_audit::session::session_tests`.

use serde::Serialize;
use trusty_audit::config::EngagementConfig;
use trusty_audit::manifest::AuditManifest;
use trusty_audit::session::{Command, GuidedStatus, NextStep, Outcome};
use trusty_audit::tools::ToolStatus;
use trusty_audit::workdir::{WorkDir, WORKDIR_ENV};
use trusty_audit::Session;

/// One repository the companion manifest names.
#[derive(Debug, Serialize)]
pub struct RepositoryView {
    name: String,
    path: String,
}

/// The engagement metadata a previous run left behind.
#[derive(Debug, Serialize)]
pub struct ManifestView {
    title: String,
    client: Option<String>,
    analyst: Option<String>,
    repositories: Vec<RepositoryView>,
}

/// One pinned tool's install state.
///
/// `version` is `None` both when the binary is missing and when it is present
/// but this client did not place it. The window renders those two differently,
/// which is why `installed` travels separately rather than being inferred.
#[derive(Debug, Serialize)]
pub struct ToolView {
    name: String,
    installed: bool,
    version: Option<String>,
    path: String,
}

/// What the guided flow says to do next.
///
/// Serialised internally tagged, so the TypeScript side discriminates on `kind`
/// and still reaches `missing`.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NextStepView {
    /// No repositories are known yet.
    SelectRepositories,
    /// Repositories are known; these binaries are not installed.
    InstallTools {
        /// Binary names, in the order the client installs them.
        missing: Vec<String>,
    },
    /// Selection and tooling are both in place.
    ReadyForRun,
    /// A sweep has audited something; assemble the deliverable.
    ReturnPackage,
}

/// Everything `Command::Guided` reports, as the window consumes it.
#[derive(Debug, Serialize)]
pub struct GuidedView {
    root: String,
    manifest: Option<ManifestView>,
    tools: Vec<ToolView>,
    next: NextStepView,
}

impl GuidedView {
    /// Copy a `GuidedStatus` into the window's shape.
    ///
    /// # Errors
    ///
    /// Names the `NextStep` variant this build does not know how to show.
    fn of(status: &GuidedStatus) -> Result<Self, String> {
        Ok(Self {
            root: status.root.display().to_string(),
            manifest: status.manifest.as_ref().map(manifest_view),
            tools: status.tools.iter().map(tool_view).collect(),
            next: next_view(&status.next)?,
        })
    }
}

fn manifest_view(manifest: &AuditManifest) -> ManifestView {
    ManifestView {
        title: manifest.report.title.clone(),
        client: manifest.report.client.clone(),
        analyst: manifest.report.analyst.clone(),
        repositories: manifest
            .repositories
            .iter()
            .map(|repo| RepositoryView {
                name: repo.name.clone(),
                path: repo.path.display().to_string(),
            })
            .collect(),
    }
}

fn tool_view(status: &ToolStatus) -> ToolView {
    ToolView {
        name: status.tool.binary_name().to_owned(),
        installed: status.installed,
        version: status.version.clone(),
        path: status.path.display().to_string(),
    }
}

/// Map the guided flow's state onto the window's.
///
/// Why: `NextStep` is `#[non_exhaustive]`, so this match needs a fallback and
/// the compiler will not report a variant added upstream. The fallback returns
/// an error rather than a nearest-neighbour state or a panic: the window shows
/// the unknown variant's name and stays up, which is a recipient reading
/// "this build does not know that step" instead of one reading a wrong
/// instruction — or one whose window died.
/// What: one arm per known variant, plus that fallback.
/// Test: `super::view_tests::the_other_three_steps_map_one_for_one`.
fn next_view(next: &NextStep) -> Result<NextStepView, String> {
    Ok(match next {
        NextStep::SelectRepositories => NextStepView::SelectRepositories,
        NextStep::InstallTools(missing) => NextStepView::InstallTools {
            missing: missing.iter().map(|t| t.binary_name().to_owned()).collect(),
        },
        NextStep::ReadyForRun => NextStepView::ReadyForRun,
        NextStep::ReturnPackage => NextStepView::ReturnPackage,
        other => {
            return Err(format!(
                "this build of the shell does not know the step {other:?} — \
                 run `trusty-audit guided` from the command line"
            ))
        }
    })
}

/// Build the session this window drives.
///
/// Why: the shell has no argv, so the two paths the CLI takes from flags come
/// from the process environment instead — `TRUSTY_AUDIT_WORKDIR` and the
/// current directory. Resolution goes through the same `WorkDir::resolve` and
/// `EngagementConfig::resolve_path` the CLI uses, so both front ends open the
/// same engagement when launched the same way.
/// What: resolves the work-dir root and the engagement config, then builds a
/// `Session` over them.
/// Test: `super::view_tests::a_session_resolves_under_the_named_root`.
fn session() -> Result<Session, String> {
    let cwd = std::env::current_dir()
        .map_err(|e| format!("cannot determine the current directory: {e}"))?;
    let env_value = std::env::var(WORKDIR_ENV).ok();
    // #5915: the home directory is read here, with the rest of the environment,
    // so `resolve` stays a pure function of what it is handed. The CLI's
    // `main.rs` reads it the same way; omitting it would put this shell's root
    // beside the cwd, which is the placement #5915 removed.
    let home = dirs::home_dir();
    let work = WorkDir::resolve(None, env_value.as_deref(), home.as_deref(), &cwd);
    Ok(Session::new(work).with_config_path(EngagementConfig::resolve_path(None, &cwd)))
}

/// Run `Command::Guided` and hand the window its outcome.
///
/// Why: the whole of phase 1. Rendering is the front end's job, so this returns
/// values rather than the CLI's text.
/// What: builds a session, awaits `Session::execute(Command::Guided)`, and maps
/// the outcome. An `AuditError` becomes its `Display` text, which the window
/// shows — a shell that swallowed it would render an empty panel that looks
/// exactly like an engagement with nothing to report.
/// Test: `super::view_tests`; end to end by launching the app.
#[tauri::command]
pub async fn guided() -> Result<GuidedView, String> {
    let outcome = session()?
        .execute(Command::Guided)
        .await
        .map_err(|e| e.to_string())?;
    match outcome {
        Outcome::Guided(status) => GuidedView::of(&status),
        // `Session::execute` maps `Command::Guided` onto `Outcome::Guided` and
        // nothing else, so anything here is a broken invariant rather than a
        // state to render. `Outcome` is `#[non_exhaustive]`; this arm is what
        // keeps the match total without hiding a real outcome behind a default.
        other => Err(format!(
            "the guided flow returned an unexpected outcome: {other:?}"
        )),
    }
}

#[cfg(test)]
mod view_tests {
    use super::*;
    use trusty_audit::tools::RequiredTool;

    /// Run the real capability against an empty work dir and map what it gives.
    async fn view_of(work: &std::path::Path) -> GuidedView {
        let session = Session::new(WorkDir::new(work));
        let Outcome::Guided(status) = session
            .execute(Command::Guided)
            .await
            .expect("the guided flow runs")
        else {
            panic!("Guided must yield a Guided outcome");
        };
        GuidedView::of(&status).expect("every step this build produces is known")
    }

    /// A first launch: the root exists, no engagement has run, and the step the
    /// window shows is repository selection.
    #[tokio::test]
    async fn a_first_launch_shows_the_root_and_asks_for_repositories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let view = view_of(&tmp.path().join("work")).await;

        assert!(view.root.ends_with("work"), "{}", view.root);
        assert!(view.manifest.is_none());
        assert_eq!(view.tools.len(), RequiredTool::ALL.len());
        assert_eq!(view.next, NextStepView::SelectRepositories);
    }

    /// The window shows `MISSING` / `UNVERIFIED` / `ok` from `installed` and
    /// `version` together, so neither may be dropped or derived from the other.
    /// A binary this client did not place is installed and has no version.
    #[tokio::test]
    async fn a_tool_carries_both_its_install_state_and_its_version() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("work");

        let before = view_of(&root).await;
        assert!(
            before.tools.iter().all(|t| !t.installed),
            "{:?}",
            before.tools
        );

        let work = WorkDir::new(&root);
        Session::new(work.clone())
            .execute(Command::WorkDir)
            .await
            .expect("create the tree");
        std::fs::write(RequiredTool::Tga.path_in(&work), b"stub").expect("stub binary");

        let after = view_of(&root).await;
        let tga = after
            .tools
            .iter()
            .find(|t| t.name == "tga")
            .expect("tga is one of the required tools");
        assert!(tga.installed, "a file this client did not place is present");
        assert!(
            tga.version.is_none(),
            "nothing may claim a version for a binary this client did not install"
        );
    }

    /// The missing binaries are what the window lists, so the names survive the
    /// mapping rather than collapsing to a count.
    #[test]
    fn the_install_step_names_every_missing_binary() {
        let view = next_view(&NextStep::InstallTools(RequiredTool::ALL.to_vec()))
            .expect("InstallTools is a known step");
        let NextStepView::InstallTools { missing } = view else {
            panic!("InstallTools must map to InstallTools");
        };
        assert_eq!(missing.len(), RequiredTool::ALL.len());
        assert!(missing.iter().any(|n| n == "tga"), "{missing:?}");
    }

    #[test]
    fn the_other_three_steps_map_one_for_one() {
        assert_eq!(
            next_view(&NextStep::SelectRepositories).expect("known"),
            NextStepView::SelectRepositories
        );
        assert_eq!(
            next_view(&NextStep::ReadyForRun).expect("known"),
            NextStepView::ReadyForRun
        );
        assert_eq!(
            next_view(&NextStep::ReturnPackage).expect("known"),
            NextStepView::ReturnPackage
        );
    }

    /// The field names and the enum tagging are a contract between Rust and
    /// the TypeScript in `ui/src/lib/session.ts`, and no compiler checks it —
    /// a renamed field would reach the window as `undefined` and render a
    /// blank panel rather than failing anywhere. So the shape is asserted
    /// against a fixture engagement that exercises all three tool states the
    /// window distinguishes.
    #[tokio::test]
    async fn guided_serialises_to_the_shape_the_window_reads() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("work");
        let work = WorkDir::new(&root);
        Session::new(work.clone())
            .execute(Command::WorkDir)
            .await
            .expect("create the tree");
        std::fs::write(
            work.path(trusty_audit::workdir::Area::Output)
                .join("manifest.toml"),
            "[report]\ntitle = \"Acme\"\nclient = \"Acme Inc\"\n",
        )
        .expect("write manifest");
        std::fs::write(RequiredTool::Tga.path_in(&work), b"stub").expect("stub binary");

        let json = serde_json::to_value(view_of(&root).await).expect("serialises");

        assert!(json["root"].is_string());
        assert_eq!(json["manifest"]["title"], "Acme");
        assert_eq!(json["manifest"]["client"], "Acme Inc");
        assert!(json["manifest"]["analyst"].is_null());
        assert_eq!(json["tools"][0]["name"], "tga");
        assert_eq!(json["tools"][0]["installed"], true);
        assert!(json["tools"][0]["version"].is_null());
        // No repositories in the manifest, so selection still comes first.
        assert_eq!(json["next"]["kind"], "select-repositories");
    }

    /// The shell resolves its working directory the way the CLI does, so the
    /// two front ends open the same engagement when launched the same way.
    #[test]
    fn a_session_resolves_under_the_named_root() {
        let cwd = std::path::Path::new("/engagement");
        let home = std::path::Path::new("/home/analyst");
        let work = WorkDir::resolve(None, Some("/engagement/work"), Some(home), cwd);
        assert_eq!(
            Session::new(work).work_dir().root(),
            std::path::Path::new("/engagement/work")
        );
    }

    /// With nothing in the environment the shell lands on the home root #5915
    /// moved the default to — not beside the cwd, which is where an emailed
    /// package gets unzipped and where `trusty-search` refuses to index.
    ///
    /// `session()` reads the process environment, so this asserts the arguments
    /// it passes rather than calling it. That indirection is exactly what let
    /// #5929's signature change reach `main.rs` and miss this shell.
    #[test]
    fn the_default_root_is_the_home_one_the_cli_uses() {
        let cwd = std::path::Path::new("/engagement");
        let home = std::path::Path::new("/home/analyst");
        let work = WorkDir::resolve(None, None, Some(home), cwd);
        assert_eq!(
            Session::new(work).work_dir().root(),
            std::path::Path::new("/home/analyst/.trusty-tools/trusty-audit/work")
        );
    }
}
