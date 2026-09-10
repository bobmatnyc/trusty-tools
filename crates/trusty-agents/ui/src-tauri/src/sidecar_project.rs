//! Why: installed builds must reopen the same state after worktree removal.
//! What: explicit root overrides a persisted desktop selection; fresh installs use app data.
//! Test: `explicit_root_survives_restart`, `invalid_selection_never_falls_back`.
use std::path::{Path, PathBuf};

/// Why: Finder restarts lose environment overrides.
/// What: validate and atomically persist the chosen root without consulting build paths.
/// Test: `explicit_root_survives_restart`, `invalid_selection_never_falls_back`.
pub(crate) fn resolve_project_root(
    explicit: Option<PathBuf>,
    legacy: Option<PathBuf>,
    settings: &Path,
    default_root: &Path,
) -> Result<PathBuf, String> {
    let override_root = explicit.or(legacy);
    let path = if let Some(path) = &override_root {
        path.clone()
    } else {
        match std::fs::read(settings) {
            Ok(bytes) => serde_json::from_slice::<PathBuf>(&bytes).map_err(|e| {
                format!(
                    "Invalid desktop project selection {}: {e}",
                    settings.display()
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(default_root)
                    .map_err(|e| format!("Cannot create desktop state directory: {e}"))?;
                default_root.to_path_buf()
            }
            Err(e) => return Err(format!("Cannot read desktop project selection: {e}")),
        }
    };
    if !path.is_absolute() || !path.is_dir() {
        return Err(format!(
            "Desktop project root must be an existing absolute directory: {}",
            path.display()
        ));
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Cannot resolve desktop project root: {e}"))?;
    std::fs::read_dir(&canonical).map_err(|e| format!("Cannot read desktop project root: {e}"))?;
    if override_root.is_some() || !settings.exists() {
        let parent = settings
            .parent()
            .ok_or("Desktop settings path has no parent")?;
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create desktop settings directory: {e}"))?;
        let bytes = serde_json::to_vec(&canonical).map_err(|e| e.to_string())?;
        let temporary = settings.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&temporary, bytes)
            .map_err(|e| format!("Cannot save desktop project selection: {e}"))?;
        std::fs::rename(&temporary, settings)
            .map_err(|e| format!("Cannot commit desktop project selection: {e}"))?;
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("trusty-ui-root-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&path).expect("fixture directory");
        path
    }
    #[test]
    fn explicit_root_survives_restart() {
        let base = fixture("restart");
        let selected = base.join("existing-state");
        std::fs::create_dir_all(&selected).expect("state directory");
        let settings = base.join("selection.json");
        let expected = selected.canonicalize().expect("canonical state");
        assert_eq!(
            resolve_project_root(
                Some(selected),
                Some(base.join("ignored")),
                &settings,
                &base.join("default")
            )
            .expect("override"),
            expected
        );
        assert_eq!(
            resolve_project_root(None, None, &settings, &base.join("other-build"))
                .expect("restart"),
            expected
        );
        std::fs::remove_dir_all(base).expect("fixture cleanup");
    }
    #[test]
    fn invalid_selection_never_falls_back() {
        let base = fixture("invalid");
        let settings = base.join("selection.json");
        std::fs::write(&settings, b"broken json").expect("invalid selection");
        assert!(resolve_project_root(None, None, &settings, &base.join("default")).is_err());
        assert!(resolve_project_root(
            Some(PathBuf::from("relative")),
            None,
            &settings,
            &base.join("default")
        )
        .is_err());
        assert!(!base.join("default").exists());
        std::fs::remove_dir_all(base).expect("fixture cleanup");
    }
}
