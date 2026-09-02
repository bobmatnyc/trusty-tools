//! Whether a launchd unit owns the socket an on-demand spawn is about to bind
//! (#6619).
//!
//! Why: a daemon's client bridges spawn it on demand when nothing answers its
//! socket. That contract is correct on a dev machine and wrong on a supervised
//! one: during a `launchctl bootout`/`bootstrap` window the socket is
//! transiently unserved, a bridge read that as "nothing is running", and spawned
//! an unsupervised daemon onto the PRODUCTION socket path — without the plist's
//! `EnvironmentVariables`. launchd's own instance then found the path already
//! held and exited 0 ("another instance is already running"), so launchd
//! reported success while a misconfigured orphan owned the socket.
//!
//! The single-flight `flock` those bridges share (#5267/#6286) cannot see this:
//! it coordinates bridges with each other, not with launchd.
//!
//! 🔴 **`launchctl print` alone cannot answer this question.** In the exact
//! window that loses the race the unit is BOOTED OUT, so launchd reports nothing
//! and a check that trusted it would permit the spawn it exists to stop. The
//! installed plist is what persists across the window, so its presence counts as
//! registration — see [`socket_owner`](crate::launchd_claim::socket_owner).
//!
//! What: [`socket_owner`](crate::launchd_claim::socket_owner) is the pure
//! decision over the two registration signals;
//! [`launchd_socket_owner`](crate::launchd_claim::launchd_socket_owner) reads
//! them off the real launchd. A caller
//! that is not on its canonical production path (a test socket, a
//! `TRUSTY_DATA_DIR_OVERRIDE` sandbox) passes `is_supervised_path: false` and is
//! never affected.
//!
//! Test: `cargo test -p trusty-common --features unconditional-only
//! launchd_claim`.

/// Who owns the socket an on-demand spawn is about to bind.
///
/// Why: the caller's two options are opposite — wait for launchd, or spawn —
/// and the waiting branch has to name the unit it is waiting for, so the label
/// travels with the verdict rather than being re-derived at the message site.
/// What: [`Launchd`](Self::Launchd) carries the owning label;
/// [`OnDemand`](Self::OnDemand) is the unsupervised case the bridge contract was
/// written for.
/// Test: `socket_owner_defers_to_a_loaded_unit`,
/// `socket_owner_defers_while_the_unit_is_booted_out`,
/// `socket_owner_leaves_an_unmanaged_host_alone`,
/// `socket_owner_ignores_a_socket_launchd_does_not_serve`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SocketOwner {
    /// A launchd unit is registered for this path. A spawn here would race it.
    Launchd {
        /// The unit's label, for the caller's message.
        label: String,
    },
    /// Nothing supervises this path — an on-demand spawn is the caller's to
    /// make, exactly as before.
    OnDemand,
}

impl SocketOwner {
    /// The owning label, when launchd owns the path.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        match self {
            SocketOwner::Launchd { label } => Some(label),
            SocketOwner::OnDemand => None,
        }
    }

    /// Whether an on-demand spawn must stand down.
    #[must_use]
    pub fn is_launchd(&self) -> bool {
        matches!(self, SocketOwner::Launchd { .. })
    }
}

/// Decide whether launchd owns this socket, from the two registration signals.
///
/// Why: kept pure and separate from the `launchctl` read, because the decision
/// is what governs whether a daemon gets spawned onto a production socket — the
/// thing that went wrong — and it must be testable without a registered unit.
///
/// What: launchd owns the path when the caller is on its canonical production
/// socket AND either signal says a unit exists.
///
/// - `is_supervised_path` — the caller compares the socket it is about to serve
///   against its own canonical production path. A socket under a `TempDir` or a
///   `TRUSTY_DATA_DIR_OVERRIDE` sandbox is never launchd's, whatever is
///   registered, so tests and dev sandboxes keep the old behaviour.
/// - `unit_loaded` — `launchctl print gui/<uid>/<label>` answered.
/// - `plist_present` — `~/Library/LaunchAgents/<label>.plist` exists.
///
/// 🔴 `plist_present` is not redundant with `unit_loaded`; it is the whole fix.
/// The #6619 window is precisely the one where the unit is booted out and
/// `unit_loaded` is false, and the plist file is what survives it. A host that
/// has genuinely uninstalled the service has no plist, so it still reads
/// [`SocketOwner::OnDemand`].
///
/// Test: `socket_owner_defers_to_a_loaded_unit`,
/// `socket_owner_defers_while_the_unit_is_booted_out`,
/// `socket_owner_leaves_an_unmanaged_host_alone`,
/// `socket_owner_ignores_a_socket_launchd_does_not_serve`.
#[must_use]
pub fn socket_owner(
    label: &str,
    is_supervised_path: bool,
    unit_loaded: bool,
    plist_present: bool,
) -> SocketOwner {
    if is_supervised_path && (unit_loaded || plist_present) {
        SocketOwner::Launchd {
            label: label.to_owned(),
        }
    } else {
        SocketOwner::OnDemand
    }
}

/// [`socket_owner`] over the launchd registration this host actually has.
///
/// Why/What: binds the two signals to `launchctl print` and the installed plist.
/// On every non-macOS platform launchd does not exist, which is a positive
/// negative rather than an unanswered question, so the answer is
/// [`SocketOwner::OnDemand`] and nothing changes for those callers.
///
/// Test: the decision is covered by `socket_owner_*`; the `launchctl` and
/// filesystem reads are side-effecting and are exercised by trusty-memory's
/// stdio bridge.
#[must_use]
pub fn launchd_socket_owner(label: &str, is_supervised_path: bool) -> SocketOwner {
    #[cfg(target_os = "macos")]
    {
        socket_owner(
            label,
            is_supervised_path,
            crate::launchd::label_is_loaded(label),
            crate::launchd::plist_path_for_label(label).is_some_and(|p| p.exists()),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = is_supervised_path;
        socket_owner(label, false, false, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unit that lost the #6619 race.
    const LABEL: &str = "com.trusty.memory";

    /// Why: the ordinary supervised steady state — launchd has the unit loaded
    /// and a bridge must never spawn a second daemon onto its socket.
    /// What: a loaded unit on the production path yields the owning label.
    /// Test: itself.
    #[test]
    fn socket_owner_defers_to_a_loaded_unit() {
        let owner = socket_owner(LABEL, true, true, true);
        assert_eq!(
            owner,
            SocketOwner::Launchd {
                label: LABEL.to_owned()
            }
        );
        assert_eq!(owner.label(), Some(LABEL));
    }

    /// Why (#6619): this IS the losing window. Between `bootout` and
    /// `bootstrap` launchd reports nothing for the label, so a check that
    /// trusted `launchctl print` alone would permit exactly the spawn that stole
    /// the production socket. The plist on disk is what survives the window.
    /// What: `unit_loaded: false` with the plist still installed still yields
    /// [`SocketOwner::Launchd`].
    /// Test: itself.
    #[test]
    fn socket_owner_defers_while_the_unit_is_booted_out() {
        assert_eq!(
            socket_owner(LABEL, true, false, true),
            SocketOwner::Launchd {
                label: LABEL.to_owned()
            },
            "a booted-out unit still owns its socket — it is coming back"
        );
    }

    /// Why: a dev machine that never installed the service must keep the
    /// on-demand spawn the bridge contract promises. Refusing there would break
    /// every developer running `trusty-memory serve --stdio` locally.
    /// What: neither signal set yields [`SocketOwner::OnDemand`].
    /// Test: itself.
    #[test]
    fn socket_owner_leaves_an_unmanaged_host_alone() {
        let owner = socket_owner(LABEL, true, false, false);
        assert_eq!(owner, SocketOwner::OnDemand);
        assert!(!owner.is_launchd());
        assert_eq!(owner.label(), None);
    }

    /// Why: the guard is about ONE path — the canonical production socket. A
    /// test socket under a `TempDir` is not launchd's even on a fully installed
    /// host, and treating it as such would make the daemon's own test suite
    /// refuse to start.
    /// What: `is_supervised_path: false` yields [`SocketOwner::OnDemand`]
    /// regardless of registration.
    /// Test: itself.
    #[test]
    fn socket_owner_ignores_a_socket_launchd_does_not_serve() {
        assert_eq!(
            socket_owner(LABEL, false, true, true),
            SocketOwner::OnDemand
        );
    }
}
