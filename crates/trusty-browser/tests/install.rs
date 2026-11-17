//! Phase 4 parity: embedded extension, published tree, host registration, launchd job.
//!
//! Every path is under a temp directory; `launchctl` is a recording fake.

mod common;

use std::cell::RefCell;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use sha2::{Digest, Sha256};
use trusty_browser::assets::{self, EXTENSION_ID};
use trusty_browser::install_host;
use trusty_browser::launchd::{self, Launchctl, Layout, Outcome};

const PY_SIGNATURE: &str = "8189a4cbd58c1314d0a5d15ecf7c3bc40add49ce16adeb21ef7690711449b4e8";
/// The state path of the Python capture (`tests/fixtures/install/*`).
const PY_STATE: &str = "/private/tmp/p4-cap/home/.trusty/browser";
const PY_EXE: &str = "/tmp/tb-venv-p1/bin/python3";
const PY_PKG: &str = "/private/tmp/tb-venv-p1/lib/python3.12/site-packages/trusty_browser";

fn fixture(name: &str) -> String {
    String::from_utf8(common::read(&format!("install/{name}"))).expect("utf8")
}

/// The same listing as `capture/tree.py`: path, kind, mode, sha256 or link target.
fn tree(root: &Path) -> String {
    fn walk(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(root.join(rel)).expect("read_dir") {
            let rel = rel.join(e.expect("entry").file_name());
            let meta = root.join(&rel).symlink_metadata().expect("lstat");
            out.push(rel.clone());
            if meta.is_dir() {
                walk(root, &rel, out);
            }
        }
    }
    let mut paths = Vec::new();
    walk(root, Path::new(""), &mut paths);
    paths.sort();
    let mut out = String::new();
    for rel in paths {
        let p = root.join(&rel);
        let m = p.symlink_metadata().expect("lstat");
        let mode = format!("0o{:o}", m.mode() & 0o7777);
        let (kind, tail) = if m.is_symlink() {
            (
                "link",
                fs::read_link(&p).expect("link").display().to_string(),
            )
        } else if m.is_dir() {
            ("dir", "-".to_owned())
        } else {
            (
                "file",
                hex::encode(Sha256::digest(fs::read(&p).expect("read"))),
            )
        };
        out.push_str(&format!("{}\t{kind}\t{mode}\t{tail}\n", rel.display()));
    }
    out
}

#[test]
fn embedded_stamp_matches_python() {
    let files = assets::embedded();
    assert_eq!(assets::signature(&files), PY_SIGNATURE);
    assert_eq!(assets::stamp(&files), &PY_SIGNATURE[..16]);
}

#[test]
fn embedded_table_is_the_whole_extension_dir() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("extension");
    assert_eq!(assets::read_dir(&dir).expect("read"), assets::embedded());
}

#[test]
fn extension_id_derives_from_the_manifest_key() {
    let manifest =
        trusty_browser::pyjson::Json::parse(&assets::embedded()[Path::new("manifest.json")])
            .expect("manifest");
    let key = manifest.get("key").and_then(|k| k.as_str()).expect("key");
    let der = base64::engine::general_purpose::STANDARD
        .decode(key)
        .expect("b64");
    let id: String = hex::encode(&Sha256::digest(der)[..16])
        .chars()
        .map(|c| (b'a' + c.to_digit(16).expect("hex") as u8) as char)
        .collect();
    assert_eq!(id, EXTENSION_ID);
    assert_eq!(
        trusty_browser::peer_darwin::ORIGIN,
        format!("chrome-extension://{EXTENSION_ID}/")
    );
}

#[test]
fn fresh_publish_matches_python_tree() {
    let dir = common::state_dir();
    let state = dir.path().join("state");
    let path = assets::extension_path(&state).expect("publish");
    assert_eq!(path, state.join("extension"));
    assert_eq!(tree(&state), fixture("assets_fresh.tsv"));
    assert_eq!(
        assets::published(&state).as_deref(),
        Some(&PY_SIGNATURE[..16])
    );
}

#[test]
fn republish_migrates_the_symlinked_layout_like_python() {
    let dir = common::state_dir();
    let state = dir.path().join("state");
    let (a, b) = ("a".repeat(64), "b".repeat(64));
    fs::create_dir_all(state.join("assets").join(&a).join("icons")).expect("mkdir");
    fs::create_dir_all(state.join("assets").join(&b)).expect("mkdir");
    for p in [&state, &state.join("assets")] {
        fs::set_permissions(p, fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    fs::write(state.join("assets").join(&a).join("stale.js"), "old\n").expect("w");
    fs::write(state.join("assets").join(&a).join("worker.js"), "old\n").expect("w");
    fs::write(state.join("assets").join(&b).join("manifest.json"), "x\n").expect("w");
    std::os::unix::fs::symlink(Path::new("assets").join(&a), state.join("extension"))
        .expect("link");
    assets::extension_path(&state).expect("publish");
    assert_eq!(tree(&state), fixture("assets_migrated.tsv"));
}

#[test]
fn republish_prunes_stale_files_and_restores_changed_ones() {
    let dir = common::state_dir();
    let state = dir.path().join("state");
    assets::extension_path(&state).expect("publish");
    let ext = state.join("extension");
    fs::write(ext.join("worker.js"), "tampered").expect("w");
    fs::write(ext.join(".DS_Store"), "x").expect("w");
    fs::create_dir_all(ext.join("old/deeper")).expect("mkdir");
    fs::write(ext.join("old/deeper/gone.js"), "x").expect("w");
    assets::extension_path(&state).expect("republish");
    assert_eq!(tree(&state), fixture("assets_fresh.tsv"));
}

#[test]
fn publish_refuses_an_unrelated_extension_path() {
    let dir = common::state_dir();
    let state = dir.path().join("state");
    fs::create_dir_all(&state).expect("mkdir");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).expect("chmod");
    std::os::unix::fs::symlink("/tmp", state.join("extension")).expect("link");
    let err = assets::extension_path(&state).expect_err("refused");
    assert!(
        err.to_string()
            .starts_with("Refusing to replace an unrelated path: "),
        "{err}"
    );
}

#[test]
fn registration_files_match_python_layout() {
    let state = Path::new(PY_STATE);
    let manifests = Path::new("/m");
    let exe = Path::new("/opt/bin/trusty-browser");
    let files = install_host::registration_files(state, manifests, exe);
    let names: Vec<_> = files.iter().map(|r| (r.path.clone(), r.mode)).collect();
    assert_eq!(
        names,
        vec![
            (state.join("chrome-native-host"), 0o700),
            (manifests.join("com.trusty.browser.json"), 0o600),
            (state.join("chrome-native-host.enroll"), 0o700),
            (manifests.join("com.trusty.browser.enroll.json"), 0o600),
        ]
    );
    // The manifests are byte-identical to Python's; they name the same launchers.
    assert_eq!(files[1].content, fixture("com.trusty.browser.json"));
    assert_eq!(files[3].content, fixture("com.trusty.browser.enroll.json"));
    // Python's launcher format, reproduced for Python's argv, then the Rust exec line.
    for (suffix, sock, idx) in [("", "service.sock", 0), (".enroll", "enrollment.sock", 2)] {
        let sock = format!("{PY_STATE}/{sock}");
        let py = install_host::launcher(&[
            PY_EXE,
            &format!("{PY_PKG}/native_host.py"),
            "--socket",
            &sock,
        ]);
        assert_eq!(py, fixture(&format!("python.chrome-native-host{suffix}")));
        assert_eq!(
            files[idx].content,
            format!("#!/bin/sh\nexec /opt/bin/trusty-browser native-host --socket {sock} \"$@\"\n")
        );
    }
    assert_eq!(install_host::shquote("a b'c"), "'a b'\"'\"'c'");
    assert_eq!(install_host::shquote(""), "''");
}

#[test]
fn python_plist_renders_byte_identically() {
    let state = Path::new(PY_STATE);
    let service = format!("{PY_PKG}/service.py");
    let text =
        launchd::plist(&[PY_EXE, &service, "--state", PY_STATE, "serve"], state).expect("plist");
    assert_eq!(text, fixture("python.plist"));
    let rust =
        launchd::launch_configuration(Path::new("/opt/bin/trusty-browser"), state).expect("plist");
    assert!(rust.contains(
        "\t<array>\n\t\t<string>/opt/bin/trusty-browser</string>\n\t\t<string>--state</string>\n"
    ));
    assert_eq!(rust.lines().count(), text.lines().count() - 1);
    assert_eq!(
        launchd::plist(&["a&<>"], state)
            .expect("plist")
            .matches("a&amp;&lt;&gt;")
            .count(),
        1
    );
}

#[test]
fn install_host_is_idempotent_and_refuses_a_foreign_file() {
    let dir = common::state_dir();
    let state = dir.path().join("state");
    let manifests = dir.path().join("hosts");
    let exe = Path::new("/opt/bin/trusty-browser");
    let path = install_host::install(&state, &manifests, exe).expect("install");
    assert_eq!(path, manifests.join("com.trusty.browser.json"));
    install_host::install(&state, &manifests, exe).expect("idempotent");
    let launcher = state.join("chrome-native-host");
    assert_eq!(fs::metadata(&launcher).expect("meta").mode() & 0o777, 0o700);
    fs::write(&launcher, "#!/bin/sh\nexec /evil\n").expect("w");
    let err = install_host::install(&state, &manifests, exe).expect_err("refused");
    assert_eq!(
        err.to_string(),
        format!("existing file differs: {}", launcher.display())
    );
}

/// Records every call; `print` answers with the configured outcome.
struct FakeCtl {
    print: Outcome,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeCtl {
    fn new(code: i32, stdout: &str) -> FakeCtl {
        FakeCtl {
            print: Outcome {
                code,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            },
            calls: RefCell::new(Vec::new()),
        }
    }
    fn verbs(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|c| c[0].clone()).collect()
    }
}

impl Launchctl for FakeCtl {
    fn run(&self, args: &[&str]) -> io::Result<Outcome> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        Ok(if args[0] == "print" {
            self.print.clone()
        } else {
            Outcome::default()
        })
    }
}

fn layout(home: &Path) -> Layout {
    Layout {
        home: home.to_path_buf(),
        state: home.join(".trusty/browser"),
        exe: PathBuf::from("/opt/bin/trusty-browser"),
        uid: 501,
    }
}

fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents/com.trusty.browser.plist")
}

#[cfg(target_os = "macos")]
#[test]
fn install_service_writes_files_and_bootstraps() {
    let dir = common::state_dir();
    let l = layout(dir.path());
    let ctl = FakeCtl::new(113, "");
    launchd::manage(&l, true, &ctl).expect("install");
    let plist = plist_path(dir.path());
    assert_eq!(
        *ctl.calls.borrow(),
        vec![
            vec!["print".to_owned(), "gui/501/com.trusty.browser".to_owned()],
            vec![
                "bootstrap".to_owned(),
                "gui/501".to_owned(),
                plist.display().to_string()
            ],
        ]
    );
    assert_eq!(
        fs::read_to_string(&plist).expect("plist"),
        launchd::launch_configuration(&l.exe, &l.state).expect("render")
    );
    assert_eq!(fs::metadata(&plist).expect("m").mode() & 0o777, 0o600);
    assert_eq!(
        assets::published(&l.state).as_deref(),
        Some(&PY_SIGNATURE[..16])
    );
    let hosts = install_host::default_directory(dir.path()).expect("dir");
    assert!(hosts.join("com.trusty.browser.enroll.json").is_file());
    // Loaded with our plist: a second install is a no-op that does not bootstrap.
    let ctl = FakeCtl::new(0, &format!("\tpath = {}\n", plist.display()));
    launchd::manage(&l, true, &ctl).expect("reinstall");
    assert_eq!(ctl.verbs(), vec!["print"]);
}

#[cfg(target_os = "macos")]
#[test]
fn install_service_refuses_a_job_owned_elsewhere() {
    let dir = common::state_dir();
    let l = layout(dir.path());
    let err =
        launchd::manage(&l, true, &FakeCtl::new(0, "path = /elsewhere\n")).expect_err("refused");
    assert_eq!(
        err.to_string(),
        "A different launchd job owns com.trusty.browser"
    );
    let plist = plist_path(dir.path());
    fs::create_dir_all(plist.parent().expect("p")).expect("mkdir");
    fs::write(&plist, "foreign").expect("w");
    fs::set_permissions(&plist, fs::Permissions::from_mode(0o600)).expect("chmod");
    let err = launchd::manage(&l, false, &FakeCtl::new(113, "")).expect_err("refused");
    assert_eq!(
        err.to_string(),
        format!(
            "Refusing to change an unrelated registration: {}",
            plist.display()
        )
    );
}

/// Write the files Python 0.7.0 `install-service` writes, for `l.state`.
#[cfg(target_os = "macos")]
fn python_registration(l: &Layout) {
    let st = l.state.to_string_lossy().into_owned();
    fs::create_dir_all(&l.state).expect("mkdir");
    fs::set_permissions(&l.state, fs::Permissions::from_mode(0o700)).expect("chmod");
    let hosts = install_host::default_directory(&l.home).expect("dir");
    fs::create_dir_all(&hosts).expect("mkdir");
    for r in install_host::registration_files(&l.state, &hosts, &l.exe) {
        let content = match r.path.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.starts_with("chrome-native-host") => {
                let sock = if n.ends_with(".enroll") {
                    "enrollment.sock"
                } else {
                    "service.sock"
                };
                let sock = format!("{st}/{sock}");
                install_host::launcher(&[
                    PY_EXE,
                    &format!("{PY_PKG}/native_host.py"),
                    "--socket",
                    &sock,
                ])
            }
            _ => r.content.clone(),
        };
        install_host::write_owned(&r.path, &content, r.mode).expect("write");
    }
    let plist = plist_path(&l.home);
    fs::create_dir_all(plist.parent().expect("p")).expect("mkdir");
    let service = format!("{PY_PKG}/service.py");
    let text =
        launchd::plist(&[PY_EXE, &service, "--state", &st, "serve"], &l.state).expect("plist");
    install_host::write_owned(&plist, &text, 0o600).expect("plist");
}

#[cfg(target_os = "macos")]
#[test]
fn uninstall_service_replaces_a_python_registration() {
    let dir = common::state_dir();
    let l = layout(dir.path());
    python_registration(&l);
    let plist = plist_path(dir.path());
    let loaded = format!("\tpath = {}\n", plist.display());
    let ctl = FakeCtl::new(0, &loaded);
    launchd::manage(&l, false, &ctl).expect("uninstall");
    assert_eq!(ctl.verbs(), vec!["print", "bootout"]);
    assert!(!plist.exists());
    assert!(!l.state.join("chrome-native-host").exists());
    assert!(!l.state.join("chrome-native-host.enroll").exists());
    // Install straight over a Python registration replaces every file.
    python_registration(&l);
    launchd::manage(&l, true, &FakeCtl::new(113, "")).expect("install");
    for r in install_host::registration_files(
        &l.state,
        &install_host::default_directory(&l.home).expect("d"),
        &l.exe,
    ) {
        assert_eq!(fs::read_to_string(&r.path).expect("read"), r.content);
    }
    assert_eq!(
        fs::read_to_string(&plist).expect("plist"),
        launchd::launch_configuration(&l.exe, &l.state).expect("render")
    );
}

/// A loaded Python broker is restarted on our plist, not left running.
#[cfg(target_os = "macos")]
#[test]
fn install_over_a_loaded_python_job_restarts_it() {
    let dir = common::state_dir();
    let l = layout(dir.path());
    python_registration(&l);
    let plist = plist_path(dir.path());
    let ctl = FakeCtl::new(0, &format!("\tpath = {}\n", plist.display()));
    launchd::manage(&l, true, &ctl).expect("install");
    assert_eq!(ctl.verbs(), vec!["print", "bootout", "bootstrap"]);
    assert_eq!(
        ctl.calls.borrow()[2],
        vec![
            "bootstrap".to_owned(),
            "gui/501".to_owned(),
            plist.display().to_string()
        ]
    );
    assert_eq!(
        fs::read_to_string(&plist).expect("plist"),
        launchd::launch_configuration(&l.exe, &l.state).expect("render")
    );
}
