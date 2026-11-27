//! The binary path the native-host launchers and the launchd plist record (#11927).
//!
//! Why: `install-service` run from a debug build recorded
//! `.../cargo-target-pool/.../debug/trusty-browser`; the next `cargo clean` or
//! pool recycle removed it, and Chrome and launchd both lost the binary.
//! What: [`is_build_path`] names a path under a cargo target directory;
//! [`registered_exe`] keeps a stable current exe, else picks an installed
//! `cargo install` binary, else refuses with the rejected path and the
//! install command. Every writer in [`crate::cli`] takes its exe from here.
//! Test: `a_build_dir_exe_is_never_registered` in `tests/install.rs`.

use std::path::{Component, Path, PathBuf};

/// The binary name `cargo install` places in `<root>/bin`.
pub const BIN: &str = "trusty-browser";

/// The repair step every refusal names.
pub const INSTALL_HINT: &str = "Install a stable binary with `cargo install --locked --path crates/trusty-browser` \
     from a trusty-tools checkout, then run `trusty-browser install-service`.";

/// The environment [`registered_exe`] reads; tests inject every field.
#[derive(Debug, Clone, Default)]
pub struct ExeEnv {
    /// `$HOME`.
    pub home: PathBuf,
    /// `$CARGO_HOME`, when set.
    pub cargo_home: Option<PathBuf>,
    /// `$CARGO_INSTALL_ROOT`, when set.
    pub cargo_install_root: Option<PathBuf>,
    /// `$CARGO_TARGET_DIR`, when set.
    pub cargo_target_dir: Option<PathBuf>,
}

impl ExeEnv {
    /// The process environment, with `home` already resolved by the caller.
    pub fn from_process(home: PathBuf) -> ExeEnv {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        ExeEnv {
            home,
            cargo_home: var("CARGO_HOME"),
            cargo_install_root: var("CARGO_INSTALL_ROOT"),
            cargo_target_dir: var("CARGO_TARGET_DIR"),
        }
    }

    /// `cargo install` roots in cargo's precedence order, each joined with `bin/trusty-browser`.
    fn candidates(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        roots.extend(self.cargo_install_root.clone());
        roots.extend(self.cargo_home.clone());
        roots.push(self.home.join(".cargo"));
        roots.into_iter().map(|r| r.join("bin").join(BIN)).collect()
    }
}

/// Why: a cargo target directory is rebuilt, cleaned and recycled, so a path
/// in it is not a place to register (#11927).
/// What: true when `path` sits under `target_dir` (raw or canonical) or
/// under a `cargo-target-pool` segment, or when it is laid out as cargo
/// output below a `target` segment, for any profile name:
/// `<profile>/<bin>`, `<profile>/{deps,examples}/<bin>`,
/// `<triple>/<profile>/<bin>`, `<triple>/<profile>/{deps,examples}/<bin>`,
/// where a triple contains `-`. A `debug`/`release` segment elsewhere does
/// not count.
/// Test: `a_build_dir_exe_is_never_registered` in `tests/install.rs`.
pub fn is_build_path(path: &Path, target_dir: Option<&Path>) -> bool {
    if let Some(dir) = target_dir {
        let canon = std::fs::canonicalize(dir).ok();
        if path.starts_with(dir) || canon.is_some_and(|c| path.starts_with(c)) {
            return true;
        }
    }
    let names: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    // #11927 review: the pool holds build output only.
    if names.iter().any(|n| n.contains("cargo-target-pool")) {
        return true;
    }
    let sub = |n: &str| n == "deps" || n == "examples";
    let triple = |n: &str| n.contains('-');
    // #11927 review: match cargo's layout below each `target` root, not any later segment.
    names.iter().enumerate().any(|(i, n)| {
        let rel: Vec<&str> = names[i + 1..].iter().map(String::as_str).collect();
        n == "target"
            && match rel.as_slice() {
                [_profile, _bin] => true,
                [a, b, _bin] => sub(b) || triple(a),
                [t, _profile, s, _bin] => triple(t) && sub(s),
                _ => false,
            }
    })
}

/// Why: `uninstall-service` records nothing, so it may match the registration
/// against every binary it could have been installed from (#11927 review).
/// What: `current` first, then the binary [`registered_exe`] substitutes, when
/// it differs.
/// Test: `uninstall_from_a_build_binary_matches_its_own_registration`,
/// `uninstall_service_takes_over_a_registration_whose_binary_is_gone` in `tests/install.rs`.
pub fn uninstall_exes(current: &Path, env: &ExeEnv) -> Vec<PathBuf> {
    let mut out = vec![current.to_path_buf()];
    if let Ok(exe) = registered_exe(current, env)
        && exe != current
    {
        out.push(exe);
    }
    out
}

/// Why: every writer of a launcher, manifest or plist must record a binary
/// that outlives the build that ran the install (#11927).
/// What: returns `current` when it is not a build path; otherwise the first
/// existing `cargo install` binary whose canonical path is not a build path
/// either; otherwise an error naming `current` and [`INSTALL_HINT`].
/// Test: `a_build_dir_exe_is_never_registered` in `tests/install.rs`.
pub fn registered_exe(current: &Path, env: &ExeEnv) -> Result<PathBuf, String> {
    let target = env.cargo_target_dir.as_deref();
    if !is_build_path(current, target) {
        return Ok(current.to_path_buf());
    }
    for candidate in env.candidates() {
        let Ok(real) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if real.is_file() && !is_build_path(&real, target) {
            return Ok(real);
        }
    }
    Err(format!(
        "refusing to register the build binary {}: a cargo target directory is cleaned and rebuilt. {INSTALL_HINT}",
        current.display()
    ))
}
