//! Read-only workspace browsing. Children are relative to persisted, canonical roots;
//! canonical containment checks prevent traversal and symlink escapes.
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::Manager;
mod diff;

const MAX_FILE: u64 = 12 * 1024 * 1024;
static ROOT_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct WorkspaceRoot {
    pub id: String,
    pub name: String,
    pub path: String,
}
#[derive(Serialize)]
pub struct FileEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: Option<u64>,
}
#[derive(Serialize)]
pub struct FileList {
    entries: Vec<FileEntry>,
}
#[derive(Serialize)]
pub struct FileContent {
    path: String,
    kind: String,
    content: String,
    mime: Option<String>,
    size: u64,
}
#[derive(Serialize, Debug)]
pub struct FileDiff {
    available: bool,
    diff: String,
    reason: Option<String>,
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn store(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(err)?
        .join("workspace-roots.json"))
}
fn load(path: &Path) -> Result<Vec<WorkspaceRoot>, String> {
    match fs::read(path) {
        Ok(data) => serde_json::from_slice(&data).map_err(err),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(err(e)),
    }
}
fn register(store: &Path, path: &Path) -> Result<WorkspaceRoot, String> {
    let _lock = ROOT_LOCK.lock().map_err(err)?;
    let path = fs::canonicalize(path).map_err(err)?;
    if !path.is_dir() {
        return Err("Choose a directory".into());
    }
    let path_string = path
        .to_str()
        .ok_or("Directory name is not valid UTF-8")?
        .to_owned();
    let mut roots = load(store)?;
    if let Some(root) = roots.iter().find(|r| r.path == path_string) {
        return Ok(root.clone());
    }
    let root = WorkspaceRoot {
        id: format!(
            "root-{:x}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(err)?
                .as_nanos(),
            roots.len()
        ),
        name: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&path_string)
            .to_owned(),
        path: path_string,
    };
    roots.push(root.clone());
    fs::create_dir_all(store.parent().ok_or("Invalid root registry")?).map_err(err)?;
    let temp = store.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec(&roots).map_err(err)?).map_err(err)?;
    fs::rename(&temp, store).map_err(err)?;
    Ok(root)
}
fn resolve(root: &Path, child: &str) -> Result<PathBuf, String> {
    let relative = Path::new(child);
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("File path must stay inside the selected folder".into());
    }
    let canonical_root = fs::canonicalize(root).map_err(err)?;
    // A registered root replaced by a symlink must not silently grant a new scope.
    if canonical_root != root {
        return Err("Selected folder has moved; select it again".into());
    }
    let target = fs::canonicalize(root.join(relative)).map_err(err)?;
    if !target.starts_with(&canonical_root) {
        return Err("File is outside the selected folder".into());
    }
    Ok(target)
}
fn root_for(app: &tauri::AppHandle, id: &str) -> Result<PathBuf, String> {
    load(&store(app)?)?
        .into_iter()
        .find(|r| r.id == id)
        .map(|r| PathBuf::from(r.path))
        .ok_or("Unknown workspace folder".into())
}
#[tauri::command]
pub fn workspace_register_root(
    app: tauri::AppHandle,
    path: String,
) -> Result<WorkspaceRoot, String> {
    register(&store(&app)?, Path::new(&path))
}
#[derive(Serialize, Debug)]
pub struct WorkspaceFolder {
    #[serde(flatten)]
    root: WorkspaceRoot,
    available: bool,
    aliases: Vec<String>,
}
// Recover only known temporary checkout layouts beneath an existing repository.
// Ordinary missing folders remain unavailable; existing worktrees stay distinct.
fn project_directory(path: &Path) -> PathBuf {
    if path.exists() {
        return path.to_owned();
    }
    for ancestor in path.ancestors().skip(1) {
        if !ancestor.join(".git").exists() {
            continue;
        }
        let Ok(relative) = path.strip_prefix(ancestor) else {
            continue;
        };
        let parts: Vec<_> = relative.components().collect();
        let mut offset = 0;
        while offset < parts.len() {
            let part = parts[offset].as_os_str();
            if part == ".base" {
                offset += 1;
            } else if part == ".worktrees" && offset + 1 < parts.len() {
                offset += 2;
            } else if part == ".claude"
                && offset + 2 < parts.len()
                && parts[offset + 1].as_os_str() == "worktrees"
            {
                offset += 3;
            } else {
                break;
            }
        }
        if offset == 0 {
            return path.to_owned();
        }
        let candidate = parts[offset..]
            .iter()
            .fold(ancestor.to_owned(), |base, part| {
                base.join(part.as_os_str())
            });
        if let (Ok(root), Ok(directory)) =
            (fs::canonicalize(ancestor), fs::canonicalize(&candidate))
        {
            if directory.is_dir() && directory.starts_with(root) {
                return directory;
            }
        }
        return path.to_owned();
    }
    path.to_owned()
}
fn reconcile_roots(store: &Path, paths: Vec<String>) -> Result<Vec<WorkspaceFolder>, String> {
    let registered = load(store)?;
    let mut folders: Vec<WorkspaceFolder> = Vec::new();
    for root in registered {
        let available = fs::canonicalize(&root.path)
            .map(|path| path.is_dir() && path == Path::new(&root.path))
            .unwrap_or(false);
        if !folders.iter().any(|folder| folder.root.path == root.path) {
            folders.push(WorkspaceFolder {
                aliases: vec![root.path.clone()],
                root,
                available,
            });
        }
    }
    for path in paths {
        if folders.iter().any(|folder| folder.aliases.contains(&path)) {
            continue;
        }
        match register(store, &project_directory(Path::new(&path))) {
            Ok(root) => {
                if let Some(folder) = folders
                    .iter_mut()
                    .find(|folder| folder.root.path == root.path)
                {
                    folder.aliases.push(path);
                } else {
                    folders.push(WorkspaceFolder {
                        aliases: vec![path],
                        root,
                        available: true,
                    });
                }
            }
            Err(_) => folders.push(WorkspaceFolder {
                root: WorkspaceRoot {
                    id: format!("missing:{path}"),
                    name: Path::new(&path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(&path)
                        .to_owned(),
                    path: path.clone(),
                },
                available: false,
                aliases: vec![path],
            }),
        }
    }
    Ok(folders)
}
#[tauri::command]
pub fn workspace_list_roots(
    app: tauri::AppHandle,
    paths: Option<Vec<String>>,
) -> Result<Vec<WorkspaceFolder>, String> {
    reconcile_roots(&store(&app)?, paths.unwrap_or_default())
}
#[tauri::command]
pub fn workspace_list_files(
    app: tauri::AppHandle,
    root_id: String,
    path: Option<String>,
) -> Result<FileList, String> {
    list_files(&root_for(&app, &root_id)?, path.as_deref().unwrap_or(""))
}
fn list_files(root: &Path, child: &str) -> Result<FileList, String> {
    let target = resolve(root, child)?;
    let mut entries = vec![];
    for (index, item) in fs::read_dir(target).map_err(err)?.enumerate() {
        if index >= 20_000 {
            return Err("Folder has too many entries to display".into());
        }
        let item = item.map_err(err)?;
        let Some(name) = item.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let relative = Path::new(child).join(&name).to_string_lossy().into_owned();
        let Ok(resolved) = resolve(root, &relative) else {
            continue;
        };
        let metadata = fs::metadata(resolved).map_err(err)?;
        if !metadata.is_dir() && !metadata.is_file() {
            continue;
        }
        entries.push(FileEntry {
            name,
            path: relative,
            is_dir: metadata.is_dir(),
            size: metadata.is_file().then_some(metadata.len()),
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(FileList { entries })
}
// Open every canonical component without following links, closing the usual
// canonicalize/open race when another process swaps an ancestor directory.
#[cfg(unix)]
fn open_regular(path: &Path) -> Result<fs::File, String> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };
    let mut directory = fs::File::open("/").map_err(err)?;
    let components: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    for (index, component) in components.iter().enumerate() {
        let name = CString::new(component.as_bytes()).map_err(err)?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK
            | if index + 1 < components.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // SAFETY: the descriptor and C string are live throughout openat; a
        // successful descriptor is immediately transferred to File ownership.
        let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(err(std::io::Error::last_os_error()));
        }
        directory = unsafe { fs::File::from_raw_fd(descriptor) };
    }
    Ok(directory)
}
#[cfg(not(unix))]
fn open_regular(path: &Path) -> Result<fs::File, String> {
    fs::File::open(path).map_err(err)
}

fn read_file(root: &Path, child: &str) -> Result<FileContent, String> {
    let target = resolve(root, child)?;
    if !fs::metadata(&target).map_err(err)?.is_file() {
        return Err("Selected item is not a regular file".into());
    }
    let file = open_regular(&target)?;
    let metadata = file.metadata().map_err(err)?;
    if !metadata.is_file() {
        return Err("Selected item is not a regular file".into());
    }
    if metadata.len() > MAX_FILE {
        return Err("File exceeds the 12 MB preview limit".into());
    }
    let mut bytes = vec![];
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("File exceeds the 12 MB preview limit".into());
    }
    let extension = target
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mime = match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        "bmp" => Some("image/bmp"),
        _ => None,
    };
    let (kind, content) = if let Some(mime) = mime {
        (
            "image",
            format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            ),
        )
    } else if !bytes.contains(&0) {
        match String::from_utf8(bytes.clone()) {
            Ok(text) => (
                if matches!(extension.as_str(), "md" | "markdown" | "mdown") {
                    "markdown"
                } else {
                    "code"
                },
                text,
            ),
            Err(_) => ("unsupported", String::new()),
        }
    } else {
        ("unsupported", String::new())
    };
    Ok(FileContent {
        path: child.into(),
        kind: kind.into(),
        content,
        mime: mime.map(str::to_owned),
        size: bytes.len() as u64,
    })
}
#[tauri::command]
pub fn workspace_read_file(
    app: tauri::AppHandle,
    root_id: String,
    path: String,
) -> Result<FileContent, String> {
    read_file(&root_for(&app, &root_id)?, &path)
}
#[tauri::command]
pub async fn workspace_diff_file(
    app: tauri::AppHandle,
    root_id: String,
    path: String,
) -> Result<FileDiff, String> {
    diff::diff_file(&root_for(&app, &root_id)?, &path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) struct Temp(pub(super) PathBuf);
    impl Temp {
        pub(super) fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "workspace-files-{}-{}-{}",
                std::process::id(),
                {
                    static NEXT: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(0);
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                },
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn persists_and_deduplicates_roots() {
        let t = Temp::new();
        let store = t.0.join("state/roots.json");
        let root = register(&store, &t.0).unwrap();
        assert_eq!(register(&store, &t.0.join(".")).unwrap().id, root.id);
        assert_eq!(load(&store).unwrap().len(), 1);
        fs::write(&store, "broken").unwrap();
        assert!(load(&store).is_err());
    }
    #[test]
    fn reconciles_deleted_worktrees_to_existing_project_directories() {
        let t = Temp::new();
        fs::create_dir(t.0.join(".git")).unwrap();
        let project = t.0.join("crates/agents");
        fs::create_dir_all(&project).unwrap();
        let stale =
            t.0.join(".worktrees/old/.claude/worktrees/worker/crates/agents");
        assert_eq!(project_directory(&stale), project);
        let live = t.0.join(".worktrees/live");
        fs::create_dir_all(&live).unwrap();
        fs::write(live.join(".git"), "gitdir: ignored").unwrap();
        let missing_child = live.join("crates/agents");
        assert_eq!(project_directory(&missing_child), missing_child);
        let nested = live.join(".worktrees/dead/crates/agents");
        assert_eq!(project_directory(&nested), nested);
        let ordinary = t.0.join("missing/crates/agents");
        assert_eq!(project_directory(&ordinary), ordinary);
        let store = t.0.join("state/roots.json");
        let folders = reconcile_roots(
            &store,
            vec![
                stale.to_string_lossy().into_owned(),
                project.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].aliases.len(), 2);
        assert!(folders[0].available);
    }

    #[test]
    fn reconciles_directory_aliases_and_missing_roots() {
        let t = Temp::new();
        let store = t.0.join("state/roots.json");
        let directory = t.0.join("project");
        fs::create_dir(&directory).unwrap();
        let root = register(&store, &directory).unwrap();
        let alias = t.0.join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&directory, &alias).unwrap();
        #[cfg(not(unix))]
        let alias = directory.join(".");
        let missing = t.0.join("missing");
        let folders = reconcile_roots(
            &store,
            vec![
                alias.to_string_lossy().into_owned(),
                missing.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].root.id, root.id);
        assert!(folders[0].available);
        assert_eq!(folders[0].aliases.len(), 2);
        assert!(!folders[1].available);
        fs::remove_dir(&directory).unwrap();
        assert!(!reconcile_roots(&store, vec![]).unwrap()[0].available);
        fs::create_dir(&directory).unwrap();
        assert!(reconcile_roots(&store, vec![]).unwrap()[0].available);
    }

    #[test]
    fn prevents_traversal_and_symlink_escape() {
        let t = Temp::new();
        assert!(resolve(&t.0, "../secret").is_err());
        assert!(resolve(&t.0, "/etc/passwd").is_err());
        #[cfg(unix)]
        {
            let other = Temp::new();
            fs::write(other.0.join("secret"), "private").unwrap();
            std::os::unix::fs::symlink(&other.0, t.0.join("escape")).unwrap();
            assert!(read_file(&t.0, "escape/secret").is_err());
            assert!(list_files(&t.0, "").unwrap().entries.is_empty());
        }
    }
    #[test]
    fn classifies_and_bounds_files() {
        let t = Temp::new();
        fs::write(t.0.join("note.md"), "# Hello").unwrap();
        assert_eq!(read_file(&t.0, "note.md").unwrap().kind, "markdown");
        fs::write(t.0.join("binary"), [0, 255]).unwrap();
        assert_eq!(read_file(&t.0, "binary").unwrap().kind, "unsupported");
        fs::write(t.0.join("a.png"), [137, 80, 78, 71]).unwrap();
        assert!(read_file(&t.0, "a.png")
            .unwrap()
            .content
            .starts_with("data:image/png;base64,"));
        assert!(read_file(&t.0, "").is_err());
        fs::File::create(t.0.join("large"))
            .unwrap()
            .set_len(MAX_FILE + 1)
            .unwrap();
        assert!(read_file(&t.0, "large").is_err());
    }
    #[test]
    fn lists_directories_first_and_rejects_special_files() {
        let t = Temp::new();
        fs::create_dir(t.0.join("z-folder")).unwrap();
        fs::write(t.0.join("a.rs"), "fn main() {}\n").unwrap();
        let entries = list_files(&t.0, "").unwrap().entries;
        assert_eq!(entries[0].name, "z-folder");
        assert_eq!(entries[1].path, "a.rs");
        assert_eq!(read_file(&t.0, "a.rs").unwrap().kind, "code");
        #[cfg(unix)]
        {
            use std::{ffi::CString, os::unix::ffi::OsStrExt};
            let fifo = CString::new(t.0.join("pipe").as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            assert!(read_file(&t.0, "pipe").is_err());
            std::os::unix::fs::symlink(t.0.join("a.rs"), t.0.join("link")).unwrap();
            assert!(open_regular(&t.0.join("link")).is_err());
            // Stable links to files within the root are safe to preview.
            assert_eq!(read_file(&t.0, "link").unwrap().kind, "code");
        }
    }
}
