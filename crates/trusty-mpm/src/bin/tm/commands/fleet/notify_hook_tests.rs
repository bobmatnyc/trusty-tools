//! Tests for the `tm fleet init` notification-hook step and the `tm fleet
//! status` row (#10885). One test per row of the issue's init-step contract;
//! every test runs under a temp home, so nothing reaches the operator's
//! `~/.trusty-mpm` or `~/.trusty-tools`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::config::{Edit, enable_notification_hook};
use super::launch::PaneState;
use super::notify_hook::{status_check, step};
use super::{InitReport, Probe, Step, init, resolve_dir, user_config_path};
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::trusty_tools_config::managed_claude_config_dir_at;

/// A tmux with no session, so no test reaches a tmux server.
const NO_TMUX: Probe = Probe {
    pane: |_| PaneState::Absent,
    stamp: |_| None,
    claude: |_| None,
};

/// The fix line the Skipped rows print.
const FIX: &str =
    "edit ~/.trusty-mpm/config.toml, then tm doctor --fix --only notification_hook --yes";

/// A scratch home and the default Architect directory under it.
struct Home(tempfile::TempDir);

impl Home {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("scratch home"))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn dir(&self) -> PathBuf {
        resolve_dir(None, self.path()).unwrap()
    }

    fn config_path(&self) -> PathBuf {
        user_config_path(self.path())
    }

    fn write_config(&self, text: &str) {
        let path = self.config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.config_path()).unwrap_or_default()
    }

    fn settings_path(&self) -> PathBuf {
        managed_claude_config_dir_at(self.path()).join("settings.json")
    }

    fn init(&self) -> anyhow::Result<InitReport> {
        init(&self.dir(), self.path(), false, NO_TMUX)
    }
}

/// The steps that mention `[notification_hook]`.
fn hook_steps(report: &InitReport) -> Vec<&Step> {
    report
        .steps
        .iter()
        .filter(|s| s.render_line().contains("[notification_hook]"))
        .collect()
}

/// Installed-looking binary the hook resolver accepts without a PATH lookup.
const EXE: &str = "/usr/local/bin/tm";

/// The step against `home` for its canonical Architect directory, with [`EXE`].
fn run_step(home: &Home) -> Step {
    let dir = home.dir();
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(dir).unwrap();
    step(&dir, home.path(), Some(Path::new(EXE)))
}

/// tm's `Notification` hook commands in the managed settings.
fn notification_commands(home: &Home) -> Vec<String> {
    let text = std::fs::read_to_string(home.settings_path()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["hooks"]["Notification"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|h| h["command"].as_str().map(str::to_owned))
        .collect()
}

/// Row 1: no section — both keys are written, the rest survives, and the hook
/// lands in the managed settings.
#[test]
fn a_first_run_enables_the_hook_and_applies_it() {
    let home = Home::new();
    let original = "# keep me\n[models]\ndefault = \"sonnet\" # pinned\n";
    home.write_config(original);
    let got = run_step(&home);
    let settings = home.settings_path().display().to_string();
    assert!(
        matches!(&got, Step::Changed(t) if t.contains(&settings)),
        "{got:?}"
    );
    let after = home.config();
    assert!(after.starts_with(original), "{after}");
    let cfg = MpmConfig::load(&home.path().join(".trusty-mpm"));
    let inbox = std::fs::canonicalize(home.dir()).unwrap().join("inbox");
    assert!(cfg.notification_hook.enabled);
    assert_eq!(
        cfg.notification_hook.inbox.as_deref(),
        Some(inbox.as_path())
    );
    assert_eq!(notification_commands(&home), vec![format!("{EXE} hook")]);
}

/// Row 2 and acceptance 2: a second run is Unchanged and writes nothing.
#[test]
fn a_second_run_is_unchanged_and_writes_nothing() {
    let home = Home::new();
    run_step(&home);
    let config = std::fs::read(home.config_path()).unwrap();
    let settings = std::fs::read(home.settings_path()).unwrap();
    let got = run_step(&home);
    assert!(
        matches!(&got, Step::Unchanged(t) if !t.contains("not applied")),
        "{got:?}"
    );
    assert_eq!(std::fs::read(home.config_path()).unwrap(), config);
    assert_eq!(std::fs::read(home.settings_path()).unwrap(), settings);
}

/// A full `init` re-run keeps the hook section and reports no change for it.
#[test]
fn a_second_init_reports_the_hook_unchanged() {
    let home = Home::new();
    let dir = std::fs::canonicalize({
        let d = home.dir();
        std::fs::create_dir_all(&d).unwrap();
        d
    })
    .unwrap();
    home.write_config(&format!(
        "[notification_hook]\nenabled = true\ninbox = \"{}\"\n",
        dir.join("inbox").display()
    ));
    let before = home.config();
    home.init().unwrap();
    let report = home.init().unwrap();
    assert!(
        matches!(hook_steps(&report).as_slice(), [Step::Unchanged(_)]),
        "{}",
        report.render()
    );
    assert!(home.config().starts_with(&before), "{}", home.config());
}

/// The status row's four states; none of them touches `complete`'s inputs.
#[test]
fn the_status_row_reports_each_of_the_four_states() {
    let home = Home::new();
    let dir = home.dir();
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(dir).unwrap();
    let exe = Some(Path::new(EXE));
    let row = || status_check(&dir, home.path(), exe);
    assert_eq!((row().ok, row().detail), (false, "off".to_owned()));
    home.write_config("[notification_hook]\nenabled = true\ninbox = \"/srv/other\"\n");
    assert_eq!(row().detail, "points elsewhere: /srv/other");
    let inbox = dir.join("inbox");
    home.write_config(&format!(
        "[notification_hook]\nenabled = true\ninbox = \"{}\"\n",
        inbox.display()
    ));
    let got = row();
    assert!(
        !got.ok && got.detail.starts_with("enabled but not applied"),
        "{got:?}"
    );
    let settings = home.settings_path();
    trusty_mpm::core::standalone::hooks::notification::apply_notification_hook(
        &settings, exe, true,
    )
    .unwrap();
    let got = row();
    assert!(got.ok, "{got:?}");
    assert_eq!(got.detail, format!("on → {}", inbox.display()));
}

#[test]
fn a_missing_section_is_added_and_the_rest_survives() {
    let path = Path::new("/h/.trusty-mpm/config.toml");
    let raw = "# mine\n[supervisor]\nprojects = [\"/a\"] # keep\n";
    let Edit::Changed(text) = enable_notification_hook(raw, Path::new("/a/inbox"), path).unwrap()
    else {
        panic!("expected a change");
    };
    assert!(text.starts_with(raw), "{text}");
    assert!(
        text.contains("[notification_hook]\nenabled = true\ninbox = \"/a/inbox\"\n"),
        "{text}"
    );
}

#[test]
fn a_present_section_is_never_edited() {
    let path = Path::new("/h/.trusty-mpm/config.toml");
    for raw in [
        "[notification_hook]\nenabled = false\n",
        "[notification_hook]\ninbox = \"/x\"\n",
        "notification_hook.enabled = true\n",
    ] {
        assert_eq!(
            enable_notification_hook(raw, Path::new("/a/inbox"), path).unwrap(),
            Edit::Unchanged,
            "{raw}"
        );
    }
}

/// Row 3: an `inbox` that points elsewhere is left as is, and the fix line is printed.
#[test]
fn an_inbox_set_elsewhere_is_skipped_and_the_config_keeps_it() {
    let home = Home::new();
    home.write_config(
        "# mine\n[notification_hook]\nenabled = true\ninbox = \"/srv/other/inbox\"\n",
    );
    let report = home.init().unwrap();
    let steps = hook_steps(&report);
    assert!(
        matches!(steps.as_slice(), [Step::Skipped(t)] if t.contains(FIX) && t.contains("/srv/other/inbox")),
        "{}",
        report.render()
    );
    let cfg = MpmConfig::load(&home.path().join(".trusty-mpm"));
    assert_eq!(
        cfg.notification_hook.inbox.as_deref(),
        Some(Path::new("/srv/other/inbox"))
    );
    assert!(!home.settings_path().exists(), "the settings were written");
    assert!(!report.failed(), "{}", report.render());
}

/// Row 4: a managed settings file the hook writer refuses skips the step, and
/// neither file is written.
#[test]
fn a_malformed_managed_settings_file_is_skipped_and_nothing_is_written() {
    let home = Home::new();
    let settings = home.settings_path();
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, "{not json").unwrap();
    let report = home.init().unwrap();
    let steps = hook_steps(&report);
    assert!(
        matches!(steps.as_slice(), [Step::Skipped(t)]
            if t.contains(FIX) && t.contains(&settings.display().to_string())),
        "{}",
        report.render()
    );
    assert!(
        !home.config().contains("[notification_hook]"),
        "{}",
        home.config()
    );
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{not json");
    assert!(!report.failed(), "{}", report.render());
}

/// Row 5: a `config.toml` that cannot be written fails the step and `init`.
#[test]
fn a_config_write_failure_fails_the_step() {
    let home = Home::new();
    let dir = home.dir();
    std::fs::create_dir_all(&dir).unwrap();
    let canonical = std::fs::canonicalize(&dir).unwrap();
    home.write_config(&format!(
        "[supervisor]\nprojects = [\"{}\"]\n",
        canonical.display()
    ));
    let before = home.config();
    let root = home.path().join(".trusty-mpm");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
    let report = home.init();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = report.unwrap();
    assert!(
        matches!(hook_steps(&report).as_slice(), [Step::Failed(t)] if t.contains("cannot write")),
        "{}",
        report.render()
    );
    assert!(report.failed());
    assert_eq!(home.config(), before);
    assert!(!home.settings_path().exists(), "the settings were written");
}
