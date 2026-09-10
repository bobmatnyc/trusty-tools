//! Read-only Git HEAD comparisons use private snapshots without repository helpers.
use super::{err, read_file, resolve, FileDiff, MAX_FILE};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncReadExt, process::Command};

const MAX_DIFF: u64 = 2 * 1024 * 1024;

fn unavailable(reason: impl Into<String>) -> FileDiff {
    FileDiff {
        available: false,
        diff: String::new(),
        reason: Some(reason.into()),
    }
}
async fn git_output(
    root: &Path,
    args: &[&str],
    limit: u64,
    isolated: bool,
) -> Result<(i32, String), String> {
    let mut command = Command::new("git");
    command
        .arg("--no-pager")
        .arg("--literal-pathspecs")
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    // Inherited Git overrides must not redirect reads to a different repository.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(name);
    }
    if isolated {
        // Snapshot comparisons never discover the original repository or load
        // user/system attributes, configuration, clean filters, or diff drivers.
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        command
            .env("GIT_CONFIG_GLOBAL", null)
            .env("GIT_CONFIG_SYSTEM", null)
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env(
                "GIT_CEILING_DIRECTORIES",
                root.parent().ok_or("Invalid snapshot folder")?,
            );
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(err)?;
    let stdout = child.stdout.take().ok_or("Git output unavailable")?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut bytes = vec![];
        stdout
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(err)?;
        if bytes.len() as u64 > limit {
            return Err("Git output exceeds the preview limit".into());
        }
        let status = child.wait().await.map_err(err)?.code().unwrap_or(-1);
        Ok((
            status,
            String::from_utf8(bytes).map_err(|_| "Git version is not UTF-8 text".to_owned())?,
        ))
    })
    .await
    .map_err(|_| "Git preview timed out".to_string())?
}
async fn git(root: &Path, args: &[&str]) -> Result<(bool, String), String> {
    let (status, output) = git_output(root, args, MAX_DIFF, false).await?;
    Ok((status == 0, output))
}
struct DiffSnapshot(PathBuf);
impl DiffSnapshot {
    fn create(before: &str, after: &str) -> Result<Self, String> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        let path = std::env::temp_dir().join(format!(
            "trusty-file-diff-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(err)?
                .as_nanos()
        ));
        builder.create(&path).map_err(err)?;
        let snapshot = Self(path);
        for (name, content) in [("before", before), ("after", after)] {
            use std::io::Write;
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options
                .open(snapshot.0.join(name))
                .map_err(err)?
                .write_all(content.as_bytes())
                .map_err(err)?;
        }
        Ok(snapshot)
    }
}
impl Drop for DiffSnapshot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub(super) async fn diff_file(root: &Path, path: &str) -> Result<FileDiff, String> {
    let content = read_file(root, path)?;
    if matches!(content.kind.as_str(), "image" | "unsupported") {
        return Ok(unavailable("Diff preview supports text files"));
    }
    let target = resolve(root, path)?;
    let git_root = target.parent().ok_or("Invalid file parent")?;
    let (in_repo, repository) = git(git_root, &["rev-parse", "--show-toplevel"]).await?;
    if !in_repo {
        return Ok(unavailable("This folder is not in a Git repository"));
    }
    let repository = fs::canonicalize(repository.trim_end_matches('\n')).map_err(err)?;
    let relative = target
        .strip_prefix(&repository)
        .map_err(err)?
        .to_str()
        .ok_or("Invalid filename")?;
    let revision = format!("HEAD:{relative}");
    // Read the committed blob directly. Working-tree Git diff can execute clean
    // filters even with --no-ext-diff/--no-textconv, so never pass it user files.
    let exists = git(git_root, &["cat-file", "-e", &revision]).await?.0;
    let before = if exists {
        let (status, value) =
            git_output(git_root, &["cat-file", "blob", &revision], MAX_FILE, false).await?;
        if status != 0 {
            return Ok(unavailable("Git could not read this file from HEAD"));
        }
        if value.contains('\0') {
            return Ok(unavailable("Git version is not a text file"));
        }
        value
    } else {
        String::new()
    };
    let snapshot = DiffSnapshot::create(&before, &content.content)?;
    let (status, diff) = git_output(
        &snapshot.0,
        &[
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--",
            "before",
            "after",
        ],
        MAX_DIFF,
        true,
    )
    .await?;
    if status != 0 && status != 1 {
        return Ok(unavailable("Git could not produce a diff for this file"));
    }
    let label = path.escape_debug().to_string();
    let diff = diff
        .lines()
        .map(|line| match line {
            "diff --git a/before b/after" => format!("diff --git a/{label} b/{label}"),
            "--- a/before" => {
                if exists {
                    format!("--- a/{label}")
                } else {
                    "--- /dev/null".into()
                }
            }
            "+++ b/after" => format!("+++ b/{label}"),
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(FileDiff {
        available: true,
        diff,
        reason: None,
    })
}
#[cfg(test)]
mod tests {
    use super::super::tests::Temp;
    use super::*;
    #[tokio::test]
    async fn diff_includes_staged_and_working_changes_and_literal_paths() {
        let t = Temp::new();
        assert!(diff_file(&t.0, "missing").await.is_err());
        git(&t.0, &["init"]).await.unwrap();
        fs::write(t.0.join("a[1].md"), "original\n").unwrap();
        git(&t.0, &["add", "."]).await.unwrap();
        assert!(
            git(
                &t.0,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "-m",
                    "initial"
                ]
            )
            .await
            .unwrap()
            .0
        );
        fs::write(t.0.join("a[1].md"), "staged\n").unwrap();
        git(&t.0, &["add", "."]).await.unwrap();
        fs::write(t.0.join("a[1].md"), "working\n").unwrap();
        let diff = diff_file(&t.0, "a[1].md").await.unwrap();
        assert!(diff.available);
        assert!(diff.diff.contains("-original"));
        assert!(diff.diff.contains("+working"));
        fs::write(t.0.join("new.md"), "added\n").unwrap();
        assert!(diff_file(&t.0, "new.md")
            .await
            .unwrap()
            .diff
            .contains("+added"));
    }
    #[tokio::test]
    async fn diff_does_not_execute_repository_helpers_and_reads_head_after_index_removal() {
        let t = Temp::new();
        let repository = t.0.join("nested");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init"]).await.unwrap();
        fs::write(repository.join("file.txt"), "original\n").unwrap();
        git(&repository, &["add", "."]).await.unwrap();
        assert!(
            git(
                &repository,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "-m",
                    "initial"
                ]
            )
            .await
            .unwrap()
            .0
        );
        // Install malicious helper commands only after preparing the commit.
        fs::write(
            repository.join(".gitattributes"),
            "file.txt filter=evil diff=evil\n",
        )
        .unwrap();
        for (key, value) in [
            ("filter.evil.clean", "touch clean-executed; cat"),
            ("filter.evil.process", "touch process-executed; cat"),
            ("diff.evil.command", "touch diff-executed"),
            ("diff.evil.textconv", "touch textconv-executed"),
            ("core.fsmonitor", "touch fsmonitor-executed"),
        ] {
            assert!(git(&repository, &["config", key, value]).await.unwrap().0);
        }
        fs::write(repository.join("file.txt"), "changed\n").unwrap();
        let result = diff_file(&t.0, "nested/file.txt").await.unwrap();
        assert!(result.available);
        assert!(result.diff.contains("-original"));
        assert!(result.diff.contains("+changed"));
        for marker in [
            "clean-executed",
            "process-executed",
            "diff-executed",
            "textconv-executed",
            "fsmonitor-executed",
        ] {
            assert!(!repository.join(marker).exists(), "helper ran: {marker}");
        }
        // Remove only the index entry: a retained working file still compares
        // with its committed version, not an empty invented untracked baseline.
        assert!(
            git(
                &repository,
                &["update-index", "--force-remove", "--", "file.txt"]
            )
            .await
            .unwrap()
            .0
        );
        let result = diff_file(&t.0, "nested/file.txt").await.unwrap();
        assert!(result.diff.contains("-original"));
        assert!(result.diff.contains("+changed"));
        fs::write(repository.join("file.txt"), "original\n").unwrap();
        assert!(diff_file(&t.0, "nested/file.txt")
            .await
            .unwrap()
            .diff
            .is_empty());
    }
    #[test]
    fn diff_snapshots_are_private_and_removed() {
        let path;
        {
            let snapshot = DiffSnapshot::create("before", "after").unwrap();
            path = snapshot.0.clone();
            assert_eq!(fs::read_to_string(path.join("after")).unwrap(), "after");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
                assert_eq!(
                    fs::metadata(path.join("after"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        assert!(!path.exists());
    }
    #[tokio::test]
    async fn diff_outside_git_is_explicit() {
        let t = Temp::new();
        fs::write(t.0.join("x"), "hello").unwrap();
        assert!(!diff_file(&t.0, "x").await.unwrap().available);
    }
}
