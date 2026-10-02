//! Tests for the `credential_reach` doctor row (#8236 item 8).
//!
//! Nothing here touches an OS keychain, a real `$HOME`, or a launchd domain:
//! every outcome is a `Result` written three lines above the assertion that
//! reads it. The one "value" any test uses is a literal defined in the test
//! itself, so a leak assertion can name what must be absent.

use super::*;

/// A credential value that exists only inside this file.
///
/// Why: `a_present_credential_is_never_printed` has to assert that a SPECIFIC
/// string is absent from the row, and the only string it can safely name is one
/// this file invented.
const FAKE_VALUE: &str = "not-a-real-credential-9f3c1a";

/// Why: after every `cargo install` the daemon's first Keychain read waits on a
/// SecurityAgent dialog. An operator who reads that as "absent" puts the
/// plaintext plist entry back, which is #8236 returning. The row therefore has
/// to say "a dialog may be on screen" and has to mark the credential as needing
/// approval — a silent downgrade of this arm to the `Absent` wording, or to
/// `needs_approval: false`, is exactly the regression, and fails here.
/// Test: this test.
#[test]
fn a_timeout_is_reported_as_a_waiting_dialog() {
    let outcome = Err(SecretResolveError::Timeout {
        var: "OPENROUTER_API_KEY".to_string(),
        waited_ms: 3000,
        cached: false,
    });

    let verdict = verdict_for("OPENROUTER_API_KEY", &outcome);

    assert!(verdict.needs_approval, "{verdict:?}");
    assert!(verdict.degraded, "{verdict:?}");
    assert!(verdict.verdict.contains("timed out"), "{}", verdict.verdict);
    assert!(verdict.verdict.contains("3000 ms"), "{}", verdict.verdict);
    assert!(
        verdict.verdict.contains("Keychain approval dialog"),
        "{}",
        verdict.verdict
    );
    assert!(
        !verdict.verdict.contains("absent"),
        "a timeout must not read as absent: {}",
        verdict.verdict
    );
}

/// Why: a host that deliberately runs without Telegram is not broken. "Absent"
/// is a degraded row, never an approval prompt — if this arm were downgraded to
/// the timeout wording the operator would go hunting for a dialog that does not
/// exist, and if it were downgraded to a clean row the unconfigured credential
/// would be invisible.
/// Test: this test.
#[test]
fn an_absent_credential_is_not_an_error() {
    let outcome = Err(SecretResolveError::Absent {
        var: "TELEGRAM_BOT_TOKEN".to_string(),
    });

    let verdict = verdict_for("TELEGRAM_BOT_TOKEN", &outcome);

    assert!(verdict.degraded, "{verdict:?}");
    assert!(!verdict.needs_approval, "{verdict:?}");
    assert!(verdict.verdict.starts_with("absent"), "{}", verdict.verdict);

    // The row an absent-only host sees: flagged, never failed, never clean.
    let row = build_row(&[verdict]);
    assert_eq!(row.status, CheckStatus::Warn, "{row:?}");
}

/// Why: "the store could not supply it" and "nothing is configured" send an
/// operator to two different places. The kind is the only thing in the row that
/// tells them apart, and it is a fixed label precisely so a downgrade to a
/// generic message is visible here.
/// Test: this test.
#[test]
fn a_store_error_names_its_kind() {
    for (kind, label) in [
        (
            trusty_common::credentials::StoreErrorKind::Keyring,
            "keyring-backend",
        ),
        (trusty_common::credentials::StoreErrorKind::Io, "io"),
        (trusty_common::credentials::StoreErrorKind::Toml, "toml"),
        (
            trusty_common::credentials::StoreErrorKind::HomeUnavailable,
            "home-unavailable",
        ),
    ] {
        let outcome = Err(SecretResolveError::Store {
            var: "SLACK_BOT_TOKEN".to_string(),
            kind,
            cached: false,
        });

        let verdict = verdict_for("SLACK_BOT_TOKEN", &outcome);

        assert!(verdict.degraded, "{verdict:?}");
        assert!(verdict.verdict.contains(label), "{}", verdict.verdict);
    }
}

/// Why: the whole defect this issue names is a credential reaching a file a
/// human reads. A doctor row that printed the value it just resolved would
/// reintroduce it in `tm doctor` output, which agents paste into tickets.
/// Test: this test.
#[test]
fn a_present_credential_is_never_printed() {
    let outcome: Result<String, SecretResolveError> = Ok(FAKE_VALUE.to_string());

    let verdict = verdict_for("ANTHROPIC_API_KEY", &outcome);

    assert!(!verdict.degraded, "{verdict:?}");
    assert!(!verdict.needs_approval, "{verdict:?}");
    assert_eq!(verdict.verdict, "present");
    assert!(
        !verdict.verdict.contains(FAKE_VALUE),
        "the row disclosed the value"
    );

    let row = build_row(&[verdict]);
    assert!(
        !row.message.contains(FAKE_VALUE),
        "the row disclosed the value"
    );
}

/// Why: an operator can point `[llm] api_key_env` at any variable. One that is
/// not in the registry has no provider and therefore cannot be resolved at all
/// — reporting that as "absent" would send them to configure a store tier that
/// will never be consulted.
/// Test: this test.
#[test]
fn an_unregistered_name_is_reported_as_unresolvable() {
    let outcome = Err(SecretResolveError::Unregistered {
        var: "MY_OWN_KEY".to_string(),
    });

    let verdict = verdict_for("MY_OWN_KEY", &outcome);

    assert!(verdict.degraded, "{verdict:?}");
    assert!(!verdict.needs_approval, "{verdict:?}");
    assert!(
        verdict.verdict.contains("not a registered credential name"),
        "{}",
        verdict.verdict
    );
}

/// Why: the row is the operator's whole view of which credentials the daemon
/// can actually reach. One line per credential, each naming its variable, is
/// what makes "the overseer stopped working" diagnosable without a log dive.
/// Test: this test.
#[test]
fn row_reports_one_line_per_daemon_credential() {
    let verdicts: Vec<ReachVerdict> = DAEMON_PROVIDERS
        .iter()
        .map(|provider| {
            let var = trusty_common::credential_registry::env_var_for(provider)
                .expect("every daemon provider is registered");
            verdict_for(var, &Ok("x".to_string()))
        })
        .collect();

    let row = build_row(&verdicts);

    assert_eq!(row.name, "credential_reach");
    for provider in DAEMON_PROVIDERS {
        let var = trusty_common::credential_registry::env_var_for(provider)
            .expect("every daemon provider is registered");
        assert!(
            row.message.contains(var),
            "{} missing: {}",
            var,
            row.message
        );
    }
}

/// Why: a timeout has learned nothing — the read may still land once a human
/// approves. `Warn` would claim a known-minor problem and `Ok` would claim
/// health; both are false, and both would hide the one state that follows every
/// reinstall.
/// Test: this test.
#[test]
fn row_is_unknown_when_a_credential_times_out() {
    let row = build_row(&[
        verdict_for("ANTHROPIC_API_KEY", &Ok("x".to_string())),
        verdict_for(
            "OPENROUTER_API_KEY",
            &Err(SecretResolveError::Timeout {
                var: "OPENROUTER_API_KEY".to_string(),
                waited_ms: 3000,
                cached: false,
            }),
        ),
    ]);

    assert_eq!(row.status, CheckStatus::Unknown, "{row:?}");
}

/// Why: the row has to be able to go green, or an operator learns to ignore it.
/// Test: this test.
#[test]
fn row_is_ok_when_every_credential_resolves() {
    let row = build_row(&[
        verdict_for("ANTHROPIC_API_KEY", &Ok("x".to_string())),
        verdict_for("SLACK_BOT_TOKEN", &Ok("y".to_string())),
    ]);

    assert_eq!(row.status, CheckStatus::Ok, "{row:?}");
}

/// Why (#8236): the row is reached by `GET /api/v1/doctor` on a tokio worker,
/// and every provider it reads can park for the store's full 3 s bound — up to
/// ~12 s of a runtime thread held by a Keychain dialog. The probe therefore has
/// to run on the BLOCKING pool, and the only way to prove that is a probe that
/// reports the thread it ran on.
/// Test: this test.
#[tokio::test(flavor = "current_thread")]
async fn the_row_is_produced_off_the_runtime_thread() {
    let runtime_thread = format!("{:?}", std::thread::current().id());

    let check = off_runtime(|| {
        DoctorCheck::new(
            "credential_reach",
            CheckStatus::Ok,
            format!("{:?}", std::thread::current().id()),
        )
    })
    .await;

    assert_ne!(
        check.message, runtime_thread,
        "the probe ran on the runtime thread it was supposed to leave"
    );
}

/// Why: a probe that panics must not take the whole doctor report with it, and
/// must not be silently dropped either — a row that did not run has not shown
/// the daemon healthy.
/// Test: this test.
#[tokio::test]
async fn a_probe_that_panics_is_unknown_not_a_lost_row() {
    let check = off_runtime(|| panic!("a deliberate probe panic, not a real failure")).await;

    assert_eq!(check.status, CheckStatus::Unknown, "{check:?}");
    assert_eq!(check.name, "credential_reach");
    assert!(check.message.contains("UNKNOWN"), "{}", check.message);
}
