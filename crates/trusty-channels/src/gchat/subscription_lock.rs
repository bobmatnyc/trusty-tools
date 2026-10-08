//! The user-level subscription lock: one `gchat-mcp` per Pub/Sub
//! subscription, whatever project dir it serves.
//!
//! Why: `routes.toml` is tracked, so every worktree and clone of a project
//! names the same `[gchat.connection]`. The per-project state lock cannot see
//! across them, and a second server pulling the subscription acks a reply its
//! ledger cannot bind, which loses the answer (#9448 review).
//! What: [`lock_subscription`] takes an exclusive lock on
//! `<root>/<encoded subscription>.lock` without waiting. The binary passes
//! `<user data dir>/trusty-channels/`[`LOCK_DIR`]; the root is a parameter
//! so tests never touch the real home directory.
//! Test: `second_server_on_one_subscription_is_refused_from_another_project_dir`.

use std::path::Path;

use crate::gchat::error::StateError;

/// The lock directory's name inside the per-user `trusty-channels` data dir.
pub const LOCK_DIR: &str = "gchat-locks";

/// An exclusive advisory lock on one subscription; dropping it unlocks.
#[derive(Debug)]
pub struct SubscriptionLock {
    _file: std::fs::File,
}

/// Take `subscription`'s lock under `root` without waiting.
///
/// Why: only one process may consume a subscription (#9448, Architect
/// ruling), and two project dirs can name the same one.
/// What: creates `root`, opens (creating) the lock file named by
/// [`lock_file_name`] and takes an exclusive `flock`-style lock. A held lock
/// returns [`StateError::SubscriptionInUse`]; the lock lives as long as the
/// returned guard.
/// Test: `second_server_on_one_subscription_is_refused_from_another_project_dir`.
pub fn lock_subscription(root: &Path, subscription: &str) -> Result<SubscriptionLock, StateError> {
    std::fs::create_dir_all(root).map_err(|e| StateError::io(root, &e))?;
    let path = root.join(lock_file_name(subscription));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| StateError::io(&path, &e))?;
    match file.try_lock() {
        Ok(()) => Ok(SubscriptionLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(StateError::SubscriptionInUse {
            subscription: subscription.to_string(),
            path,
        }),
        Err(std::fs::TryLockError::Error(e)) => Err(StateError::io(&path, &e)),
    }
}

/// A file name for `subscription`, one-to-one: ASCII letters, digits, `-`
/// and `.` pass through; every other byte becomes `_XX` (uppercase hex).
fn lock_file_name(subscription: &str) -> String {
    let mut name = String::with_capacity(subscription.len() + 5);
    for b in subscription.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'.' {
            name.push(char::from(b));
        } else {
            name.push_str(&format!("_{b:02X}"));
        }
    }
    name.push_str(".lock");
    name
}

#[cfg(test)]
mod tests {
    use super::lock_file_name;

    #[test]
    fn lock_file_name_is_one_to_one_and_has_no_separator() {
        let a = lock_file_name("projects/p/subscriptions/s_x");
        assert_eq!(a, "projects_2Fp_2Fsubscriptions_2Fs_5Fx.lock");
        assert_ne!(a, lock_file_name("projects/p/subscriptions/s/x"));
        assert!(!lock_file_name("../..").contains('/'));
    }
}
