//! First-run user-profile load + interactive interview (#193).
//!
//! Why: The profile setup is a self-contained side concern of CTRL startup;
//! splitting it from the command dispatcher + stdin loop keeps both files under
//! the line cap.
//! What: `load_or_create_user_profile` (load-or-interview) and
//! `conduct_user_interview` (the interactive prompts).
//! Test: Smoke-tested via the tmux REPL harness; the load path short-circuits
//! when a complete profile already exists.

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Load `~/.open-mpm/user.toml`, or run the first-run interview to create it. (#193)
pub(super) async fn load_or_create_user_profile()
-> Result<Option<crate::identity::user_profile::UserProfile>> {
    use crate::identity::user_profile::UserProfile;

    if let Some(p) = UserProfile::load()
        && p.is_complete()
    {
        return Ok(Some(p));
    }

    let noninteractive = std::env::var("OPEN_MPM_NONINTERACTIVE").is_ok()
        || std::env::var("OPEN_MPM_API_TOKEN").is_ok();
    if noninteractive {
        let p = UserProfile {
            name: "User".to_string(),
            email: None,
            preferred_model: None,
            timezone: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        return Ok(Some(p));
    }

    let profile = conduct_user_interview().await?;
    if let Err(e) = profile.save() {
        tracing::warn!(error = %e, "failed to save user profile (continuing in-memory)");
    } else {
        eprintln!(
            "Welcome, {}! Your profile has been saved to ~/.open-mpm/user.toml",
            profile.name
        );
    }
    Ok(Some(profile))
}

/// Interactive first-run interview that captures the user profile. (#193)
async fn conduct_user_interview() -> Result<crate::identity::user_profile::UserProfile> {
    use crate::identity::user_profile::UserProfile;

    eprintln!("[open-mpm] First-run setup — let's capture a quick profile.");
    eprint!("What's your name? ");
    let _ = std::io::Write::flush(&mut std::io::stderr());

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);

    let mut name = String::new();
    reader.read_line(&mut name).await?;
    let name = name.trim().to_string();
    let name = if name.is_empty() {
        "User".to_string()
    } else {
        name
    };

    eprint!("Email address (optional, press Enter to skip): ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut email = String::new();
    reader.read_line(&mut email).await?;
    let email = email.trim().to_string();
    let email = if email.is_empty() { None } else { Some(email) };

    eprint!("Timezone (e.g. America/New_York, or Enter to skip): ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut tz = String::new();
    reader.read_line(&mut tz).await?;
    let tz = tz.trim().to_string();
    let timezone = if tz.is_empty() { None } else { Some(tz) };

    Ok(UserProfile {
        name,
        email,
        preferred_model: None,
        timezone,
        created_at: chrono::Utc::now().to_rfc3339(),
    })
}
