//! The opt-in `gcloud_auth` row of `tm doctor --network` (#8371).
//!
//! Why: a `gcp-ops` agent stopped when every configured `gcloud` account
//! needed interactive reauth, and nothing told the operator which step
//! unblocks it. "No credentials" and "credentials present, but every refresh
//! token needs an interactive login" need different operator steps, and
//! doctor reported neither. Minting a token calls Google, so the row is
//! OPT-IN (owner ruling 4d): plain `tm doctor` makes no network call and never
//! spawns `gcloud`.
//! What: `gcloud auth list --format=json` names the credentialed accounts;
//! `gcloud auth print-access-token --account=<a>` then tries to mint a token,
//! the ACTIVE account first, at most [`MAX_PROBED`] accounts, each call
//! bounded by [`CALL_BUDGET`]. A missing binary, a timeout, a non-zero exit or
//! unparseable output is a WARN or FAIL naming the reason, never `Ok`. The
//! minted token is captured and dropped: no token text, and no account local
//! part, reaches the row. Accounts render as `b***@example.com`.
//! Test: `doctor_gcloud_tests.rs`; `tests/tm_doctor_network.rs` drives the real
//! binary against a `gcloud` shim on `PATH`.

use std::io::ErrorKind;
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use trusty_mpm::core::bounded_proc::{BoundedError, run_bounded};
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

/// This check's name.
pub(crate) const CHECK: &str = "gcloud_auth";

/// Ceiling on one `gcloud` call. A token refresh is one HTTPS round trip; ten
/// seconds covers a slow network without letting doctor hang.
pub(crate) const CALL_BUDGET: Duration = Duration::from_secs(10);

/// Most accounts whose token mint is attempted, active first. Bounds the row
/// at `(1 + MAX_PROBED) * CALL_BUDGET`.
pub(crate) const MAX_PROBED: usize = 3;

/// The row a run without `--network` prints.
pub(crate) const SKIPPED: &str =
    "skipped (needs --network): `tm doctor --network` checks gcloud auth by minting a token";

/// How one `gcloud` invocation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Run {
    /// The child exited. `code` is `None` when a signal killed it.
    Exited {
        success: bool,
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    /// No `gcloud` binary on `PATH`.
    NotFound,
    /// The call outlived [`CALL_BUDGET`] and was killed.
    TimedOut,
    /// Any other spawn or wait failure.
    Error(String),
}

/// The seam between the row's grading and the real `gcloud` process.
///
/// Why: the grading has eight outcomes and a no-spawn guarantee; both are
/// testable only when the process boundary can be replaced.
/// Test: `doctor_gcloud_tests.rs` drives every outcome through a scripted
/// runner.
pub(crate) trait GcloudRunner {
    /// Run `gcloud <args>` once and report how it ended.
    fn run(&mut self, args: &[String]) -> Run;
}

/// The runner `tm doctor --network` uses: the real `gcloud`, bounded.
///
/// What: stdin is `/dev/null` and `CLOUDSDK_CORE_DISABLE_PROMPTS=1` is set, so
/// a reauth prompt fails at once instead of waiting for input.
pub(crate) struct LiveGcloud;

impl GcloudRunner for LiveGcloud {
    fn run(&mut self, args: &[String]) -> Run {
        let mut cmd = Command::new("gcloud");
        cmd.args(args)
            .env("CLOUDSDK_CORE_DISABLE_PROMPTS", "1")
            .stdin(Stdio::null());
        match run_bounded(cmd, CALL_BUDGET) {
            Ok(out) => Run::Exited {
                success: out.status.success(),
                code: out.status.code(),
                stdout: out.stdout,
                stderr: out.stderr,
            },
            Err(BoundedError::Spawn(e)) if e.kind() == ErrorKind::NotFound => Run::NotFound,
            Err(BoundedError::TimedOut) => Run::TimedOut,
            Err(e) => Run::Error(format!("`gcloud` {e}")),
        }
    }
}

/// One entry of `gcloud auth list --format=json`.
#[derive(Debug, serde::Deserialize)]
struct Account {
    account: String,
    #[serde(default)]
    status: String,
}

impl Account {
    fn active(&self) -> bool {
        self.status.eq_ignore_ascii_case("ACTIVE")
    }
}

/// How one account's token mint ended.
#[derive(Debug, PartialEq, Eq)]
enum Mint {
    /// A non-empty token came back. Its text is never kept.
    Ok,
    /// The refresh was refused and `gcloud` asks for `gcloud auth login`.
    NeedsLogin,
    /// Anything else, with an already-redacted reason.
    Failed(String),
}

/// The row, on the blocking pool, for this process's real `gcloud`.
///
/// Test: `the_live_wrapper_never_runs_gcloud_without_network`.
pub(crate) async fn gcloud_auth_row(network: bool) -> DoctorCheck {
    row_on_blocking_pool(network, LiveGcloud).await
}

/// Gate on `network`, then grade on the blocking pool.
///
/// Why: `run_bounded` blocks its thread for up to [`CALL_BUDGET`] per call.
/// What: without `network` it returns the [`SKIPPED`] row before `runner` is
/// touched. A panicked probe task is a WARN, never a missing row.
/// Test: `the_live_wrapper_never_runs_gcloud_without_network`,
/// `the_live_wrapper_runs_gcloud_with_network`.
pub(crate) async fn row_on_blocking_pool<R>(network: bool, mut runner: R) -> DoctorCheck
where
    R: GcloudRunner + Send + 'static,
{
    if !network {
        return DoctorCheck::new(CHECK, CheckStatus::Ok, SKIPPED);
    }
    match tokio::task::spawn_blocking(move || gcloud_auth_check(&mut runner)).await {
        Ok(check) => check,
        Err(e) => warn(format!("the gcloud probe task did not finish: {e}")),
    }
}

/// Grade gcloud auth through `runner`.
///
/// What: FAIL for no credentialed account and for every probed account
/// needing an interactive login, both naming `! gcloud auth login`; OK only
/// when the ACTIVE account mints a token; WARN for every other outcome,
/// including any `gcloud` failure that is not a reauth refusal.
/// Test: the grading cases in `doctor_gcloud_tests.rs`.
pub(crate) fn gcloud_auth_check(runner: &mut dyn GcloudRunner) -> DoctorCheck {
    let accounts = match list_accounts(runner) {
        Ok(accounts) => accounts,
        Err(check) => return check,
    };
    if accounts.is_empty() {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Fail,
            "NO CREDENTIALS: `gcloud auth list` names no credentialed account. Operator step: \
             run `! gcloud auth login` at the Claude Code prompt; an agent cannot complete the \
             browser login.",
        );
    }
    let ordered: Vec<&Account> = accounts
        .iter()
        .filter(|a| a.active())
        .chain(accounts.iter().filter(|a| !a.active()))
        .collect();
    let mut probed: Vec<(&Account, Mint)> = Vec::new();
    for account in ordered.into_iter().take(MAX_PROBED) {
        let mint = mint_token(runner, &account.account);
        let minted = mint == Mint::Ok;
        probed.push((account, mint));
        if minted {
            break;
        }
    }
    grade(accounts.len(), &probed)
}

/// Read the credentialed accounts, or the WARN row saying why not.
fn list_accounts(runner: &mut dyn GcloudRunner) -> Result<Vec<Account>, DoctorCheck> {
    let args = ["auth", "list", "--format=json"].map(String::from);
    let stdout = match runner.run(&args) {
        Run::Exited {
            success: true,
            stdout,
            ..
        } => stdout,
        Run::NotFound => {
            return Err(warn(
                "`gcloud` is not on PATH, so gcloud auth was not checked",
            ));
        }
        other => return Err(warn(format!("`gcloud auth list` {}", describe(&other)))),
    };
    serde_json::from_str(&stdout).map_err(|e| {
        warn(format!(
            "`gcloud auth list --format=json` printed output that is not an account list ({e})"
        ))
    })
}

/// Try to mint one account's token. The token text is dropped here.
fn mint_token(runner: &mut dyn GcloudRunner, account: &str) -> Mint {
    let args = [
        "auth".to_string(),
        "print-access-token".to_string(),
        format!("--account={account}"),
        "--verbosity=error".to_string(),
    ];
    match runner.run(&args) {
        Run::Exited {
            success: true,
            stdout,
            ..
        } if !stdout.trim().is_empty() => Mint::Ok,
        Run::Exited { success: true, .. } => {
            Mint::Failed("exited 0 but printed no token".to_string())
        }
        Run::Exited {
            success: false,
            stderr,
            ..
        } if needs_login(&stderr) => Mint::NeedsLogin,
        other => Mint::Failed(describe(&other)),
    }
}

/// Whether `stderr` is gcloud's refresh refusal that only a login clears.
fn needs_login(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("reauth") || lower.contains("gcloud auth login")
}

/// Fold the probes into one row.
fn grade(total: usize, probed: &[(&Account, Mint)]) -> DoctorCheck {
    let active = probed.first().filter(|(a, _)| a.active());
    if let Some((account, Mint::Ok)) = active {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Ok,
            format!(
                "active account {} can mint an access token ({total} credentialed \
                 account(s)); the token is not shown",
                redact_account(&account.account)
            ),
        );
    }
    let lead = match active {
        Some((account, mint)) => format!(
            "active account {} {}",
            redact_account(&account.account),
            mint_phrase(mint)
        ),
        None => "no account is ACTIVE".to_string(),
    };
    if let Some((account, _)) = probed.iter().find(|(_, m)| *m == Mint::Ok) {
        return warn(format!(
            "{lead}; account {} can mint a token. Select it with `gcloud config set account`, \
             or re-login with `! gcloud auth login`.",
            redact_account(&account.account)
        ));
    }
    if probed.iter().all(|(_, m)| *m == Mint::NeedsLogin) {
        return DoctorCheck::new(
            CHECK,
            CheckStatus::Fail,
            format!(
                "REAUTH NEEDED: credentials are present for {total} account(s), but every one of \
                 the {} probed needs an interactive login (its refresh was refused \
                 non-interactively). Operator step: run `! gcloud auth login` at the Claude \
                 Code prompt; an agent cannot complete the browser login.",
                probed.len()
            ),
        );
    }
    let reasons: Vec<String> = probed
        .iter()
        .map(|(a, m)| format!("{} {}", redact_account(&a.account), mint_phrase(m)))
        .collect();
    warn(format!(
        "no probed account minted a token: {}",
        reasons.join("; ")
    ))
}

/// One clause for a failed mint, already redacted.
fn mint_phrase(mint: &Mint) -> String {
    match mint {
        Mint::Ok => "can mint a token".to_string(),
        Mint::NeedsLogin => "needs an interactive login (`! gcloud auth login`)".to_string(),
        Mint::Failed(reason) => format!("could not mint a token: {reason}"),
    }
}

/// One redacted clause for a run that did not succeed.
fn describe(run: &Run) -> String {
    match run {
        Run::Exited { code, stderr, .. } => {
            let first = stderr
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("no error output");
            let exit = code.map_or("was killed by a signal".to_string(), |c| {
                format!("exited {c}")
            });
            format!("{exit}: {}", redact_text(first))
        }
        Run::NotFound => "could not run: `gcloud` is not on PATH".to_string(),
        Run::TimedOut => format!(
            "did not answer within {}s and was killed",
            CALL_BUDGET.as_secs()
        ),
        Run::Error(e) => redact_text(e),
    }
}

fn warn(message: impl Into<String>) -> DoctorCheck {
    DoctorCheck::new(CHECK, CheckStatus::Warn, message)
}

/// `bob@example.com` → `b***@example.com`; no `@` → `***`.
pub(crate) fn redact_account(account: &str) -> String {
    match account.split_once('@') {
        Some((local, domain)) => {
            let first = local.chars().next().map(String::from).unwrap_or_default();
            format!("{first}***@{domain}")
        }
        None => "***".to_string(),
    }
}

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]+@([A-Za-z0-9.-]+)").expect("static email pattern compiles")
});

static TOKENISH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"ya29\.[\w.-]+|1//[\w.-]+|[A-Za-z0-9_.-]{40,}")
        .expect("static token pattern compiles")
});

/// Strip anything token-shaped and every email local part from `text`.
///
/// Why: a `gcloud` error line is the one piece of child output the row
/// prints, and it can name the account or echo a credential.
/// Test: `redact_text_strips_tokens_and_email_local_parts`.
pub(crate) fn redact_text(text: &str) -> String {
    // Redact before truncating, so a cut can never split a match.
    let text = TOKENISH.replace_all(text, "[redacted]");
    let text = EMAIL.replace_all(&text, "***@$1");
    text.chars().take(200).collect()
}

#[cfg(test)]
#[path = "doctor_gcloud_tests.rs"]
mod tests;
