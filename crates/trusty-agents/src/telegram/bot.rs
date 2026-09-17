//! One Telegram bot this host polls, and who it may deliver to (#8190).
//!
//! Why (owner ruling 2026-09-16): each assistant has its own Telegram bot. A
//! binding on izzie, cto-assistant or writing-assistant carries its own bot
//! token through the binding's `credential_ref`, the same pattern Slack
//! bindings use — so "the bot token" is not a machine-wide fact and cannot be
//! resolved once at startup. Everything downstream of that ruling needs ONE
//! value carrying the token, the assistants that own it, and the file-name key
//! its lock and pairing state live under; this module is that value.
//!
//! What: [`BotKey`] is a non-reversible digest of the token, used ONLY to name
//! per-bot state files. It never renders: its `Debug` is redacted and it has no
//! `Display`, because a stable digest of a secret is still a correlatable
//! identifier for that secret. [`TelegramBot`] pairs the key with the resolved
//! token, the assistants allowed to claim this bot's updates, and the operator
//! credential references that named it — the last being config text, which is
//! what every log line and status row shows instead.
//!
//! Test: `crate::telegram::tests` — `telegram_bot_key_is_stable_per_token`,
//! `telegram_bot_key_never_renders_the_token`,
//! `telegram_state_file_names_never_contain_the_token`.

use sha2::{Digest, Sha256};
use trusty_common::credentials::Secret;

/// Characters of the token digest used in a state-file name.
///
/// Why: 16 hex characters is 64 bits — collision-free across the handful of
/// bots one host runs, and short enough to keep the file name readable.
const DIGEST_LEN: usize = 16;

/// A non-reversible, stable key for one bot token.
///
/// Why: the lock file and the pairing file must be per bot, and the only thing
/// that distinguishes two bots without an extra config field is the token
/// itself — which must never appear in a path, a log, or a status row. A digest
/// gives a stable name with no way back to the secret.
/// What: the first [`DIGEST_LEN`] hex characters of the token's SHA-256. Equal
/// tokens give equal keys, which is what makes two bindings sharing one token
/// collapse onto one poller.
/// Test: `telegram_bot_key_is_stable_per_token`,
/// `telegram_bot_key_never_renders_the_token`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct BotKey(String);

impl BotKey {
    /// Derive the key for `token`.
    pub(crate) fn from_token(token: &str) -> Self {
        let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
        Self(digest.chars().take(DIGEST_LEN).collect())
    }

    /// The digest, for a FILE NAME only — never for a log line or a status row.
    pub(crate) fn digest(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for BotKey {
    /// Redacted, like [`Secret`]'s own `Debug`.
    ///
    /// Why (#8190): a `#[derive(Debug)]` here would put the digest into every
    /// `tracing` field that formats a struct containing it. A digest is a
    /// stable correlator for the secret it was derived from, so it is withheld
    /// on the same terms as the secret.
    /// Test: `telegram_bot_key_never_renders_the_token`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotKey(<redacted>)")
    }
}

/// One bot token, its owners, and the operator text that names it.
///
/// Why (#8190): the long-poll loop used to resolve one process-global token and
/// deliver to whichever assistant's binding matched the chat — so a message on
/// izzie's bot could wake cto-assistant. Carrying the owners beside the token
/// is what makes "this bot delivers only to its own assistants" a property of
/// the value the poller holds, rather than a rule someone has to remember.
/// What: `owners` is the `allowed_personas` filter
/// `agent_channels::inbound::receive_inbound` already applies — `None` means
/// the pre-#8190 behaviour (any assistant may claim), which only the standalone
/// `--telegram` and REPL paths use. `credential_refs` is display text: a
/// credential REFERENCE is a config name, never a secret.
/// Test: `telegram_bot_owners_scope_the_dispatch`.
///
/// `Debug` is hand-written, not derived: a derive would render the [`BotKey`]
/// field, and keeping the whole type free of any key-shaped output is the
/// property a test can state (#8190).
pub(crate) struct TelegramBot {
    key: BotKey,
    token: Secret<String>,
    owners: Option<Vec<String>>,
    credential_refs: Vec<String>,
}

impl std::fmt::Debug for TelegramBot {
    /// Renders the operator-facing label only — never the token or the key.
    ///
    /// Test: `telegram_bot_key_never_renders_the_token`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramBot")
            .field("credential_refs", &self.credential_refs)
            .field("owners", &self.owners)
            .finish_non_exhaustive()
    }
}

impl TelegramBot {
    /// Build a bot from a resolved token.
    pub(crate) fn new(
        token: Secret<String>,
        owners: Option<Vec<String>>,
        credential_refs: Vec<String>,
    ) -> Self {
        Self {
            key: BotKey::from_token(token.expose()),
            token,
            owners,
            credential_refs,
        }
    }

    /// The per-bot state-file key.
    pub(crate) fn key(&self) -> &BotKey {
        &self.key
    }

    /// The assistants this bot may wake; `None` means any.
    pub(crate) fn owners(&self) -> Option<&[String]> {
        self.owners.as_deref()
    }

    /// The operator-authored credential references that named this bot, joined
    /// for a log line or a status row. Never a token and never a digest.
    ///
    /// Test: `telegram_bot_key_never_renders_the_token`.
    pub(crate) fn label(&self) -> String {
        if self.credential_refs.is_empty() {
            return "telegram (default credential)".to_string();
        }
        self.credential_refs.join(", ")
    }

    /// Consume the bot, yielding the token the HTTP client authenticates with.
    pub(crate) fn into_token(self) -> Secret<String> {
        self.token
    }

    /// A second handle on the same bot.
    ///
    /// Why (#8190): the supervisor restarts the poller, and each attempt
    /// CONSUMES a bot to reach its token. [`Secret`] deliberately implements no
    /// `Clone` — every copy must be a deliberate act at a named site — so this
    /// is that site, and it is the only one.
    /// Test: `telegram_gateway_restarts_a_poller_that_returned_ok`.
    pub(crate) fn duplicate(&self) -> Self {
        Self {
            key: self.key.clone(),
            token: Secret::new(self.token.expose().clone()),
            owners: self.owners.clone(),
            credential_refs: self.credential_refs.clone(),
        }
    }
}
