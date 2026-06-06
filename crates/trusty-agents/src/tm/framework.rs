//! Framework / language detection from a project root directory.
//!
//! Why: TM needs to label each project with its primary language and
//! framework so the orchestrator can pick appropriate agents and the UI can
//! show meaningful status. We deliberately avoid spawning subprocesses
//! (`cargo metadata`, `npm ls`, etc.) so detection is cheap, sandbox-safe,
//! and works on uncloned/stale checkouts.
//! What: Inspects the existence and contents of well-known marker files
//! (Cargo.toml, package.json, pyproject.toml, …) and returns a
//! `DetectedFramework` summarizing the language, framework, and package
//! manager.
//! Test: See `#[cfg(test)] mod tests` at the bottom of this file —
//! covers Rust (plain + axum), TS/Next.js, SvelteKit/pnpm, Python, Go, and
//! the unknown case.

use super::project::DetectedFramework;
use chrono::Utc;
use std::path::Path;

/// Detect language/framework by inspecting files in the project root.
///
/// Why: Single entry point so callers don't have to know the priority order.
/// Never spawns subprocesses — purely file-existence + content checks.
/// What: Tries detectors in priority order (Rust → Go → Java → Python →
/// JavaScript) and returns the first match, or `DetectedFramework::default()`
/// if nothing is recognized.
/// Test: `test_detect_*` functions cover each language; `test_detect_unknown`
/// covers the empty-directory fallback.
pub fn detect_framework(root: &Path) -> DetectedFramework {
    if let Some(fw) = try_rust(root) {
        return fw;
    }
    if let Some(fw) = try_go(root) {
        return fw;
    }
    if let Some(fw) = try_java(root) {
        return fw;
    }
    if let Some(fw) = try_python(root) {
        return fw;
    }
    if let Some(fw) = try_javascript(root) {
        return fw;
    }
    DetectedFramework::default()
}

fn try_rust(root: &Path) -> Option<DetectedFramework> {
    let cargo = root.join("Cargo.toml");
    if !cargo.exists() {
        return None;
    }
    let contents = std::fs::read_to_string(&cargo).unwrap_or_default();
    let framework = if contents.contains("\"axum\"") || contents.contains("axum =") {
        Some("axum".to_string())
    } else if contents.contains("\"actix-web\"")
        || contents.contains("actix-web =")
        || contents.contains("\"actix_web\"")
        || contents.contains("actix_web =")
    {
        Some("actix-web".to_string())
    } else if contents.contains("\"rocket\"") || contents.contains("rocket =") {
        Some("rocket".to_string())
    } else if contents.contains("\"warp\"") || contents.contains("warp =") {
        Some("warp".to_string())
    } else {
        None
    };

    Some(DetectedFramework {
        language: Some("rust".to_string()),
        framework,
        package_manager: Some("cargo".to_string()),
        detected_from: vec!["Cargo.toml".to_string()],
        detected_at: Some(Utc::now()),
    })
}

fn try_go(root: &Path) -> Option<DetectedFramework> {
    if !root.join("go.mod").exists() {
        return None;
    }
    Some(DetectedFramework {
        language: Some("go".to_string()),
        framework: Some("go-modules".to_string()),
        package_manager: Some("go".to_string()),
        detected_from: vec!["go.mod".to_string()],
        detected_at: Some(Utc::now()),
    })
}

fn try_java(root: &Path) -> Option<DetectedFramework> {
    if root.join("pom.xml").exists() {
        return Some(DetectedFramework {
            language: Some("java".to_string()),
            framework: Some("maven".to_string()),
            package_manager: Some("maven".to_string()),
            detected_from: vec!["pom.xml".to_string()],
            detected_at: Some(Utc::now()),
        });
    }
    if root.join("build.gradle").exists() || root.join("build.gradle.kts").exists() {
        return Some(DetectedFramework {
            language: Some("java".to_string()),
            framework: Some("gradle".to_string()),
            package_manager: Some("gradle".to_string()),
            detected_from: vec!["build.gradle".to_string()],
            detected_at: Some(Utc::now()),
        });
    }
    None
}

fn try_python(root: &Path) -> Option<DetectedFramework> {
    let pyproject = root.join("pyproject.toml");
    if pyproject.exists() {
        let contents = std::fs::read_to_string(&pyproject).unwrap_or_default();
        let framework = if contents.contains("[tool.poetry]") {
            Some("poetry".to_string())
        } else if contents.contains("[project]") {
            Some("pep621".to_string())
        } else {
            None
        };
        let package_manager = if root.join("uv.lock").exists() {
            "uv".to_string()
        } else {
            "pip".to_string()
        };
        return Some(DetectedFramework {
            language: Some("python".to_string()),
            framework,
            package_manager: Some(package_manager),
            detected_from: vec!["pyproject.toml".to_string()],
            detected_at: Some(Utc::now()),
        });
    }
    if root.join("setup.py").exists() {
        return Some(DetectedFramework {
            language: Some("python".to_string()),
            framework: Some("setuptools".to_string()),
            package_manager: Some("pip".to_string()),
            detected_from: vec!["setup.py".to_string()],
            detected_at: Some(Utc::now()),
        });
    }
    None
}

fn try_javascript(root: &Path) -> Option<DetectedFramework> {
    let pkg = root.join("package.json");
    if !pkg.exists() {
        return None;
    }

    // Sub-framework detection by config-file presence.
    let has_next = root.join("next.config.js").exists()
        || root.join("next.config.ts").exists()
        || root.join("next.config.mjs").exists();
    let has_svelte =
        root.join("svelte.config.js").exists() || root.join("svelte.config.ts").exists();
    let has_nuxt = root.join("nuxt.config.js").exists() || root.join("nuxt.config.ts").exists();
    let has_vite = root.join("vite.config.js").exists() || root.join("vite.config.ts").exists();

    let framework = if has_next {
        Some("nextjs".to_string())
    } else if has_svelte {
        Some("sveltekit".to_string())
    } else if has_nuxt {
        Some("nuxt".to_string())
    } else if has_vite {
        Some("vite".to_string())
    } else {
        Some("node".to_string())
    };

    // Package manager — prefer the lockfile that exists.
    let package_manager = if root.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if root.join("yarn.lock").exists() {
        "yarn"
    } else if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    };

    let language = if root.join("tsconfig.json").exists() {
        "typescript"
    } else {
        "javascript"
    };

    // Capture every marker file we used so callers can show provenance.
    let mut detected_from = vec!["package.json".to_string()];
    if has_next {
        detected_from.push("next.config".to_string());
    }
    if has_svelte {
        detected_from.push("svelte.config".to_string());
    }
    if has_nuxt {
        detected_from.push("nuxt.config".to_string());
    }
    if has_vite && !has_next && !has_svelte && !has_nuxt {
        detected_from.push("vite.config".to_string());
    }
    if language == "typescript" {
        detected_from.push("tsconfig.json".to_string());
    }

    Some(DetectedFramework {
        language: Some(language.to_string()),
        framework,
        package_manager: Some(package_manager.to_string()),
        detected_from,
        detected_at: Some(Utc::now()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, contents: &str) {
        fs::write(dir.join(name), contents).expect("write fixture");
    }

    #[test]
    fn test_detect_rust_plain() {
        let td = TempDir::new().unwrap();
        write(
            td.path(),
            "Cargo.toml",
            "[package]\nname = \"hello\"\nversion = \"0.1.0\"\n",
        );
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("rust"));
        assert_eq!(fw.package_manager.as_deref(), Some("cargo"));
        assert_eq!(fw.framework, None);
        assert!(fw.detected_at.is_some());
        assert_eq!(fw.detected_from, vec!["Cargo.toml".to_string()]);
    }

    #[test]
    fn test_detect_rust_axum() {
        let td = TempDir::new().unwrap();
        write(
            td.path(),
            "Cargo.toml",
            "[package]\nname = \"api\"\n\n[dependencies]\naxum = \"0.7\"\n",
        );
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("rust"));
        assert_eq!(fw.framework.as_deref(), Some("axum"));
    }

    #[test]
    fn test_detect_typescript_nextjs() {
        let td = TempDir::new().unwrap();
        write(td.path(), "package.json", "{\"name\":\"app\"}");
        write(td.path(), "next.config.js", "module.exports = {};");
        write(td.path(), "tsconfig.json", "{}");
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("typescript"));
        assert_eq!(fw.framework.as_deref(), Some("nextjs"));
        assert_eq!(fw.package_manager.as_deref(), Some("npm"));
    }

    #[test]
    fn test_detect_typescript_sveltekit_pnpm() {
        let td = TempDir::new().unwrap();
        write(td.path(), "package.json", "{\"name\":\"app\"}");
        write(td.path(), "svelte.config.js", "export default {};");
        write(td.path(), "tsconfig.json", "{}");
        write(td.path(), "pnpm-lock.yaml", "lockfileVersion: 6.0\n");
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("typescript"));
        assert_eq!(fw.framework.as_deref(), Some("sveltekit"));
        assert_eq!(fw.package_manager.as_deref(), Some("pnpm"));
    }

    #[test]
    fn test_detect_javascript_node_default() {
        let td = TempDir::new().unwrap();
        write(td.path(), "package.json", "{\"name\":\"app\"}");
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("javascript"));
        assert_eq!(fw.framework.as_deref(), Some("node"));
        assert_eq!(fw.package_manager.as_deref(), Some("npm"));
    }

    #[test]
    fn test_detect_javascript_vite_yarn() {
        let td = TempDir::new().unwrap();
        write(td.path(), "package.json", "{\"name\":\"app\"}");
        write(td.path(), "vite.config.ts", "export default {};");
        write(td.path(), "tsconfig.json", "{}");
        write(td.path(), "yarn.lock", "");
        let fw = detect_framework(td.path());
        assert_eq!(fw.framework.as_deref(), Some("vite"));
        assert_eq!(fw.package_manager.as_deref(), Some("yarn"));
        assert_eq!(fw.language.as_deref(), Some("typescript"));
    }

    #[test]
    fn test_detect_python_pyproject() {
        let td = TempDir::new().unwrap();
        write(
            td.path(),
            "pyproject.toml",
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n",
        );
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("python"));
        assert_eq!(fw.framework.as_deref(), Some("pep621"));
        assert_eq!(fw.package_manager.as_deref(), Some("pip"));
    }

    #[test]
    fn test_detect_python_pyproject_uv() {
        let td = TempDir::new().unwrap();
        write(
            td.path(),
            "pyproject.toml",
            "[tool.poetry]\nname = \"x\"\nversion = \"0.1.0\"\n",
        );
        write(td.path(), "uv.lock", "");
        let fw = detect_framework(td.path());
        assert_eq!(fw.framework.as_deref(), Some("poetry"));
        assert_eq!(fw.package_manager.as_deref(), Some("uv"));
    }

    #[test]
    fn test_detect_python_setup_py() {
        let td = TempDir::new().unwrap();
        write(td.path(), "setup.py", "from setuptools import setup\n");
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("python"));
        assert_eq!(fw.framework.as_deref(), Some("setuptools"));
    }

    #[test]
    fn test_detect_go() {
        let td = TempDir::new().unwrap();
        write(td.path(), "go.mod", "module example.com/x\n");
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("go"));
        assert_eq!(fw.package_manager.as_deref(), Some("go"));
    }

    #[test]
    fn test_detect_java_maven() {
        let td = TempDir::new().unwrap();
        write(td.path(), "pom.xml", "<project/>");
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("java"));
        assert_eq!(fw.framework.as_deref(), Some("maven"));
    }

    #[test]
    fn test_detect_java_gradle() {
        let td = TempDir::new().unwrap();
        write(td.path(), "build.gradle.kts", "");
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("java"));
        assert_eq!(fw.framework.as_deref(), Some("gradle"));
    }

    #[test]
    fn test_detect_unknown() {
        let td = TempDir::new().unwrap();
        let fw = detect_framework(td.path());
        assert!(!fw.is_known());
        assert_eq!(fw.display(), "unknown");
    }

    #[test]
    fn test_priority_rust_wins_over_js() {
        // If a repo has both Cargo.toml and package.json, Rust takes priority.
        let td = TempDir::new().unwrap();
        write(td.path(), "Cargo.toml", "[package]\nname = \"x\"\n");
        write(td.path(), "package.json", "{\"name\":\"x\"}");
        let fw = detect_framework(td.path());
        assert_eq!(fw.language.as_deref(), Some("rust"));
    }
}
