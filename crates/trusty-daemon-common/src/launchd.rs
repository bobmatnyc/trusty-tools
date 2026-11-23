//! A per-user launchd job: the plist text, the `launchctl` seam, owned-file writes.
//!
//! Why: each broker must start at login and stay up, and its install verb must
//! never overwrite a file it did not write. Both products render the same
//! plist shape (`plistlib.dumps` output) and use the same ownership checks.
//! What: [`plist`] renders a KeepAlive/RunAtLoad job for any label and argv,
//! logs in the state dir, umask 077. [`Launchctl`] wraps `/bin/launchctl` so
//! tests never reach the real `gui/<uid>` domain; [`checked`] turns a non-zero
//! exit into [`LaunchdError::Launchctl`]. [`same_file`], [`write_owned`],
//! [`replace_owned`] and [`mkdir_0700`] publish files without clobbering.
//! Test: `plist_renders_plistlib_bytes`, `plist_refuses_control_characters`,
//! `write_owned_publishes_once_and_refuses_a_different_file`,
//! `checked_reports_launchctl_stderr`.

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

/// Why a launchd action failed.
#[derive(Debug, thiserror::Error)]
pub enum LaunchdError {
    /// A refusal or I/O error; the CLI prints the message.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// `launchctl` exited non-zero on a checked call.
    #[error("launchctl failed: {0}")]
    Launchctl(String),
}

/// One `launchctl` result.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// Exit status (0 = success).
    pub code: i32,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

/// Runs `/bin/launchctl <args>`; a seam so tests never touch launchd.
pub trait Launchctl {
    /// Run once and capture the result.
    fn run(&self, args: &[&str]) -> io::Result<Outcome>;
}

/// The real `/bin/launchctl`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemLaunchctl;

impl Launchctl for SystemLaunchctl {
    fn run(&self, args: &[&str]) -> io::Result<Outcome> {
        let out = std::process::Command::new("/bin/launchctl")
            .args(args)
            .output()?;
        Ok(Outcome {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Run `args` once; a non-zero exit is [`LaunchdError::Launchctl`] with its stderr.
pub fn checked(ctl: &dyn Launchctl, args: &[&str]) -> Result<(), LaunchdError> {
    let out = ctl.run(args)?;
    if out.code == 0 {
        Ok(())
    } else {
        Err(LaunchdError::Launchctl(out.stderr.trim().to_owned()))
    }
}

/// `plistlib` string escaping; control characters are refused as plistlib does.
pub fn escape(s: &str) -> io::Result<String> {
    if s.chars()
        .any(|c| (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err(io::Error::other(
            "strings can't contain control characters; use bytes instead",
        ));
    }
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    Ok(s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;"))
}

/// `plistlib.dumps` of the job `label` running `argv`, logging into `state`.
pub fn plist(label: &str, argv: &[&str], state: &Path) -> io::Result<String> {
    let mut args = String::new();
    for a in argv {
        args.push_str(&format!("\t\t<string>{}</string>\n", escape(a)?));
    }
    let label = escape(label)?;
    let err = escape(&state.join("service.stderr.log").to_string_lossy())?;
    let out = escape(&state.join("service.stdout.log").to_string_lossy())?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
\t<key>KeepAlive</key>\n\t<true/>\n\
\t<key>Label</key>\n\t<string>{label}</string>\n\
\t<key>ProgramArguments</key>\n\t<array>\n{args}\t</array>\n\
\t<key>RunAtLoad</key>\n\t<true/>\n\
\t<key>StandardErrorPath</key>\n\t<string>{err}</string>\n\
\t<key>StandardOutPath</key>\n\t<string>{out}</string>\n\
\t<key>Umask</key>\n\t<integer>63</integer>\n\
</dict>\n</plist>\n"
    ))
}

/// True when `path` is a regular, user-owned file with exactly `mode` and `content`.
pub fn same_file(path: &Path, content: &str, mode: u32) -> bool {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            m.is_file()
                && m.uid() == crate::private::uid()
                && m.mode() & 0o777 == mode
                && fs::read(path).is_ok_and(|b| b == content.as_bytes())
        }
        Err(_) => false,
    }
}

/// Publish a complete file once; an identical owned file is a no-op.
///
/// What: writes a temp file with `mode` in the same directory and hard-links
/// it into place, so a reader never sees a partial file and an existing
/// file is never replaced; an existing different file is `existing file differs`.
pub fn write_owned(path: &Path, content: &str, mode: u32) -> io::Result<()> {
    let mut tmp = temp_beside(path, content, mode)?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    match fs::hard_link(tmp.path(), path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if same_file(path, content, mode) {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("existing file differs: {}", path.display()),
                ))
            }
        }
        Err(e) => Err(e),
    }
}

/// Replace `path` with `content` atomically (temp file + rename).
///
/// Only for a file the caller has already proven its own.
pub fn replace_owned(path: &Path, content: &str, mode: u32) -> io::Result<()> {
    let tmp = temp_beside(path, content, mode)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn temp_beside(path: &Path, content: &str, mode: u32) -> io::Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("registration path has no parent"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    tmp.write_all(content.as_bytes())?;
    Ok(tmp)
}

/// `mkdir(mode=0o700, parents=True, exist_ok=True)`: parents default mode, the leaf 0700.
pub fn mkdir_0700(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_renders_plistlib_bytes() {
        let text = plist("com.trusty.x", &["/bin/x", "a&<>"], Path::new("/s")).expect("plist");
        assert!(text.contains("\t<key>Label</key>\n\t<string>com.trusty.x</string>\n"));
        assert!(
            text.contains("\t\t<string>/bin/x</string>\n\t\t<string>a&amp;&lt;&gt;</string>\n")
        );
        assert!(text.contains("<string>/s/service.stderr.log</string>"));
        assert!(text.ends_with("\t<key>Umask</key>\n\t<integer>63</integer>\n</dict>\n</plist>\n"));
    }

    #[test]
    fn plist_refuses_control_characters() {
        assert!(plist("l", &["a\u{1}"], Path::new("/s")).is_err());
        assert_eq!(escape("a\r\nb\rc").expect("escape"), "a\nb\nc");
    }

    #[test]
    fn write_owned_publishes_once_and_refuses_a_different_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("agents");
        mkdir_0700(&dir).expect("mkdir");
        mkdir_0700(&dir).expect("mkdir twice");
        let path = dir.join("job.plist");
        write_owned(&path, "one", 0o600).expect("write");
        assert!(same_file(&path, "one", 0o600));
        write_owned(&path, "one", 0o600).expect("identical is a no-op");
        let err = write_owned(&path, "two", 0o600).expect_err("differs");
        assert!(err.to_string().contains("existing file differs"));
        replace_owned(&path, "two", 0o600).expect("replace");
        assert!(same_file(&path, "two", 0o600));
        assert!(!same_file(&path, "two", 0o644));
    }

    struct Fixed(i32);

    impl Launchctl for Fixed {
        fn run(&self, _args: &[&str]) -> io::Result<Outcome> {
            Ok(Outcome {
                code: self.0,
                stdout: String::new(),
                stderr: " boom \n".to_owned(),
            })
        }
    }

    #[test]
    fn checked_reports_launchctl_stderr() {
        checked(&Fixed(0), &["print"]).expect("ok");
        let err = checked(&Fixed(5), &["print"]).expect_err("failed");
        assert_eq!(err.to_string(), "launchctl failed: boom");
    }
}
