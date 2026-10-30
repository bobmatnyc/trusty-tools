//! The TUI `/connect` path refuses an Architect on an old Claude Code before
//! the prompt write, the daemon POST and the spawn (#10459).

use std::io::Read as _;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Sets `HOME` and `TMPDIR` for one `#[serial]` test, restoring both on drop.
struct EnvOverride(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvOverride {
    fn set(pairs: &[(&'static str, &std::path::Path)]) -> Self {
        let prev = pairs
            .iter()
            .map(|(key, value)| {
                let old = std::env::var_os(key);
                // SAFETY: the only caller is `#[serial]`.
                unsafe { std::env::set_var(key, value) };
                (*key, old)
            })
            .collect();
        Self(prev)
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        for (key, old) in self.0.drain(..) {
            // SAFETY: the only caller is `#[serial]`.
            match old {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

#[tokio::test]
#[serial_test::serial]
async fn connect_session_refuses_an_old_claude_code_before_any_write() {
    let scratch = tempfile::tempdir().expect("scratch");
    let root = scratch.path().canonicalize().expect("canonical");
    let (home, project, tmp) = (root.join("home"), root.join("project"), root.join("tmp"));
    for dir in [home.join(".trusty-mpm"), project.clone(), tmp.clone()] {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    std::fs::write(
        home.join(".trusty-mpm/config.toml"),
        format!(
            "[architect]\nprojects = [{:?}]\n",
            project.display().to_string()
        ),
    )
    .expect("grant");
    std::fs::write(
        project.join(".trusty-mpm.toml"),
        "profile = \"supervisor\"\n",
    )
    .expect("profile");

    // A daemon stand-in that only counts connections: any POST would connect.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            seen.fetch_add(1, Ordering::SeqCst);
            let _ = stream.read(&mut [0u8; 64]);
        }
    });

    let probe = &crate::core::architect_claude_version::TEST_PROBE;
    probe.with(|p| *p.borrow_mut() = Ok("2.1.294 (Claude Code)".to_owned()));
    let _env = EnvOverride::set(&[("HOME", &home), ("TMPDIR", &tmp)]);
    let client = super::super::DaemonClient::new(url);
    let result = client.connect_session(&project.to_string_lossy()).await;
    probe.with(|p| *p.borrow_mut() = Ok("2.1.312 (Claude Code)".to_owned()));

    let err = result.expect_err("an old Claude Code refuses the connect");
    assert!(err.to_string().contains("older than v2.1.295"), "{err}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "the refusal reached the daemon"
    );
    let written: Vec<_> = std::fs::read_dir(&tmp).expect("tmp").collect();
    assert!(written.is_empty(), "a prompt file was written: {written:?}");
    assert!(
        !project.join(".claude").exists(),
        "the connect wrote settings"
    );
}
