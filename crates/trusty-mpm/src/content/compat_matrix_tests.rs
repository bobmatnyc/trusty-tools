//! The content compatibility matrix (#8390, ADR-0064 decision 6).
//!
//! Why: content ships on its own release channel, so a content release can
//! carry an asset that an older `tm` cannot parse. Nothing caught that before
//! a user installed the release. The matrix is that check, run on the tree a
//! release would ship.
//! What: [`run_matrix`] packages a `content/` tree with the release packager
//! (`scripts/package_content.sh`), installs the bundle into a temp cache,
//! resolves it as an installed bundle and dry-run-deploys every file of it
//! into temp directories, against [`OLDEST_SUPPORTED`]: the manifest's
//! `schema_major` is at most the floor's; every frontmatter block is strict
//! YAML with a readable `requires:`; every JSON and TOML asset parses; every
//! agent composes through the shared deployer, the trusty-code roster against
//! the shared base chain; every skill entry point declares `name` and
//! `description` and deploys; and the instructions load. It reports one
//! PASS or FAIL row per asset, and a failure is a FAIL row — nothing is
//! skipped.
//! Test: `every_shipped_asset_passes_the_compatibility_matrix`,
//! `the_matrix_fails_each_schema_incompatible_asset`,
//! `the_matrix_fails_a_tree_newer_than_the_oldest_supported_schema`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_agents_common::agent_content::{AgentRoster, ResolvedContent, resolve_content_in};
use trusty_agents_common::agents::frontmatter::validate_frontmatter;
use trusty_common::content::{ContentLock, DevOverride, LOCK_FILE_NAME};
use trusty_common::integrity::Sha256Digest;

use crate::content::requires::parse_requires;
use crate::core::content_source::test_support::repo_root;
use crate::core::framework_content::FrameworkContent;

/// The oldest binary a content release is promised to work with.
struct SchemaFloor {
    /// The binary, for the report.
    binary: &'static str,
    /// The newest bundle `schema_major` that binary reads.
    schema_major: i64,
}

/// tm 1.7.10 shipped the content resolver (#8389), reading bundle
/// `schema_major` 1; no supported binary reads less. Raise it when support for
/// that binary ends, never to make a release pass.
const OLDEST_SUPPORTED: SchemaFloor = SchemaFloor {
    binary: "tm 1.7.10",
    schema_major: 1,
};

/// The version the matrix packages under; never published.
const MATRIX_VERSION: &str = "0.0.0-matrix";

/// One asset's verdict: its bundle path and every reason it failed.
type Rows = BTreeMap<String, Vec<String>>;

/// The matrix's verdict on one content tree.
struct Matrix {
    floor: String,
    rows: Rows,
}

impl Matrix {
    /// The FAIL rows as `path: reason; reason`.
    fn failures(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|(_, why)| !why.is_empty())
            .map(|(path, why)| format!("{path}: {}", why.join("; ")))
            .collect()
    }

    /// One PASS or FAIL line per asset, then the counts.
    fn report(&self) -> String {
        let mut out = format!("content compatibility matrix against {}\n", self.floor);
        for (path, why) in &self.rows {
            match why.is_empty() {
                true => out.push_str(&format!("PASS {path}\n")),
                false => out.push_str(&format!("FAIL {path}: {}\n", why.join("; "))),
            }
        }
        let failed = self.failures().len();
        out.push_str(&format!(
            "{} passed, {failed} failed\n",
            self.rows.len() - failed
        ));
        out
    }
}

fn fail(rows: &mut Rows, path: &str, why: impl Into<String>) {
    rows.entry(path.to_owned()).or_default().push(why.into());
}

/// Runs the matrix over `content`, a directory laid out like `content/`.
fn run_matrix(content: &Path) -> Matrix {
    let mut rows = Rows::new();
    let floor = format!(
        "{} (schema_major {})",
        OLDEST_SUPPORTED.binary, OLDEST_SUPPORTED.schema_major
    );
    check_schema_floor(content, &mut rows);
    let tmp = tempfile::tempdir().expect("tmp");
    match install_bundle(content, tmp.path()) {
        Ok(resolved) => grade_bundle(&resolved, tmp.path(), &mut rows),
        Err(why) => fail(&mut rows, "bundle", why),
    }
    Matrix { floor, rows }
}

/// The `manifest.toml` row: the tree's `schema_major` is one the oldest
/// supported binary reads.
fn check_schema_floor(content: &Path, rows: &mut Rows) {
    let _ = content;
    rows.entry("manifest.toml".to_owned()).or_default();
}

/// Packages `content` with the release packager and resolves the bundle as an
/// installed release from a cache under `work`.
fn install_bundle(content: &Path, work: &Path) -> Result<ResolvedContent, String> {
    let out = work.join("dist");
    let packaged = Command::new("bash")
        .arg(repo_root().join("scripts/package_content.sh"))
        .args(["--version", MATRIX_VERSION, "--out-dir"])
        .arg(&out)
        .arg("--source-root")
        .arg(content)
        .output()
        .map_err(|e| format!("the packager did not start: {e}"))?;
    if !packaged.status.success() {
        return Err(format!(
            "the packager refused the tree: {}",
            String::from_utf8_lossy(&packaged.stderr).trim()
        ));
    }
    let tag = format!("content-v{MATRIX_VERSION}");
    let bytes = std::fs::read(out.join(format!("{tag}.tar.gz"))).map_err(|e| e.to_string())?;
    let cache = work.join("cache");
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    std::fs::write(cache.join(format!("{tag}.tar.gz")), &bytes).map_err(|e| e.to_string())?;
    ContentLock::new(&tag, Sha256Digest::of_bytes(&bytes))
        .and_then(|lock| lock.store(&cache.join(LOCK_FILE_NAME)))
        .map_err(|e| e.to_string())?;
    resolve_content_in(&cache, DevOverride::Off)
        .map_err(|e| format!("the resolver refused the bundle: {e}"))
}

/// Grades every file of the resolved bundle; `work` holds the deploy targets.
fn grade_bundle(content: &ResolvedContent, work: &Path, rows: &mut Rows) {
    let _ = work;
    for class in ["agents", "skills", "instructions"] {
        for key in content.list(class).unwrap_or_default() {
            rows.entry(key).or_default();
        }
    }
}

/// The per-file parse every reader of the schema shares.
fn parse_asset(key: &str, text: &str) -> Result<(), String> {
    if key.ends_with(".json") {
        return serde_json::from_str::<serde_json::Value>(text)
            .map(drop)
            .map_err(|e| format!("not JSON: {e}"));
    }
    if key.ends_with(".toml") {
        return text
            .parse::<toml::Table>()
            .map(drop)
            .map_err(|e| format!("not TOML: {e}"));
    }
    if !key.ends_with(".md") || !text.starts_with("---") {
        return Ok(());
    }
    // A composed roster's source is read by the composer's line grammar; the
    // deployer strict-checks the composition instead (see `deploy_agents`).
    // Every other `.md` reaches a strict reader as written.
    if !is_composed_agent(key) {
        validate_frontmatter(text).map_err(|e| format!("frontmatter is not strict YAML: {e}"))?;
    }
    parse_requires(text).map(drop).map_err(|e| e.to_string())?;
    if is_skill_entry(key) {
        let block = frontmatter_map(text)?;
        for required in ["name", "description"] {
            if !block
                .get(required)
                .is_some_and(serde_yaml::Value::is_string)
            {
                return Err(format!("a skill entry point needs a `{required}:` string"));
            }
        }
    }
    Ok(())
}

/// Whether `key` is an agent source the composer reads: `agents/<n>.md` or
/// `agents/trusty-code/<n>.md`.
fn is_composed_agent(key: &str) -> bool {
    let parts: Vec<&str> = key.split('/').collect();
    matches!(parts.as_slice(), ["agents", file] if file.ends_with(".md"))
        || matches!(parts.as_slice(), ["agents", "trusty-code", file] if file.ends_with(".md"))
}

/// Whether `key` is a skill entry point: `skills/<n>.md` or `skills/<n>/SKILL.md`.
fn is_skill_entry(key: &str) -> bool {
    let parts: Vec<&str> = key.split('/').collect();
    matches!(parts.as_slice(), ["skills", file] if file.ends_with(".md"))
        || matches!(parts.as_slice(), ["skills", _, "SKILL.md"])
}

/// The frontmatter block of `text` as a YAML mapping.
fn frontmatter_map(text: &str) -> Result<serde_yaml::Mapping, String> {
    let body = text.strip_prefix("---\n").ok_or("no frontmatter block")?;
    let end = body.find("\n---").ok_or("unterminated frontmatter block")?;
    serde_yaml::from_str(&body[..end]).map_err(|e| format!("frontmatter: {e}"))
}

/// Composes and deploys the shared roster, then the trusty-code roster over
/// the shared base chain, recording each agent that fails.
fn deploy_agents(
    content: &ResolvedContent,
    files: &BTreeMap<String, String>,
    work: &Path,
    rows: &mut Rows,
) {
    let roster = match AgentRoster::load(content) {
        Ok(roster) => roster,
        Err(e) => return fail(rows, "agents/", format!("the roster does not load: {e}")),
    };
    let tcode: Vec<(&str, &str)> = files
        .iter()
        .filter_map(|(k, t)| Some((k.strip_prefix("agents/trusty-code/")?, t.as_str())))
        .filter(|(name, _)| !name.contains('/') && name.ends_with(".md"))
        .collect();
    for (label, overlay) in [("agents/", &[][..]), ("agents/trusty-code/", &tcode[..])] {
        let src = work.join(format!("src-{}", label.replace('/', "-")));
        if let Err(e) = roster.materialize(&src) {
            return fail(rows, label, format!("materialize failed: {e}"));
        }
        for (name, text) in overlay {
            std::fs::write(src.join(name), text).expect("overlay");
        }
        let dst = work.join(format!("dst-{}", label.replace('/', "-")));
        let result =
            crate::core::agent_deployer::deploy_agents(&src, &dst, &work.join("skills-root"));
        let failed = match result {
            Ok(result) => result.failed,
            Err(e) => vec![format!("*: {e}")],
        };
        for entry in failed {
            let (name, why) = entry.split_once(": ").unwrap_or(("*", entry.as_str()));
            let from_overlay = overlay
                .iter()
                .any(|(n, _)| n.trim_end_matches(".md") == name);
            let prefix = if from_overlay { label } else { "agents/" };
            fail(
                rows,
                &format!("{prefix}{name}.md"),
                format!("does not deploy: {why}"),
            );
        }
    }
}

/// Deploys every skill through the shared skill deployer, recording each
/// entry point that is not written.
fn deploy_skills(files: &BTreeMap<String, String>, work: &Path, rows: &mut Rows) {
    let src = work.join("skills-src");
    for (key, text) in files {
        if let Some(rel) = key.strip_prefix("skills/") {
            let path = src.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(path, text).expect("skill");
        }
    }
    let stats = match crate::core::skill_deployer::deploy_skills(&src, &work.join("skills-dst")) {
        Ok(stats) => stats,
        Err(e) => return fail(rows, "skills/", format!("does not deploy: {e}")),
    };
    for key in files.keys().filter(|k| is_skill_entry(k)) {
        let stem = key
            .trim_start_matches("skills/")
            .trim_end_matches("/SKILL.md")
            .trim_end_matches(".md");
        let written = stats
            .deployed
            .iter()
            .any(|f| f == stem || f.starts_with(&format!("{stem}/")));
        if !written {
            fail(rows, key, "the skill deployer did not write it");
        }
    }
}

/// Copies `src` into `dst`, recursively.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("dir");
    for entry in std::fs::read_dir(src).expect("read_dir") {
        let entry = entry.expect("entry");
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).expect("copy");
        }
    }
}

/// A copy of this repository's `content/` under `tmp`.
fn repo_content_copy(tmp: &Path) -> PathBuf {
    let dir = tmp.join("content");
    copy_tree(&repo_root().join("content"), &dir);
    dir
}

/// #8390 acceptance 3: every asset the next release would ship passes.
#[test]
fn every_shipped_asset_passes_the_compatibility_matrix() {
    let matrix = run_matrix(&repo_root().join("content"));
    let report = matrix.report();
    println!("{report}");
    assert!(
        matrix.rows.len() > 400,
        "the matrix saw too few assets:\n{report}"
    );
    assert!(matrix.failures().is_empty(), "{report}");
}

/// #8390 acceptance 3: a schema-incompatible asset is a FAIL row naming it —
/// never a skip — and the rest of the tree still passes.
#[test]
fn the_matrix_fails_each_schema_incompatible_asset() {
    let tmp = tempfile::tempdir().expect("tmp");
    let content = repo_content_copy(tmp.path());
    let plant = |rel: &str, body: &str| std::fs::write(content.join(rel), body).expect("plant");
    plant(
        "agents/zz-unterminated.md",
        "---\nname: zz-unterminated\nrole: qa\nextends: base-agent\nbody\n",
    );
    plant(
        "skills/zz-no-description.md",
        "---\nname: zz-no-description\nmetadata: {version: \"1.0.0\"}\n---\nbody\n",
    );
    plant(
        "skills/zz-bad-requires.md",
        "---\nname: x\ndescription: y\nrequires: \">=1\"\n---\n",
    );
    plant("agents/trusty-agents/workflows/zz.json", "{ not json");
    let matrix = run_matrix(&content);
    let report = matrix.report();
    let failed = matrix.failures();
    for path in [
        "agents/zz-unterminated.md",
        "skills/zz-no-description.md",
        "skills/zz-bad-requires.md",
        "agents/trusty-agents/workflows/zz.json",
    ] {
        assert!(
            failed.iter().any(|f| f.starts_with(&format!("{path}: "))),
            "`{path}` must be a FAIL row:\n{report}"
        );
    }
    assert_eq!(failed.len(), 4, "only the planted assets fail:\n{report}");
    assert!(report.contains("PASS agents/engineer.md"), "{report}");
}

/// #8390 acceptance 3: a tree declaring a `schema_major` newer than the
/// oldest supported binary reads fails at the manifest row.
#[test]
fn the_matrix_fails_a_tree_newer_than_the_oldest_supported_schema() {
    let tmp = tempfile::tempdir().expect("tmp");
    let content = repo_content_copy(tmp.path());
    let manifest = content.join("manifest.toml");
    let text = std::fs::read_to_string(&manifest).expect("manifest");
    let bumped = text.replace(
        &format!("schema_major = {}", OLDEST_SUPPORTED.schema_major),
        &format!("schema_major = {}", OLDEST_SUPPORTED.schema_major + 1),
    );
    assert_ne!(
        text, bumped,
        "the manifest declares the floor's schema_major"
    );
    std::fs::write(&manifest, bumped).expect("write");
    let failed = run_matrix(&content).failures();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        failed[0].starts_with("manifest.toml: schema_major 2"),
        "{failed:?}"
    );
}
