//! Stage the checkout's instructional content into a scratch `$HOME` (#9014).
//!
//! Why: since #9014 a spawned `tagent` reads its bundled roster and workflows
//! from instructional content — a checkout above its cwd, else the bundle
//! pinned under `<home>/.trusty-mpm/content`. A test that pins the child's cwd
//! and `$HOME` to tempdirs has neither, so this stages what
//! `tm content install` would have.
//! What: [`stage_repo_content`] writes a gzip tar of every file under the
//! checkout's `content/{agents,skills,instructions}` (bundle paths, plus
//! `bundle-manifest.toml`) as `content-v0.0.1.tar.gz` with a
//! `content-lock.toml` pinning its sha256. Mirrors trusty-mpm's
//! `tests/common::stage_repo_content`.
//! Test: `plain_cli_resolves_assistant_via_bundled_deploy_when_no_project_tier`.

use std::path::Path;

/// Install the checkout's content into `home` as a verified bundle; idempotent.
pub fn stage_repo_content(home: &Path) {
    use trusty_common::content::{ContentLock, LOCK_FILE_NAME};
    use trusty_common::integrity::Sha256Digest;

    const TAG: &str = "content-v0.0.1";
    let cache = home.join(".trusty-mpm").join("content");
    if cache.join(LOCK_FILE_NAME).exists() {
        return;
    }
    // #9298: the runtime checkout, never the compile-time build path.
    let repo = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    let mut entries: Vec<(String, Vec<u8>)> = vec![(
        "bundle-manifest.toml".to_string(),
        format!("tag = \"{TAG}\"\nschema_major = 1\n").into_bytes(),
    )];
    fn walk(dir: &Path, key: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).expect("content dir").flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let child = format!("{key}/{name}");
            if entry.path().is_dir() {
                walk(&entry.path(), &child, out);
            } else {
                out.push((child, std::fs::read(entry.path()).expect("content file")));
            }
        }
    }
    for class in ["agents", "skills", "instructions"] {
        walk(&repo.join("content").join(class), class, &mut entries);
    }
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    for (path, data) in &entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, path, data.as_slice())
            .expect("append");
    }
    let bytes = tar.into_inner().expect("tar").finish().expect("gzip");
    std::fs::create_dir_all(&cache).expect("content cache");
    let lock = ContentLock::new(TAG, Sha256Digest::of_bytes(&bytes)).expect("lock");
    std::fs::write(cache.join(lock.bundle_file_name()), &bytes).expect("bundle");
    lock.store(&cache.join(LOCK_FILE_NAME)).expect("store lock");
}
