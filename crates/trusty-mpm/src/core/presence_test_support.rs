//! Shared fixtures for the #8878 PR2b-1a presence tests: a scratch anchor,
//! a test key (never the login Keychain), a fake device and two verifiers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::architect_launch::ARCHITECT_DIR;
use crate::core::presence_device::{self, Device, NewDevice};
use crate::core::presence_enrol::{TicketPurpose, mint_ticket};
use crate::core::presence_key::GrantKey;
use crate::core::presence_request::{self, NewRequest, Pending};
use crate::core::presence_verify::{
    Assertion, AssertionVerifier, Ceremonies, Challenge, Granted, Verified, VerifyEnv, VerifyError,
    verify_and_grant,
};
use crate::core::twin_identity::ClaudeProcess;

pub(crate) const NOW: u64 = 1_800_000_000;
pub(crate) const TTL: Duration = Duration::from_secs(300);
pub(crate) const ARCHITECT: ClaudeProcess = ClaudeProcess {
    pid: 4242,
    start_time: 1_799_990_000,
};

/// The test key: fixed bytes, never read from a Keychain.
pub(crate) fn key() -> GrantKey {
    GrantKey::from_bytes([7; 32])
}

/// A scratch `~/.trusty-mpm` with the anchor directory made.
pub(crate) fn root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(".trusty-mpm");
    std::fs::create_dir_all(root.join(ARCHITECT_DIR)).expect("mkdir anchor");
    (dir, root)
}

/// Every file under the anchor, with its bytes: a refusal changes none.
pub(crate) fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.join(ARCHITECT_DIR)];
    while let Some(dir) = stack.pop() {
        for path in std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
        {
            let meta = std::fs::symlink_metadata(&path).expect("stat");
            if meta.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap_or_default();
                out.insert(path, bytes);
            }
        }
    }
    out
}

/// The grant files under the anchor.
pub(crate) fn grant_files(root: &Path) -> Vec<PathBuf> {
    snapshot(root)
        .into_keys()
        .filter(|p| p.extension().is_some_and(|e| e == "presence"))
        .collect()
}

/// A fake authenticator: its "signature" is an HMAC under a secret whose
/// hex is the device's public key.
#[derive(Debug, Clone)]
pub(crate) struct FakeDevice {
    pub(crate) credential_id: String,
    pub(crate) secret: [u8; 32],
}

impl FakeDevice {
    pub(crate) fn new(credential_id: &str, seed: u8) -> Self {
        Self {
            credential_id: credential_id.to_owned(),
            secret: [seed; 32],
        }
    }

    /// Enrol through a valid ticket; the record starts at `counter`.
    pub(crate) fn enrol(&self, root: &Path, counter: u32) -> Device {
        let name = format!("phone-{}", self.credential_id);
        let ticket = mint_ticket(&key(), TicketPurpose::Enrol, &name, NOW).expect("ticket");
        presence_device::enrol(root, &key(), &ticket, self.new_device(&name, counter), NOW)
            .expect("enrol")
    }

    pub(crate) fn new_device(&self, name: &str, counter: u32) -> NewDevice {
        NewDevice {
            credential_id: self.credential_id.clone(),
            name: name.to_owned(),
            public_key: hex::encode(self.secret),
            sign_count: counter,
            backup_eligible: false,
            backup_state: false,
        }
    }

    /// An assertion over `challenge` for `pending`, flags and counter given.
    pub(crate) fn sign(
        &self,
        challenge: &Challenge,
        pending: &Pending,
        counter: u32,
        uv: bool,
    ) -> Assertion {
        let body = signed_body(&challenge.bytes, counter, uv);
        let sig = GrantKey::from_bytes(self.secret).seal(body.as_bytes());
        Assertion {
            request_id: pending.id.clone(),
            credential_id: self.credential_id.clone(),
            action_hash: pending.action_hash.clone(),
            payload: format!("{body}|{sig}").into_bytes(),
        }
    }
}

fn signed_body(challenge: &[u8], counter: u32, uv: bool) -> String {
    format!("{}|{counter}|{}", hex::encode(challenge), u8::from(uv))
}

/// Checks a [`FakeDevice`] signature over the open challenge.
pub(crate) struct FakeVerifier;

impl AssertionVerifier for FakeVerifier {
    fn verify(
        &self,
        challenge: &Challenge,
        device: &Device,
        a: &Assertion,
    ) -> Result<Verified, String> {
        let text = String::from_utf8(a.payload.clone()).map_err(|_| "not utf-8")?;
        let (body, sig) = text.rsplit_once('|').ok_or("no signature")?;
        let secret = hex::decode(&device.public_key).map_err(|_| "bad key")?;
        let secret: [u8; 32] = secret.try_into().map_err(|_| "bad key length")?;
        if !GrantKey::from_bytes(secret).verify(body.as_bytes(), sig) {
            return Err("bad signature".to_owned());
        }
        let mut parts = body.split('|');
        let signed_challenge = parts.next().ok_or("no challenge")?;
        if signed_challenge != hex::encode(challenge.bytes) {
            return Err("the challenge differs".to_owned());
        }
        let counter = parts
            .next()
            .and_then(|c| c.parse().ok())
            .ok_or("no counter")?;
        let uv = parts.next() == Some("1");
        Ok(Verified {
            sign_count: counter,
            user_present: true,
            user_verified: uv,
            backup_state: false,
        })
    }
}

/// Accepts anything, with a counter one above the stored one: the spoof
/// proof that a check lives outside the verifier.
pub(crate) struct AlwaysAcceptVerifier;

impl AssertionVerifier for AlwaysAcceptVerifier {
    fn verify(&self, _: &Challenge, device: &Device, _: &Assertion) -> Result<Verified, String> {
        Ok(Verified {
            sign_count: device.sign_count.saturating_add(1),
            user_present: true,
            user_verified: true,
            backup_state: false,
        })
    }
}

/// A request from the Architect `claude`.
pub(crate) fn request(root: &Path, action: &str) -> Pending {
    request_for(root, ARCHITECT, action, NOW)
}

pub(crate) fn request_for(root: &Path, claude: ClaudeProcess, action: &str, now: u64) -> Pending {
    presence_request::create(
        root,
        &key(),
        NewRequest {
            pid: claude.pid,
            start_time: claude.start_time,
            project: "trusty-tools".to_owned(),
            session: "tm-architect".to_owned(),
            action: action.to_owned(),
        },
        TTL,
        now,
    )
    .expect("create request")
}

/// Every process is the Architect at its recorded start time.
pub(crate) fn architect_alive(pid: u32) -> Option<u64> {
    (pid == ARCHITECT.pid).then_some(ARCHITECT.start_time)
}

/// Verify `assertion` at `now` with the test key.
pub(crate) fn verify_at(
    root: &Path,
    ceremonies: &Ceremonies,
    verifier: &dyn AssertionVerifier,
    assertion: &Assertion,
    now: u64,
) -> Result<Granted, VerifyError> {
    let key_fn = || Some(key());
    let env = VerifyEnv {
        root,
        key: &key_fn,
        start_time_of: &architect_alive,
        now,
    };
    verify_and_grant(&env, ceremonies, verifier, assertion)
}

/// Run a spoof scenario under the real fake-device verifier (it must be
/// refused and change nothing), then under [`AlwaysAcceptVerifier`]:
/// `verifier` spoofs must then grant (the test can fail), `orchestrator`
/// spoofs must still be refused (the check is not the verifier's).
macro_rules! spoof {
    ($name:ident, $owner:ident, $scenario:expr) => {
        #[test]
        fn $name() {
            use $crate::core::presence_test_support::{AlwaysAcceptVerifier, FakeVerifier, root, snapshot};
            let (_d, root) = root();
            let (before, outcome) = $scenario(&root, &FakeVerifier);
            assert!(outcome.is_err(), "the spoof was granted: {outcome:?}");
            assert_eq!(snapshot(&root), before, "a refusal writes and consumes nothing");
            let (_d2, root2) = $crate::core::presence_test_support::root();
            let (_, accepted) = $scenario(&root2, &AlwaysAcceptVerifier);
            spoof!(@second $owner, accepted);
        }
    };
    (@second verifier, $accepted:expr) => {
        assert!($accepted.is_ok(), "an always-accept verifier must grant this spoof: {:?}", $accepted);
    };
    (@second orchestrator, $accepted:expr) => {
        assert!($accepted.is_err(), "the orchestrator must refuse this spoof by itself");
    };
}
pub(crate) use spoof;
