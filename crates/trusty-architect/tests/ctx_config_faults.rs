//! #9171 A3a fix round: a bad ctx variable disables only its own sensor.
//!
//! Why: `fleet-poll.py` catches `ValueError` from `int(os.environ.get(...))`
//! inside `self_ctx_alerts` and `transcript_ctx_pct`, logs it, and keeps every
//! other sensor running. A startup abort would silence the whole poller for a
//! typo in one variable.
//! Test: this file; the golden mapping is `self_ctx_alerts_match_python_goldens`.

use std::fs;
use std::path::Path;

use trusty_architect::poller::{Config, TranscriptCtx, self_ctx_alerts};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn config(dir: &Path, vars: &[(&str, &str)]) -> Result<Config, trusty_architect::ConfigError> {
    let mut all: Vec<(String, String)> = [
        ("SUPERVISOR_INBOX_DIR", "inbox"),
        ("SESSIONS_DIR", "sessions"),
        ("TRANSCRIPT_BASE", "projects"),
        ("SELF_CTX_DIR", "self"),
    ]
    .iter()
    .map(|(k, sub)| {
        (
            (*k).to_owned(),
            dir.join(sub).to_string_lossy().into_owned(),
        )
    })
    .collect();
    all.extend(vars.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
    let get = move |k: &str| {
        all.iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    };
    Config::from_env(dir, dir, &get)
}

fn usage(tokens: u64) -> String {
    serde_json::json!({
        "type": "assistant", "entrypoint": "cli",
        "message": {"usage": {"input_tokens": tokens}}
    })
    .to_string()
        + "\n"
}

/// One self-ctx transcript at 40% of 1M, and a live PM pane `%1` whose own
/// transcript is at 50% of 1M.
fn world(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir.join("self"))?;
    fs::write(dir.join("self/t.jsonl"), usage(400_000))?;
    fs::create_dir_all(dir.join("sessions"))?;
    let session = serde_json::json!({
        "pid": std::process::id(), "sessionId": "sid1", "tmux": "tm:0.%1", "updatedAt": 1
    });
    fs::write(dir.join("sessions/a.json"), session.to_string())?;
    fs::create_dir_all(dir.join("projects/p"))?;
    fs::write(dir.join("projects/p/sid1.jsonl"), usage(500_000))
}

/// Bad values in the ctx variables build a config; the sensor that reads the
/// bad value is off and the other two sensors still run.
#[test]
fn a_bad_ctx_variable_disables_only_its_own_sensor() -> TestResult {
    let dir = tempfile::tempdir()?;
    world(dir.path())?;
    let pct = |cfg: &Config| TranscriptCtx::new().pct(cfg, "%1");

    // Healthy baseline: both sensors fire.
    let ok = config(dir.path(), &[])?;
    assert_eq!(self_ctx_alerts(&ok, 0.0)?.len(), 1);
    assert_eq!(pct(&ok)?, Some(50));

    // Python's int("") and int("lots") raise; the empty string is a set value.
    for bad in ["", "lots", "1.5"] {
        let cfg = config(dir.path(), &[("PANE_CTX_WINDOW", bad)])?;
        assert_eq!(pct(&cfg)?, None, "PANE_CTX_WINDOW={bad:?} disables ctx");
        assert_eq!(self_ctx_alerts(&cfg, 0.0)?.len(), 1, "self_ctx still runs");

        for var in ["SELF_CTX_WINDOW", "SELF_CTX_THRESHOLD"] {
            let cfg = config(dir.path(), &[(var, bad)])?;
            assert!(self_ctx_alerts(&cfg, 0.0)?.is_empty(), "{var}={bad:?}");
            assert_eq!(pct(&cfg)?, Some(50), "{var}={bad:?}: pane ctx still runs");
        }
    }
    Ok(())
}
