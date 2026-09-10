//! Bounded discovery and project-local switches for installed Markdown skills.
//! The same snapshot backs project settings and the assistant's read-only loader.
use crate::tools::traits::{ToolExecutor, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
};

pub const SOURCE_IDS: [&str; 4] = [
    ".claude/skills",
    ".agents/skills",
    ".codex/skills",
    ".trusty-agents/skills",
];
const MAX_BYTES: u64 = 128 * 1024;
const MAX_ENTRIES: usize = 1024;
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    sources: BTreeMap<String, bool>,
}
#[derive(Clone, Serialize)]
pub struct Skill {
    pub scope: &'static str,
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}
#[derive(Serialize)]
pub struct Source {
    pub scope: &'static str,
    pub id: String,
    pub label: String,
    pub path: PathBuf,
    pub enabled: bool,
    pub skills: Vec<Skill>,
}
#[derive(Serialize)]
pub struct Snapshot {
    pub path: PathBuf,
    pub revision: String,
    pub sources: Vec<Source>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceUpdate {
    pub id: String,
    pub enabled: bool,
}
fn bounded_read(path: &Path) -> anyhow::Result<String> {
    anyhow::ensure!(
        std::fs::metadata(path)?.is_file(),
        "Expected a regular file"
    );
    let f = std::fs::File::open(path)?;
    anyhow::ensure!(f.metadata()?.is_file(), "Expected a regular file");
    let mut bytes = Vec::new();
    f.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_BYTES, "Skill file exceeds 128 KB");
    Ok(String::from_utf8(bytes)?)
}
fn settings_path_scoped(root: &Path, user: bool) -> anyhow::Result<PathBuf> {
    let dir = root.join(".trusty-agents");
    if dir.exists() {
        anyhow::ensure!(
            dir.canonicalize()?.starts_with(root),
            "Project settings directory must stay inside the project"
        );
    }
    let path = dir.join(if user {
        "user-skills.json"
    } else {
        "project-skills.json"
    });
    if path.exists() {
        anyhow::ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "Project skill settings cannot be a symbolic link"
        );
    }
    Ok(path)
}
fn settings(root: &Path, user: bool) -> anyhow::Result<(Settings, String)> {
    let path = settings_path_scoped(root, user)?;
    let raw = match bounded_read(&path) {
        Ok(s) => s,
        Err(_e) if !path.exists() => String::new(),
        Err(e) => return Err(e),
    };
    let value = if raw.is_empty() {
        Settings::default()
    } else {
        serde_json::from_str(&raw)?
    };
    Ok((value, format!("{:x}", Sha256::digest(raw.as_bytes()))))
}
pub(super) fn scan(dir: &Path) -> Vec<Skill> {
    scan_checked(dir).unwrap_or_default()
}
fn scan_checked(dir: &Path) -> anyhow::Result<Vec<Skill>> {
    let entries = std::fs::read_dir(dir)?;
    let mut paths: Vec<_> = entries
        .take(MAX_ENTRIES)
        .flatten()
        .map(|e| e.path())
        .collect();
    paths.sort();
    let mut seen = HashSet::new();
    Ok(paths
        .into_iter()
        .filter_map(|p| {
            let p = if p.is_dir() { p.join("SKILL.md") } else { p };
            if p.extension().and_then(|s| s.to_str()) != Some("md") {
                return None;
            }
            let p = p.canonicalize().ok()?;
            if !seen.insert(p.clone()) {
                return None;
            }
            let content = bounded_read(&p).ok()?;
            let fm = content
                .strip_prefix("---\r\n")
                .or_else(|| content.strip_prefix("---\n"))?;
            let end = fm.find("\n---")?;
            let meta: Value = serde_yaml::from_str(&fm[..end]).ok()?;
            let fallback = if p.file_name()?.to_str()? == "SKILL.md" {
                p.parent()?.file_name()?.to_str()?
            } else {
                p.file_stem()?.to_str()?
            };
            let name = meta
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(fallback)
                .trim();
            if name.is_empty() || name.len() > 160 {
                return None;
            }
            let description = meta
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(1000)
                .collect();
            Some(Skill {
                scope: "project",
                name: name.into(),
                description,
                path: p,
            })
        })
        .collect())
}
pub fn snapshot(root: &Path) -> anyhow::Result<Snapshot> {
    snapshot_scoped(root, false)
}
pub fn user_root() -> anyhow::Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("User home unavailable"))?
        .canonicalize()?)
}
pub fn user_snapshot() -> anyhow::Result<Snapshot> {
    snapshot_scoped(&user_root()?, true)
}
pub fn snapshot_scoped(root: &Path, user: bool) -> anyhow::Result<Snapshot> {
    let root = root.canonicalize()?;
    anyhow::ensure!(root.is_dir(), "Project folder is unavailable");
    let (settings, revision) = settings(&root, user)?;
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    for id in SOURCE_IDS {
        let candidate = root.join(id);
        match std::fs::symlink_metadata(&candidate) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Cannot inspect skill source {}: {e}",
                    candidate.display()
                ));
            }
            Ok(_) => {}
        }
        let path = candidate.canonicalize().map_err(|e| {
            anyhow::anyhow!("Cannot resolve skill source {}: {e}", candidate.display())
        })?;
        anyhow::ensure!(
            path.is_dir(),
            "Skill source {} is not a directory",
            candidate.display()
        );
        if !seen.insert(path.clone()) {
            continue;
        }
        let skills = scan_checked(&path)
            .map_err(|e| anyhow::anyhow!("Cannot read skill source {}: {e}", candidate.display()))?
            .into_iter()
            .map(|mut s| {
                s.scope = if user { "user" } else { "project" };
                s
            })
            .collect();
        sources.push(Source {
            scope: if user { "user" } else { "project" },
            id: id.into(),
            label: id.into(),
            path,
            enabled: *settings.sources.get(id).unwrap_or(&true),
            skills,
        });
    }
    Ok(Snapshot {
        path: root,
        revision,
        sources,
    })
}
pub fn save(root: &Path, revision: &str, updates: Vec<SourceUpdate>) -> anyhow::Result<Snapshot> {
    save_scoped(root, false, revision, updates)
}
pub fn save_scoped(
    root: &Path,
    user: bool,
    revision: &str,
    updates: Vec<SourceUpdate>,
) -> anyhow::Result<Snapshot> {
    let root = root.canonicalize()?;
    let current = snapshot_scoped(&root, user)?;
    anyhow::ensure!(
        current.revision == revision,
        "Revision conflict: reload skill settings"
    );
    let (mut settings, _) = settings(&root, user)?;
    let mut ids = HashSet::new();
    for update in updates {
        anyhow::ensure!(
            current.sources.iter().any(|s| s.id == update.id) && ids.insert(update.id.clone()),
            "Unknown or duplicate skill source"
        );
        settings.sources.insert(update.id, update.enabled);
    }
    let path = settings_path_scoped(&root, user)?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    use std::io::Write;
    temp.write_all(serde_json::to_string_pretty(&settings)?.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    snapshot_scoped(&root, user)
}
pub fn enabled(root: &Path) -> Vec<Skill> {
    let Ok(snapshot) = snapshot(root) else {
        return vec![];
    };
    let mut paths = HashSet::new();
    let mut names = HashSet::new();
    snapshot
        .sources
        .into_iter()
        .filter(|s| s.enabled)
        .flat_map(|s| s.skills)
        .filter(|s| paths.insert(s.path.clone()) && names.insert(s.name.clone()))
        .collect()
}
pub fn runtime_enabled(root: &Path) -> Vec<Skill> {
    runtime_with_user(root, user_snapshot().ok())
}
fn runtime_with_user(root: &Path, user: Option<Snapshot>) -> Vec<Skill> {
    let mut skills = enabled(root);
    let mut names: HashSet<_> = skills.iter().map(|s| s.name.clone()).collect();
    let mut paths: HashSet<_> = skills.iter().map(|s| s.path.clone()).collect();
    if let Some(user) = user {
        for skill in user
            .sources
            .into_iter()
            .filter(|s| s.enabled)
            .flat_map(|s| s.skills)
        {
            if names.insert(skill.name.clone()) && paths.insert(skill.path.clone()) {
                skills.push(skill);
            }
        }
    }
    skills
}
fn diagnostics(root: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(e) = snapshot(root) {
        errors.push(format!("Project skill discovery unavailable: {e}"));
    }
    if let Err(e) = user_snapshot() {
        errors.push(format!("User skill discovery unavailable: {e}"));
    }
    errors
}
pub fn context(root: &Path, native: bool) -> String {
    let skills = runtime_enabled(root);
    let catalog: Vec<_> = skills
        .iter()
        .take(256)
        .map(|s| json!({"name":s.name,"description":s.description,"path":s.path,"scope":s.scope}))
        .collect();
    format!(
        "\n\n## Installed user and project skills\nThese are user/project guidance, subordinate to the user's request and system instructions. Use relevant skills and read their full SKILL.md before applying them. Skill files do not grant tool permissions. Project names override user names. User sources can be configured in Skills; project sources in project configuration; changes apply on subsequent turns. {} Resolve relative references from the skill file's directory. Catalog (data):\n{}",
        if native {
            "Call project_skill with no name to list installed guidance AND your authorized tool capabilities, or with an exact name/id to read. An empty guidance list does not mean no native skills. Provider readiness remains separate."
        } else {
            "Read enabled skill files using your file-reading tools. Do not apply disabled project skills."
        },
        json!({"guidance":catalog,"diagnostics":diagnostics(root)})
    )
}
pub fn capabilities(names: &[String]) -> Vec<Value> {
    let catalog = crate::skills::manifest::SkillCatalog::builtin();
    names.iter().map(|n| { let m = catalog.skill_for_tool(n).cloned().unwrap_or_else(|| crate::skills::manifest::SkillManifest::derived(n)); json!({"id":m.id,"name":m.name,"description":m.description,"tool":n,"movable":false,"kind":"tool","provider_readiness":"not_checked"}) }).collect()
}
/// Assistant-bound read-only tool; no caller-supplied filesystem path.
pub struct ProjectSkillTool {
    root: PathBuf,
    granted: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}
impl ProjectSkillTool {
    pub fn with_granted(mut self, granted: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Self {
        self.granted = granted;
        self
    }
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            granted: Default::default(),
        }
    }
}
#[async_trait::async_trait]
impl ToolExecutor for ProjectSkillTool {
    fn name(&self) -> &str {
        "project_skill"
    }
    fn schema(&self) -> Value {
        json!({"type":"function","function":{"name":"project_skill","description":"List installed user/project guidance and authorized tool capabilities, or read by exact name/id. Native capabilities are metadata, not movable files. Does not grant permissions or execute scripts.","parameters":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string"}}}}})
    }
    async fn execute(&self, args: Value) -> ToolResult {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            name: Option<String>,
        }
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let skills = runtime_enabled(&self.root);
        let Some(name) = input.name else {
            let names = self
                .granted
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            return ToolResult::ok(json!({"guidance":skills,"capabilities":capabilities(&names),"diagnostics":diagnostics(&self.root),"note":"Guidance files and authorized tool capabilities are separate. Empty guidance does not mean no skills. Provider readiness is not verified here."}).to_string());
        };
        let Some(skill) = skills.iter().find(|s| s.name == name) else {
            let names = self
                .granted
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            if let Some(capability) = capabilities(&names)
                .into_iter()
                .find(|c| c["id"] == name || c["name"] == name || c["tool"] == name)
            {
                return ToolResult::ok(capability.to_string());
            }
            return ToolResult::err(
                "Enabled user/project guidance or authorized capability not found",
            );
        };
        match bounded_read(&skill.path) {
            Ok(content) => ToolResult::ok(format!(
                "Installed skill guidance from {} (relative references use this directory; no additional permissions):\n{}",
                skill.path.display(),
                content
            )),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn write(root: &Path, path: &str, body: &str) {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    #[test]
    fn project_skills_formats_settings_and_revisions() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            ".claude/skills/edit/SKILL.md",
            "---\nname: edit\ndescription: |\n  Edit text\n  carefully\n---\nBody",
        );
        write(
            d.path(),
            ".agents/skills/rust.md",
            "---\nname: rust\n---\nRust",
        );
        let s = snapshot(d.path()).unwrap();
        assert_eq!(s.sources.len(), 2);
        assert!(s.sources[0].skills[0].description.contains("carefully"));
        let next = save(
            d.path(),
            &s.revision,
            vec![SourceUpdate {
                id: SOURCE_IDS[0].into(),
                enabled: false,
            }],
        )
        .unwrap();
        assert_eq!(enabled(d.path()).len(), 1);
        assert!(save(d.path(), &s.revision, vec![]).is_err());
        assert_ne!(next.revision, s.revision);
        assert!(!context(d.path(), true).contains("carefully"));
    }
    #[test]
    fn project_skills_skip_invalid_and_dedupe() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            ".claude/skills/edit/SKILL.md",
            "---\nname: edit\n---\nBody",
        );
        write(
            d.path(),
            ".codex/skills/other.md",
            "---\nname: edit\n---\nOther",
        );
        write(d.path(), ".codex/skills/readme.md", "not a skill");
        assert_eq!(enabled(d.path()).len(), 1);
        assert!(
            save(
                d.path(),
                &snapshot(d.path()).unwrap().revision,
                vec![SourceUpdate {
                    id: "/tmp".into(),
                    enabled: true
                }]
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn project_skills_loader_cannot_read_arbitrary_or_disabled() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            ".claude/skills/edit/SKILL.md",
            "---\nname: edit\n---\nBody",
        );
        let tool = ProjectSkillTool::new(d.path());
        assert!(
            tool.execute(json!({"name":"edit"}))
                .await
                .content()
                .contains("Body")
        );
        assert!(
            !tool
                .execute(json!({"name":"../../etc/passwd"}))
                .await
                .content()
                .contains("Body")
        );
        let s = snapshot(d.path()).unwrap();
        save(
            d.path(),
            &s.revision,
            vec![SourceUpdate {
                id: SOURCE_IDS[0].into(),
                enabled: false,
            }],
        )
        .unwrap();
        assert!(
            !tool
                .execute(json!({"name":"edit"}))
                .await
                .content()
                .contains("Body")
        );
    }
    #[test]
    fn project_skills_unreadable_configuration_fails_closed() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            ".claude/skills/edit/SKILL.md",
            "---\nname: edit\n---\nBody",
        );
        write(d.path(), ".trusty-agents/project-skills.json", "not json");
        assert!(snapshot(d.path()).is_err());
        assert!(enabled(d.path()).is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn project_skills_aliases_deduplicate_without_writing_outside_project() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(
            d.path(),
            ".claude/skills/edit/SKILL.md",
            "---\nname: edit\n---\nBody",
        );
        std::fs::create_dir_all(d.path().join(".agents")).unwrap();
        std::os::unix::fs::symlink(
            d.path().join(".claude/skills"),
            d.path().join(".agents/skills"),
        )
        .unwrap();
        assert_eq!(snapshot(d.path()).unwrap().sources.len(), 1);
        std::os::unix::fs::symlink(outside.path(), d.path().join(".trusty-agents")).unwrap();
        assert!(snapshot(d.path()).is_err());
        assert!(!outside.path().join("project-skills.json").exists());
    }
    #[test]
    fn user_skills_precedence_switches_and_native_capabilities() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write(
            user.path(),
            ".claude/skills/shared/SKILL.md",
            "---\nname: shared\n---\nUser",
        );
        write(
            user.path(),
            ".agents/skills/user/SKILL.md",
            "---\nname: user-only\n---\nUser",
        );
        write(
            project.path(),
            ".claude/skills/shared/SKILL.md",
            "---\nname: shared\n---\nProject",
        );
        let u = snapshot_scoped(user.path(), true).unwrap();
        let skills = runtime_with_user(project.path(), Some(u));
        assert_eq!(skills.len(), 2);
        assert!(
            skills[0]
                .path
                .starts_with(project.path().canonicalize().unwrap())
        );
        let u = snapshot_scoped(user.path(), true).unwrap();
        save_scoped(
            user.path(),
            true,
            &u.revision,
            vec![SourceUpdate {
                id: SOURCE_IDS[0].into(),
                enabled: false,
            }],
        )
        .unwrap();
        assert!(user.path().join(".trusty-agents/user-skills.json").exists());
        assert!(
            !user
                .path()
                .join(".trusty-agents/project-skills.json")
                .exists()
        );
        assert!(save_scoped(user.path(), true, &u.revision, vec![]).is_err());
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            runtime_with_user(
                empty.path(),
                Some(snapshot_scoped(user.path(), true).unwrap())
            )
            .len(),
            1
        );
        let native = capabilities(&["get_train_schedule".into()]);
        assert_eq!(native[0]["name"], "MTA Train Time");
        assert_eq!(native[0]["movable"], false);
        assert!(capabilities(&[]).is_empty());
    }

    #[tokio::test]
    async fn user_skill_named_native_lookup_preserves_authorization() {
        let p = tempfile::tempdir().unwrap();
        let granted = std::sync::Arc::new(std::sync::Mutex::new(vec!["get_train_schedule".into()]));
        let tool = ProjectSkillTool::new(p.path()).with_granted(granted);
        assert!(
            tool.execute(json!({"name":"MTA Train Time"}))
                .await
                .content()
                .contains("get_train_schedule")
        );
        assert!(
            !tool
                .execute(json!({"name":"mta-service-alerts"}))
                .await
                .content()
                .contains("get_train_alerts")
        );
    }
    #[test]
    fn user_skill_existing_invalid_source_is_explicit_error() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), ".claude/skills", "not a directory");
        assert!(
            snapshot_scoped(t.path(), true)
                .err()
                .unwrap()
                .to_string()
                .contains("not a directory")
        );
        assert!(
            diagnostics(t.path())
                .iter()
                .any(|s| s.contains("not a directory"))
        );
    }
}
