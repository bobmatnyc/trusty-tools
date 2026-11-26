//! trusty-architect's local install.toml reader agrees with trusty-common's
//! resolver (#11821 P4-f2, Architect ruling D1af(1) option A).
//!
//! Why: trusty-architect is a leaf and may not depend on trusty-common, so it
//! reads `install.toml` itself. Two readers of one file drift unless a test
//! holds them together; a drift would put the poller on a different
//! claude-config dir than the one tm writes, or let one side fall back to
//! `$HOME` where the other fails closed.
//! What: for each key (`config_dir`, `projects_root`) and each fixture (env
//! unset, set, blank, `~`, relative; install.toml absent, valid, broken,
//! unknown key, unreadable, relative, empty; template set, `~`, relative),
//! runs `trusty_architect::install_paths` and
//! `trusty_common::install_paths::InstallPaths::resolve_with` over the same
//! scratch home and env, and compares the path and source, or the refusal's
//! kind, key and origin. trusty-common resolves all four keys at once; the
//! other three keys are pinned by env so they never read the file, which gives
//! its one-key answer for the key under test.
//! Test: `architect_reader_matches_trusty_common_resolver`,
//! `parity_check_fails_on_a_skewed_reader`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use trusty_architect::install_paths as local;
use trusty_common::install_paths as common;

/// The key under test.
#[derive(Clone, Copy, Debug)]
enum Key {
    ConfigDir,
    ProjectsRoot,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Self::ConfigDir => "config_dir",
            Self::ProjectsRoot => "projects_root",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            Self::ConfigDir => "TRUSTY_MPM_CONFIG_DIR",
            Self::ProjectsRoot => "TRUSTY_MPM_WORKSPACE_ROOT",
        }
    }
}

/// The install file's state in one fixture.
#[derive(Clone, Copy, Debug)]
enum File {
    Absent,
    /// The body; `{key}` is replaced by the key under test.
    Body(&'static str),
    /// A directory where the file should be: a read error, not "absent".
    Unreadable,
}

#[derive(Clone, Copy, Debug)]
struct Fixture {
    name: &'static str,
    env: Option<&'static str>,
    file: File,
    template: Option<&'static str>,
}

const fn fx(
    name: &'static str,
    env: Option<&'static str>,
    file: File,
    template: Option<&'static str>,
) -> Fixture {
    Fixture {
        name,
        env,
        file,
        template,
    }
}

const VALID: File = File::Body("{key} = \"/from/file\"\n");
const BROKEN: File = File::Body("{key} = \"/from/file\nnot toml at all\n");

/// The fixture set; every outcome class appears at least once (checked).
const FIXTURES: &[Fixture] = &[
    fx("env unset, file absent", None, File::Absent, None),
    fx(
        "env set, file absent",
        Some("/from/env"),
        File::Absent,
        None,
    ),
    fx("env ~ expanded", Some("~/tilde"), File::Absent, None),
    fx("env bare ~", Some("~"), File::Absent, None),
    fx("env padded", Some("  /padded  "), File::Absent, None),
    fx("env blank, file valid", Some("   "), VALID, None),
    fx("env set, file valid", Some("/from/env"), VALID, None),
    fx("env unset, file valid", None, VALID, None),
    fx("env unset, file broken", None, BROKEN, None),
    fx("env set, file broken", Some("/from/env"), BROKEN, None),
    fx(
        "env unset, unknown key",
        None,
        File::Body("project_root = \"/x\"\n"),
        None,
    ),
    fx(
        "env unset, non-string value",
        None,
        File::Body("{key} = 7\n"),
        None,
    ),
    fx("env unset, file unreadable", None, File::Unreadable, None),
    fx(
        "env set, file unreadable",
        Some("/from/env"),
        File::Unreadable,
        None,
    ),
    fx(
        "env relative root",
        Some("relative/root"),
        File::Absent,
        None,
    ),
    fx(
        "file relative root",
        None,
        File::Body("{key} = \"relative/root\"\n"),
        None,
    ),
    fx("file empty value", None, File::Body("{key} = \"\"\n"), None),
    fx(
        "file ~ is literal",
        None,
        File::Body("{key} = \"~/not-expanded\"\n"),
        None,
    ),
    fx(
        "file comments, quoted key, literal string",
        None,
        File::Body("# moved\n\n\"{key}\" = '/lit/path' # here\n"),
        None,
    ),
    fx(
        "file escapes",
        None,
        File::Body("{key} = \"/a\\\\b/\\u00e9\"\n"),
        None,
    ),
    fx(
        "file names only other keys",
        None,
        File::Body("codex_home = \"/codex\"\nclaude_config_dir = \"rel\"\n"),
        None,
    ),
    fx("template ~", None, File::Absent, Some("~/tpl")),
    fx("template absolute", None, File::Absent, Some(" /tpl ")),
    fx("template blank", None, File::Absent, Some("   ")),
    fx("template relative root", None, File::Absent, Some("tpl")),
    fx("file beats template", None, VALID, Some("/tpl")),
    fx(
        "env beats template",
        Some("/from/env"),
        File::Absent,
        Some("/tpl"),
    ),
    fx("broken file beats template", None, BROKEN, Some("/tpl")),
];

/// One resolver's answer, in a form both crates can produce.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Path {
        path: PathBuf,
        source: String,
    },
    Refused {
        kind: &'static str,
        key: Option<String>,
        origin: Option<String>,
    },
}

impl Outcome {
    fn class(&self) -> String {
        match self {
            Self::Path { source, .. } => source.clone(),
            Self::Refused { kind, .. } => (*kind).to_owned(),
        }
    }
}

/// A scratch home holding `fixture`'s install file for `key`.
fn scratch_home(fixture: &Fixture, key: Key) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let file = common::install_file_path(home.path());
    match fixture.file {
        File::Absent => {}
        File::Body(body) => {
            let tools = file.parent().expect("install file has a parent");
            fs::create_dir_all(tools).expect("tools dir");
            fs::write(&file, body.replace("{key}", key.name())).expect("install file");
        }
        File::Unreadable => fs::create_dir_all(&file).expect("directory as file"),
    }
    home
}

fn local_outcome(fixture: &Fixture, key: Key, home: &Path) -> Outcome {
    let get = |name: &str| (name == key.env_var()).then(|| fixture.env.map(str::to_owned))?;
    let hit = match key {
        Key::ConfigDir => local::config_dir(home, &get),
        Key::ProjectsRoot => local::projects_root(home, &get, fixture.template),
    };
    match hit {
        Ok(r) => Outcome::Path {
            path: r.path,
            source: r.source.to_string(),
        },
        Err(e) => {
            use local::InstallPathsError as E;
            let (kind, key, origin) = match &e {
                E::Empty { key, origin } => ("empty", Some(*key), Some(origin.to_string())),
                E::Relative { key, origin, .. } => {
                    ("relative", Some(*key), Some(origin.to_string()))
                }
                E::Read { .. } => ("read", None, None),
                E::Parse { .. } => ("parse", None, None),
                _ => ("other", None, None),
            };
            Outcome::Refused {
                kind,
                key: key.map(str::to_owned),
                origin,
            }
        }
    }
}

fn common_outcome(fixture: &Fixture, key: Key, home: &Path) -> Outcome {
    // The other three keys are decided by env, so only `key` reads the file.
    let get = |name: &str| {
        if name == key.env_var() {
            fixture.env.map(str::to_owned)
        } else {
            Some(format!("/parity/other/{name}"))
        }
    };
    let overrides = common::PathOverrides {
        projects_root_template: fixture.template.map(str::to_owned),
        ..common::PathOverrides::default()
    };
    match common::InstallPaths::resolve_with(get, home, &overrides) {
        Ok(paths) => {
            let r = match key {
                Key::ConfigDir => paths.config_dir,
                Key::ProjectsRoot => paths.projects_root,
            };
            Outcome::Path {
                path: r.path,
                source: r.source.to_string(),
            }
        }
        Err(e) => {
            use common::InstallPathsError as E;
            let (kind, key, origin) = match &e {
                E::Empty { key, origin } => ("empty", Some(*key), Some(origin.to_string())),
                E::Relative { key, origin, .. } => {
                    ("relative", Some(*key), Some(origin.to_string()))
                }
                E::Read { .. } => ("read", None, None),
                E::Parse { .. } => ("parse", None, None),
                _ => ("other", None, None),
            };
            Outcome::Refused {
                kind,
                key: key.map(str::to_owned),
                origin,
            }
        }
    }
}

/// Every (fixture, key) where `reader` and trusty-common disagree, plus the
/// outcome classes seen.
fn mismatches(reader: &dyn Fn(&Fixture, Key, &Path) -> Outcome) -> (Vec<String>, BTreeSet<String>) {
    let mut bad = Vec::new();
    let mut classes = BTreeSet::new();
    for fixture in FIXTURES {
        for key in [Key::ConfigDir, Key::ProjectsRoot] {
            let home = scratch_home(fixture, key);
            let (mine, theirs) = (
                reader(fixture, key, home.path()),
                common_outcome(fixture, key, home.path()),
            );
            classes.insert(theirs.class());
            if mine != theirs {
                bad.push(format!(
                    "{} / {}: architect {mine:?} != trusty-common {theirs:?}",
                    fixture.name,
                    key.name()
                ));
            }
        }
    }
    (bad, classes)
}

#[test]
fn architect_reader_matches_trusty_common_resolver() {
    let (bad, classes) = mismatches(&local_outcome);
    assert!(bad.is_empty(), "resolvers drifted:\n{}", bad.join("\n"));
    let want = [
        "config-template",
        "default",
        "empty",
        "env",
        "install-file",
        "parse",
        "read",
        "relative",
    ];
    for class in want {
        assert!(
            classes.contains(class),
            "no fixture reaches {class}: {classes:?}"
        );
    }
}

/// The check can fail: a reader that falls back to home on a broken file, as
/// the pre-#11821 poller did, is caught.
#[test]
fn parity_check_fails_on_a_skewed_reader() {
    let skewed = |fixture: &Fixture, key: Key, home: &Path| match local_outcome(fixture, key, home)
    {
        Outcome::Refused {
            kind: "parse" | "read",
            ..
        } => Outcome::Path {
            path: home.to_path_buf(),
            source: "default".to_owned(),
        },
        other => other,
    };
    let (bad, _) = mismatches(&skewed);
    assert!(
        bad.iter().any(|m| m.starts_with("env unset, file broken")),
        "a home fallback went unnoticed: {bad:?}"
    );
}
