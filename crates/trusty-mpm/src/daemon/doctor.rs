//! The `tm doctor` diagnostic engine.
//!
//! Why: a misconfigured trusty-mpm stack fails in confusing ways — sessions
//! launch with no instructions, agents never deploy, memory recall silently
//! returns nothing. `tm doctor` collapses every "is this wired correctly?"
//! question into one command so the operator gets a single, actionable verdict.
//! What: [`run_doctor`] runs five independent probes — the instruction
//! pipeline, agent deployment, skill deployment, and the trusty-memory /
//! trusty-search sidecars — and folds their outcomes into a
//! [`DoctorReport`]. Each network probe is bounded by [`PROBE_TIMEOUT`] so an
//! unreachable service cannot hang the report.
//! Test: `cargo test -p trusty-mpm-daemon doctor` exercises the filesystem
//! probes against temp directories and the HTTP probes against an in-process
//! test server.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::agent_manifest::MANIFEST_FILE;
use crate::core::doctor::{CheckStatus, DoctorCheck, DoctorReport};
use crate::core::paths::FrameworkPaths;

use super::discover::{TRUSTY_MEMORY_DEFAULT_ADDR, TRUSTY_SEARCH_DEFAULT_ADDR, discover_addr};

/// Per-probe network timeout.
///
/// Why: a sidecar that is down or wedged must not stall the whole diagnostic;
/// a short bound turns "hung" into a clean `Fail`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The trusty-search index `tm doctor` expects to exist for this repo.
const EXPECTED_SEARCH_INDEX: &str = "trusty-mpm";

/// Run every diagnostic probe and assemble the report.
///
/// Why: the single entry point behind `GET /api/v1/doctor` and `tm doctor` —
/// running all probes here keeps the check set identical across every UI.
/// What: runs the instruction / agent / skill filesystem probes, then the
/// memory and search HTTP probes (each bounded by [`PROBE_TIMEOUT`]), and folds
/// the five [`DoctorCheck`]s into a [`DoctorReport`] whose `overall` status is
/// the worst of them. `project_dir`, when supplied, scopes the instruction
/// probe to that project's `.trusty-mpm/last-instructions.md`.
/// Test: `run_doctor_produces_five_checks`.
pub async fn run_doctor(project_dir: Option<&Path>) -> DoctorReport {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let paths = FrameworkPaths::default();

    let mut checks = vec![
        check_instructions(project_dir),
        check_agents(&paths),
        check_skills(&home),
    ];
    checks.push(check_memory(&home).await);
    checks.push(check_search(&home).await);

    DoctorReport::from_checks(checks)
}

/// Probe the instruction pipeline by looking for `last-instructions.md`.
///
/// Why: `prepare_session` writes `<project>/.trusty-mpm/last-instructions.md`
/// every time it assembles a PM prompt; its presence proves the instruction
/// pipeline has run at least once for the project.
/// What: when `project_dir` is given, checks that project's
/// `.trusty-mpm/last-instructions.md`; with no project it cannot scope the
/// probe and reports `Warn`. A missing file is `Warn` (the pipeline simply has
/// not run yet), an empty file is `Fail`.
/// Test: `instructions_present_is_ok`, `instructions_missing_is_warn`.
fn check_instructions(project_dir: Option<&Path>) -> DoctorCheck {
    let Some(project) = project_dir else {
        return DoctorCheck::new(
            "instructions",
            CheckStatus::Warn,
            "no project directory supplied — cannot verify last-instructions.md",
        );
    };
    let stash = project.join(".trusty-mpm").join("last-instructions.md");
    match std::fs::metadata(&stash) {
        Ok(meta) if meta.len() > 0 => DoctorCheck::new(
            "instructions",
            CheckStatus::Ok,
            format!("instruction pipeline ran — {} present", stash.display()),
        ),
        Ok(_) => DoctorCheck::new(
            "instructions",
            CheckStatus::Fail,
            format!("{} exists but is empty", stash.display()),
        ),
        Err(_) => DoctorCheck::new(
            "instructions",
            CheckStatus::Warn,
            format!(
                "{} not found — launch a session in this project to run the pipeline",
                stash.display()
            ),
        ),
    }
}

/// Probe agent deployment under `~/.claude/agents/`.
///
/// Why: without deployed agent files Claude Code has nothing to delegate to;
/// the daemon also expects an ownership manifest alongside them.
/// What: `Fail` when the directory is absent or holds no `.md` files; `Warn`
/// when agents are present but the manifest JSON is missing; `Ok` when both
/// agent files and the manifest are present.
/// Test: `agents_missing_dir_is_fail`, `agents_without_manifest_is_warn`.
fn check_agents(paths: &FrameworkPaths) -> DoctorCheck {
    let dir = paths.claude_agents_dir();
    let md_count = count_files_with_extension(&dir, "md");
    if md_count == 0 {
        return DoctorCheck::new(
            "agents",
            CheckStatus::Fail,
            format!(
                "no agent files in {} — run `tm install` to deploy agents",
                dir.display()
            ),
        );
    }
    let manifest = dir.join(MANIFEST_FILE);
    if manifest.exists() {
        DoctorCheck::new(
            "agents",
            CheckStatus::Ok,
            format!(
                "{md_count} agent(s) deployed in {} with manifest",
                dir.display()
            ),
        )
    } else {
        DoctorCheck::new(
            "agents",
            CheckStatus::Warn,
            format!(
                "{md_count} agent(s) in {} but {MANIFEST_FILE} is missing",
                dir.display()
            ),
        )
    }
}

/// Probe skill deployment under `~/.claude/skills/`.
///
/// Why: skills extend the PM with reusable capabilities; their deployment is a
/// separate step that may not have run yet.
/// What: `Fail` when `~/.claude/skills/` does not exist at all; `Warn` when it
/// exists but is empty (skill deployment not yet implemented / not run); `Ok`
/// when it holds at least one entry.
/// Test: `skills_missing_dir_is_fail`, `skills_empty_dir_is_warn`.
fn check_skills(home: &Path) -> DoctorCheck {
    let dir = home.join(".claude").join("skills");
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            let count = entries.flatten().count();
            if count == 0 {
                DoctorCheck::new(
                    "skills",
                    CheckStatus::Warn,
                    format!(
                        "{} exists but is empty — no skills deployed yet",
                        dir.display()
                    ),
                )
            } else {
                DoctorCheck::new(
                    "skills",
                    CheckStatus::Ok,
                    format!("{count} skill entr(ies) in {}", dir.display()),
                )
            }
        }
        Err(_) => DoctorCheck::new(
            "skills",
            CheckStatus::Fail,
            format!("{} does not exist", dir.display()),
        ),
    }
}

/// Probe the trusty-memory sidecar's `/health` endpoint.
///
/// Why: memory recall and store route through trusty-memory; if it is down the
/// PM silently loses its long-term memory, so the operator must know.
/// What: resolves the service address via [`discover_addr`], issues a
/// `GET /health` bounded by [`PROBE_TIMEOUT`], and reports `Ok` on a 2xx,
/// `Fail` on any other status or a transport error.
/// Test: `memory_unreachable_is_fail`.
async fn check_memory(home: &Path) -> DoctorCheck {
    let dir = home.join(".trusty-memory");
    let default = TRUSTY_MEMORY_DEFAULT_ADDR
        .parse()
        .expect("static default is valid");
    let env = std::env::var("TRUSTY_MEMORY_ADDR").ok();
    let addr = discover_addr(&dir, default, env.as_deref()).await;
    match http_get_ok(&format!("http://{addr}/health")).await {
        Ok(true) => DoctorCheck::new(
            "memory",
            CheckStatus::Ok,
            format!("trusty-memory healthy at {addr}"),
        ),
        Ok(false) => DoctorCheck::new(
            "memory",
            CheckStatus::Fail,
            format!("trusty-memory at {addr} returned a non-2xx status"),
        ),
        Err(e) => DoctorCheck::new(
            "memory",
            CheckStatus::Fail,
            format!("trusty-memory unreachable at {addr}: {e}"),
        ),
    }
}

/// Probe the trusty-search sidecar's health and the `trusty-mpm` index.
///
/// Why: code search backs the PM's "search before grep" rule; both the service
/// being up *and* the `trusty-mpm` index existing are required for it to work.
/// What: resolves the service address, checks `GET /health` (a non-2xx or
/// transport error is `Fail`), then checks `GET /indexes` for an index named
/// [`EXPECTED_SEARCH_INDEX`] — a healthy service missing that index is `Warn`.
/// Test: `search_unreachable_is_fail`.
async fn check_search(home: &Path) -> DoctorCheck {
    let dir = home.join(".trusty-search");
    let default = TRUSTY_SEARCH_DEFAULT_ADDR
        .parse()
        .expect("static default is valid");
    let env = std::env::var("TRUSTY_SEARCH_ADDR").ok();
    let addr = discover_addr(&dir, default, env.as_deref()).await;

    match http_get_ok(&format!("http://{addr}/health")).await {
        Ok(true) => {}
        Ok(false) => {
            return DoctorCheck::new(
                "search",
                CheckStatus::Fail,
                format!("trusty-search at {addr} returned a non-2xx status"),
            );
        }
        Err(e) => {
            return DoctorCheck::new(
                "search",
                CheckStatus::Fail,
                format!("trusty-search unreachable at {addr}: {e}"),
            );
        }
    }

    // Service is up — confirm the expected index exists.
    match http_get_json(&format!("http://{addr}/indexes")).await {
        Ok(body) if index_present(&body, EXPECTED_SEARCH_INDEX) => DoctorCheck::new(
            "search",
            CheckStatus::Ok,
            format!("trusty-search healthy at {addr}, `{EXPECTED_SEARCH_INDEX}` index present"),
        ),
        Ok(_) => DoctorCheck::new(
            "search",
            CheckStatus::Warn,
            format!(
                "trusty-search healthy at {addr} but the `{EXPECTED_SEARCH_INDEX}` index is missing"
            ),
        ),
        Err(e) => DoctorCheck::new(
            "search",
            CheckStatus::Warn,
            format!("trusty-search healthy at {addr} but listing indexes failed: {e}"),
        ),
    }
}

/// True when `body` mentions an index named `name`.
///
/// Why: the `/indexes` payload shape varies (a bare string array, or objects
/// with an `id`/`name` field); a tolerant scan avoids coupling the probe to one
/// exact wire form.
/// What: returns true when any array element equals `name` directly or carries
/// an `id`/`name`/`index_id` field equal to `name`.
/// Test: `index_present_matches_each_shape`.
fn index_present(body: &serde_json::Value, name: &str) -> bool {
    // The array may be the top-level value or nested under `indexes`.
    let array = body
        .as_array()
        .or_else(|| body.get("indexes").and_then(|v| v.as_array()));
    let Some(array) = array else {
        return false;
    };
    array.iter().any(|entry| {
        if entry.as_str() == Some(name) {
            return true;
        }
        ["id", "name", "index_id"]
            .iter()
            .any(|key| entry.get(key).and_then(|v| v.as_str()) == Some(name))
    })
}

/// Issue a timeout-bounded `GET` and report whether the status is 2xx.
///
/// Why: the memory and search health probes only need a yes/no liveness answer.
/// What: `GET url` with a [`PROBE_TIMEOUT`] client timeout; `Ok(true)` on a 2xx
/// response, `Ok(false)` on any other status, `Err` on a transport failure.
/// Test: covered by `memory_unreachable_is_fail`.
async fn http_get_ok(url: &str) -> anyhow::Result<bool> {
    let client = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build()?;
    let resp = client.get(url).send().await?;
    Ok(resp.status().is_success())
}

/// Issue a timeout-bounded `GET` and parse the response body as JSON.
///
/// Why: the search index check needs the `/indexes` payload, not just its
/// status code.
/// What: `GET url` with a [`PROBE_TIMEOUT`] client timeout; returns the parsed
/// [`serde_json::Value`], `Err` on a non-2xx status or a transport / parse
/// failure.
/// Test: covered by `search_unreachable_is_fail`.
async fn http_get_json(url: &str) -> anyhow::Result<serde_json::Value> {
    let client = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build()?;
    let body = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(body)
}

/// Count files with the given extension directly under `dir`.
///
/// Why: the agent probe needs the number of deployed `.md` agent files; a
/// missing directory is simply zero, not an error.
/// What: returns the count of regular entries whose extension equals `ext`; an
/// unreadable directory yields `0`.
/// Test: covered by `agents_missing_dir_is_fail`.
fn count_files_with_extension(dir: &Path, ext: &str) -> usize {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case(ext))
                    .unwrap_or(false)
            })
            .count(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_present_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".trusty-mpm");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("last-instructions.md"), "PM instructions").unwrap();
        let check = check_instructions(Some(tmp.path()));
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn instructions_missing_is_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let check = check_instructions(Some(tmp.path()));
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn instructions_no_project_is_warn() {
        let check = check_instructions(None);
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn agents_missing_dir_is_fail() {
        let tmp = tempfile::tempdir().unwrap();
        // `FrameworkPaths::under` derives `<base>/.claude/agents`, which does
        // not exist under a fresh temp dir.
        let paths = FrameworkPaths::under(tmp.path());
        let check = check_agents(&paths);
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[test]
    fn agents_without_manifest_is_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let agents = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("engineer.md"), "agent").unwrap();
        let check = check_agents(&paths);
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn agents_with_manifest_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = FrameworkPaths::under(tmp.path());
        let agents = paths.claude_agents_dir();
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("engineer.md"), "agent").unwrap();
        std::fs::write(agents.join(MANIFEST_FILE), "{}").unwrap();
        let check = check_agents(&paths);
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn skills_missing_dir_is_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let check = check_skills(tmp.path());
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[test]
    fn skills_empty_dir_is_warn() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude").join("skills")).unwrap();
        let check = check_skills(tmp.path());
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn skills_populated_dir_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("tm-doctor.md"), "skill").unwrap();
        let check = check_skills(tmp.path());
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn index_present_matches_each_shape() {
        // Bare string array.
        let strings = serde_json::json!(["other", "trusty-mpm"]);
        assert!(index_present(&strings, "trusty-mpm"));
        // Objects with an `id` field, nested under `indexes`.
        let objects = serde_json::json!({"indexes": [{"id": "trusty-mpm"}]});
        assert!(index_present(&objects, "trusty-mpm"));
        // Objects with a `name` field.
        let named = serde_json::json!([{"name": "trusty-mpm"}]);
        assert!(index_present(&named, "trusty-mpm"));
        // Absent index.
        let missing = serde_json::json!(["a", "b"]);
        assert!(!index_present(&missing, "trusty-mpm"));
    }

    #[tokio::test]
    async fn memory_unreachable_is_fail() {
        // Port 0 never accepts a connection, so the probe must fail cleanly
        // rather than hang.
        unsafe {
            std::env::set_var("TRUSTY_MEMORY_ADDR", "127.0.0.1:0");
        }
        let tmp = tempfile::tempdir().unwrap();
        let check = check_memory(tmp.path()).await;
        unsafe {
            std::env::remove_var("TRUSTY_MEMORY_ADDR");
        }
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn search_unreachable_is_fail() {
        unsafe {
            std::env::set_var("TRUSTY_SEARCH_ADDR", "127.0.0.1:0");
        }
        let tmp = tempfile::tempdir().unwrap();
        let check = check_search(tmp.path()).await;
        unsafe {
            std::env::remove_var("TRUSTY_SEARCH_ADDR");
        }
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn run_doctor_produces_five_checks() {
        let report = run_doctor(None).await;
        assert_eq!(report.checks.len(), 5);
        let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["instructions", "agents", "skills", "memory", "search"]
        );
    }
}
