//! Tests for the supervisor instruction profile (#8453).

use super::*;
use crate::core::bundle::{DEFAULT_OUTPUT_STYLE_ID, OUTPUT_STYLE, OUTPUT_STYLE_SUPERVISOR};
use crate::core::instruction_overrides::resolve_pm_prompt_with_roster_for;
use crate::core::instruction_overrides::{PromptSource, resolve_pm_prompt_with_roster};
use std::ffi::OsString;
use tempfile::TempDir;

/// A fixed roster, so the PM side of a comparison is machine-independent.
const ROSTER: &str = "## Delegation Authority\n\n### rust-engineer\n\nRust work.";

/// A temp project whose `.trusty-mpm.toml` holds `toml`.
fn project_with(toml: &str) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join(".trusty-mpm.toml"), toml).expect("write config");
    tmp
}

/// A supervisor project.
fn supervisor_project() -> TempDir {
    project_with("profile = \"supervisor\"\n")
}

/// A user-level config that allow-lists exactly `dirs`.
fn allowing(dirs: &[&Path]) -> MpmConfig {
    let mut config = MpmConfig::default();
    config.supervisor.projects = dirs.iter().map(|d| d.to_path_buf()).collect();
    config
}

/// The first heading line of a PM section file.
fn heading(section: &'static str) -> &'static str {
    section
        .lines()
        .find(|line| line.starts_with('#'))
        .expect("the section has a heading")
}

/// Text that must never reach a supervisor session: the opening heading of
/// every PM section #8453 drops, the roster heading, and the output-style
/// delegation directive.
fn dropped_pm_markers() -> Vec<&'static str> {
    let mut markers: Vec<&'static str> = [
        include_str!("../assets/instructions/sections/agent-delegation.md"),
        include_str!("../assets/instructions/sections/agent-routing.md"),
        include_str!("../assets/instructions/sections/delegation-mechanics.md"),
        include_str!("../assets/instructions/sections/pm-allowlist.md"),
        include_str!("../assets/instructions/sections/phases.md"),
        include_str!("../assets/instructions/sections/qa-gate.md"),
        include_str!("../assets/instructions/sections/git-file-tracking.md"),
        include_str!("../assets/instructions/sections/enforcement.md"),
    ]
    .into_iter()
    .map(heading)
    .collect();
    markers.extend([
        "## Delegation Authority",
        "## Routing Table",
        "STRICTLY FORBIDDEN FROM DOING ANY WORK DIRECTLY",
        "PRIMARY DIRECTIVE — MANDATORY DELEGATION",
    ]);
    markers
}

/// The markers `text` contains.
fn leaked<'a>(text: &str, markers: &[&'a str]) -> Vec<&'a str> {
    markers
        .iter()
        .copied()
        .filter(|m| text.contains(m))
        .collect()
}

#[test]
fn a_supervisor_config_selects_the_supervisor_profile() {
    // All conditions met: allow-listed AND the file asks.
    let tmp = supervisor_project();
    assert_eq!(
        resolve(tmp.path(), &allowing(&[tmp.path()])),
        SessionProfile::Supervisor
    );
    let pm = project_with("profile = \"pm\"\n");
    assert_eq!(
        resolve(pm.path(), &allowing(&[pm.path()])),
        SessionProfile::Pm
    );
    let absent = TempDir::new().expect("tempdir");
    assert_eq!(
        resolve(absent.path(), &allowing(&[absent.path()])),
        SessionProfile::Pm
    );
}

#[test]
fn a_project_only_switch_stays_pm() {
    // #3981: the project file alone never exempts a project — not for the
    // profile, the prompt, the style or the model.
    let tmp = supervisor_project();
    let config = MpmConfig::default();
    assert_eq!(resolve(tmp.path(), &config), SessionProfile::Pm);
    assert_eq!(resolve_ambient(tmp.path()), SessionProfile::Pm);
    let (_, source) = resolve_pm_prompt_with_roster(tmp.path(), || Some(ROSTER.into()));
    assert_eq!(source, PromptSource::Package);
    let root = TempDir::new().expect("framework root");
    let style = crate::core::output_style::select_style_under(root.path(), tmp.path(), None);
    assert_eq!(style.style.id(), DEFAULT_OUTPUT_STYLE_ID);
    assert_eq!(
        launch_model(resolve(tmp.path(), &config), &config),
        crate::core::model_inject::resolve_pm_model(&config, None)
    );
}

#[test]
fn an_allowlist_entry_for_another_path_does_not_match() {
    let tmp = supervisor_project();
    let other = TempDir::new().expect("tempdir");
    assert_eq!(
        resolve(tmp.path(), &allowing(&[other.path()])),
        SessionProfile::Pm
    );
    // A relative entry matches nothing, whatever the process cwd is.
    let relative = allowing(&[Path::new(".")]);
    assert!(!is_allow_listed(tmp.path(), &relative.supervisor));
}

#[test]
fn a_symlinked_allowlist_entry_is_canonicalized() {
    let tmp = supervisor_project();
    let links = TempDir::new().expect("tempdir");
    let link = links.path().join("fleet");
    std::os::unix::fs::symlink(tmp.path(), &link).expect("symlink");
    // The entry names the link; the session runs in the real directory.
    assert_eq!(
        resolve(tmp.path(), &allowing(&[&link])),
        SessionProfile::Supervisor
    );
    // The entry names the real directory; the session runs through the link.
    assert_eq!(
        resolve(&link, &allowing(&[tmp.path()])),
        SessionProfile::Supervisor
    );
}

#[test]
fn the_doctor_reason_names_the_missing_allowlist_entry() {
    let tmp = supervisor_project();
    assert_eq!(
        refusal(tmp.path(), &MpmConfig::default()),
        Some(NOT_ALLOW_LISTED)
    );
    assert!(NOT_ALLOW_LISTED.contains("not allow-listed in ~/.trusty-mpm/config.toml"));
    assert_eq!(refusal(tmp.path(), &allowing(&[tmp.path()])), None);
    let pm = TempDir::new().expect("tempdir");
    assert_eq!(refusal(pm.path(), &MpmConfig::default()), None);
}

#[test]
fn an_unknown_profile_value_falls_back_to_the_pm_profile() {
    let cfg = |raw: &str| ProjectLevelConfig {
        profile: Some(raw.to_string()),
        ..ProjectLevelConfig::default()
    };
    assert_eq!(profile_from_config(None), SessionProfile::Pm);
    assert_eq!(
        profile_from_config(Some(&cfg("  Supervisor "))),
        SessionProfile::Supervisor
    );
    for raw in ["", "pm", "supervisr", "overseer"] {
        assert_eq!(
            profile_from_config(Some(&cfg(raw))),
            SessionProfile::Pm,
            "{raw:?}"
        );
    }
}

#[test]
fn a_malformed_config_falls_back_to_the_pm_profile() {
    // Fail-closed: when the selector cannot tell, the session gets the FULL PM
    // prompt — never the supervisor text, never a partial or empty prompt —
    // even for an allow-listed project.
    let clean = TempDir::new().expect("tempdir");
    let (pm_prompt, _) = resolve_pm_prompt_with_roster(clean.path(), || Some(ROSTER.into()));

    let malformed = project_with("profile = \"supervisor\"\nnot toml at all [[[\n");
    let unknown_key = project_with("profile = \"supervisor\"\nprofle = \"x\"\n");
    let unreadable = TempDir::new().expect("tempdir");
    std::fs::create_dir(unreadable.path().join(".trusty-mpm.toml")).expect("a directory");
    for (label, dir) in [
        ("malformed", &malformed),
        ("unknown key", &unknown_key),
        ("unreadable", &unreadable),
    ] {
        let profile = resolve(dir.path(), &allowing(&[dir.path()]));
        assert_eq!(profile, SessionProfile::Pm, "{label}");
        let (prompt, source) =
            resolve_pm_prompt_with_roster_for(dir.path(), profile, || Some(ROSTER.into()));
        assert_eq!(source, PromptSource::Package, "{label}");
        assert_eq!(prompt, pm_prompt, "{label}: the full PM prompt");
    }
}

#[test]
fn the_supervisor_prompt_is_never_empty() {
    let prompt = supervisor_prompt();
    assert!(prompt.starts_with("# Trusty Fleet Supervisor"), "{prompt}");
    for (name, body) in SUPERVISOR_SECTIONS {
        assert!(!body.trim().is_empty(), "{name}");
    }
    // Authoring comments are folded out, as in the PM composer.
    assert!(!prompt.contains("<!--"), "{prompt}");
}

#[test]
fn every_kept_item_reaches_the_supervisor_prompt() {
    // The Keep list of #8453, one marker per item.
    let prompt = supervisor_prompt();
    for (item, marker) in [
        ("identity", "You are a direct-action supervisor"),
        ("hard limits", "## Hard Limits"),
        ("relay marker", "`[<Name> supervisor HH:MMZ]`"),
        ("two-step send", "Send with the two-step pattern"),
        ("exact tmux targets", "`=<session>:`"),
        ("evidence labels", "**reported**"),
        ("evidence labels", "**verified**"),
        ("decisions as options", "\"(Recommended)\""),
        ("monitoring", "## Monitoring and Heartbeat"),
        ("heartbeat", "Run exactly one heartbeat"),
        ("trusty tool priority", "## Trusty Tool Priority"),
        ("prose style", "## Prose Style"),
    ] {
        assert!(prompt.contains(marker), "{item}: `{marker}` missing");
    }
}

#[test]
fn no_pm_delegation_text_reaches_a_supervisor_session() {
    // The whole delivered prompt, on both output-style paths, plus the style
    // file a native Claude Code reads.
    let tmp = supervisor_project();
    let markers = dropped_pm_markers();
    for native in [true, false] {
        let delivered = crate::core::session_launch::build_system_prompt_for_profile(
            tmp.path(),
            None,
            native,
            SessionProfile::Supervisor,
        );
        assert!(
            delivered.contains("# Trusty Fleet Supervisor"),
            "{delivered}"
        );
        assert_eq!(
            leaked(&delivered, &markers),
            Vec::<&str>::new(),
            "native={native}"
        );
    }
    assert_eq!(
        leaked(OUTPUT_STYLE_SUPERVISOR, &markers),
        Vec::<&str>::new()
    );

    // The markers are real: every one of them reaches a PM session.
    let pm = TempDir::new().expect("tempdir");
    let (pm_prompt, _) = resolve_pm_prompt_with_roster(pm.path(), || Some(ROSTER.into()));
    let pm_session = format!("{OUTPUT_STYLE}\n{pm_prompt}");
    assert_eq!(leaked(&pm_session, &markers), markers);
}

#[test]
fn a_supervisor_project_needs_no_claude_md_override_blocks() {
    // A plain CLAUDE.md is enough to act directly, and a leftover PM override
    // block does not reach the supervisor prompt.
    let tmp = supervisor_project();
    std::fs::write(
        tmp.path().join("CLAUDE.md"),
        "# Fleet\n\nWatch set: tm-api.\n",
    )
    .unwrap();
    let supervisor = SessionProfile::Supervisor;
    let (plain, source) = resolve_pm_prompt_with_roster_for(tmp.path(), supervisor, || None);
    assert_eq!(source, PromptSource::Supervisor);
    assert_eq!(plain, supervisor_prompt());

    std::fs::write(
        tmp.path().join("CLAUDE.md"),
        "<!-- TRUSTY-MPM: IDENTITY START v=1 -->\nLEFTOVER-IDENTITY-OVERRIDE\n\
         <!-- TRUSTY-MPM: IDENTITY END -->\n",
    )
    .unwrap();
    let (with_block, _) = resolve_pm_prompt_with_roster_for(tmp.path(), supervisor, || None);
    assert_eq!(with_block, supervisor_prompt());
}

#[test]
fn a_supervisor_project_selects_the_supervisor_style_over_every_tier() {
    let tmp = supervisor_project();
    let config = MpmConfig::default();
    let selected = crate::core::output_style::select_style(
        tmp.path(),
        Some("trusty-mpm-teacher"),
        &config,
        || Some("trusty-mpm-research".into()),
        SessionProfile::Supervisor,
    );
    assert_eq!(selected.style.id(), SUPERVISOR_OUTPUT_STYLE_ID);
    assert!(selected.warning.is_none());
}

#[test]
fn a_pm_project_cannot_select_the_supervisor_style() {
    let tmp = TempDir::new().expect("tempdir");
    let selected = crate::core::output_style::select_style(
        tmp.path(),
        Some(SUPERVISOR_OUTPUT_STYLE_ID),
        &MpmConfig::default(),
        || None,
        SessionProfile::Pm,
    );
    assert_eq!(selected.style.id(), DEFAULT_OUTPUT_STYLE_ID);
    let warning = selected.warning.expect("a warning");
    assert!(warning.contains("supervisor profile only"), "{warning}");
}

#[test]
fn the_supervisor_style_carries_write_plainly_verbatim() {
    let section = |doc: &str| {
        let start = doc
            .find("## Communication — Write Plainly")
            .expect("the section");
        let rest = &doc[start..];
        let end = rest[1..].find("\n## ").map_or(rest.len(), |at| at + 1);
        rest[..end].trim().to_string()
    };
    assert_eq!(section(OUTPUT_STYLE_SUPERVISOR), section(OUTPUT_STYLE));
    assert!(OUTPUT_STYLE_SUPERVISOR.contains(&format!("\nname: {SUPERVISOR_OUTPUT_STYLE_ID}\n")));
}

#[test]
fn the_supervisor_model_is_the_opus_tier_alias() {
    // A pinned id (`claude-opus-4-5`, a dated id, any id with a version
    // number) fails here.
    assert_eq!(SUPERVISOR_MODEL, "opus");
    assert!(!SUPERVISOR_MODEL.starts_with("claude-"));
    assert!(!SUPERVISOR_MODEL.chars().any(|c| c.is_ascii_digit()));

    // Operator pins on the PM chain do not reach the supervisor.
    let mut config = MpmConfig::default();
    config.models.default = Some("claude-sonnet-4-5".into());
    config.models.tiers.opus = Some("claude-opus-4-1".into());
    assert_eq!(
        launch_model(SessionProfile::Supervisor, &config),
        SUPERVISOR_MODEL
    );
    assert_eq!(
        launch_model(SessionProfile::Pm, &config),
        crate::core::model_inject::resolve_pm_model(&config, None)
    );
}

#[test]
fn the_hook_reads_only_the_launch_directory() {
    // #8453 row 4: an unset or empty `CLAUDE_PROJECT_DIR` is the PM profile;
    // the payload `cwd` follows `cd` and is never consulted.
    assert_eq!(
        hook_project_dir(Some("/work/supervisor".into())),
        Some(PathBuf::from("/work/supervisor"))
    );
    assert_eq!(hook_project_dir(Some("".into())), None);
    assert_eq!(hook_project_dir(None), None);
}

#[test]
fn the_hook_needs_the_stamp_the_allowlist_and_the_file() {
    let supervisor = supervisor_project();
    let pm = project_with("profile = \"pm\"\n");
    let missing = TempDir::new().expect("tempdir");
    let stamp = || Some(OsString::from(SUPERVISOR_PROFILE_ID));
    let dir = |d: &TempDir| Some(d.path().as_os_str().to_owned());
    let all = |d: &TempDir| allowing(&[d.path()]);
    let cases = [
        (
            "all three",
            stamp(),
            dir(&supervisor),
            all(&supervisor),
            true,
        ),
        ("no stamp", None, dir(&supervisor), all(&supervisor), false),
        (
            "pm stamp",
            Some("pm".into()),
            dir(&supervisor),
            all(&supervisor),
            false,
        ),
        (
            "no allowlist",
            stamp(),
            dir(&supervisor),
            MpmConfig::default(),
            false,
        ),
        ("file says pm", stamp(), dir(&pm), all(&pm), false),
        ("file missing", stamp(), dir(&missing), all(&missing), false),
        ("no project dir", stamp(), None, all(&supervisor), false),
    ];
    for (label, stamp, dir, config, supervisor) in cases {
        let got = hook_profile(stamp, dir, || config);
        assert_eq!(got.is_supervisor(), supervisor, "{label}");
    }
    // A PM stamp never reads the config.
    let got = hook_profile(None, dir(&supervisor), || panic!("config read for a PM"));
    assert_eq!(got, SessionProfile::Pm);
}

#[test]
fn the_launch_stamp_names_the_profile() {
    assert_eq!(
        launch_env(SessionProfile::Supervisor),
        (SESSION_PROFILE_ENV.to_owned(), "supervisor".to_owned())
    );
    assert_eq!(launch_env(SessionProfile::Pm).1, "pm");
}

#[test]
fn cli_launch_stamps_the_profile_its_prompt_was_composed_for() {
    // A temp project is on no operator's allowlist: a PM prompt and a PM stamp.
    let tmp = supervisor_project();
    let cli = crate::core::session_launch::cli_launch(tmp.path(), None);
    assert_eq!(cli.profile, SessionProfile::Pm);
    assert!(
        cli.env.contains(&launch_env(SessionProfile::Pm)),
        "{:?}",
        cli.env
    );
    assert!(!cli.prompt.contains("# Trusty Fleet Supervisor"));
}
