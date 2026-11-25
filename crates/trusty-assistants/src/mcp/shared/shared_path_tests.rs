//! Tests for `shared_path_from` (#11821).

use trusty_common::install_paths::{InstallPaths, PathOverrides, install_file_path};

use super::shared_path_from;

/// #11821: the shared MCP file follows a configured config dir.
#[test]
fn shared_path_from_follows_the_config_dir() {
    let home = tempfile::tempdir().expect("tempdir");
    let paths = InstallPaths::resolve_with(
        |name| (name == "TRUSTY_MPM_CONFIG_DIR").then(|| "/srv/cfg".to_string()),
        home.path(),
        &PathOverrides::default(),
    );
    assert_eq!(
        shared_path_from(paths).expect("resolved"),
        std::path::PathBuf::from("/srv/cfg/.trusty-tools/mcp/servers.toml")
    );
}

/// #11821: a broken `install.toml` is an error, never the `$HOME` file.
#[test]
fn shared_path_from_fails_closed_on_broken_install_file() {
    let home = tempfile::tempdir().expect("tempdir");
    let file = install_file_path(home.path());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "bogus = 1\n").unwrap();
    let paths = InstallPaths::resolve_with(|_| None, home.path(), &PathOverrides::default());
    let err = shared_path_from(paths).expect_err("a broken install.toml must not resolve");
    assert!(err.to_string().contains("install.toml"), "{err}");
}
