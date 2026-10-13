//! What tm and its `__exec-stub` child share for `tm secrets exec` (#9621).
//!
//! Why: tm and the stub are one binary in two processes. They agree on the
//! key set, on the stub's argv and on the report the stub sends back, and a
//! drift between the two sides would run a command with a partial env.
//! What: [`ExecSpec`] (the `--env` and `--stdin` keys, validated once on
//! each side), [`stub_args`] (the stub's argv, built from names and fd
//! numbers only, so no token or value can reach it), [`is_token`], and the
//! report frame: [`encode_ok`], [`encode_fail`] and [`decode_report`].
//! Test: `exec_tests.rs` beside this file.
//!
//! Governing document: DOC-74 §10.5, §15.8.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;

use anyhow::{anyhow, bail};
use trusty_secrets::SecretKey;

// #9071: the variable naming the inherited token fd comes from trusty-secrets,
// so tm and the tier-3 client cannot drift apart.
pub(crate) use trusty_secrets::api::TOKEN_FD_ENV;

/// The hidden `tm secrets` verb the stub runs as.
pub(crate) const STUB_VERB: &str = "__exec-stub";

/// Longest accepted variable name.
const MAX_NAME_LEN: usize = 128;

/// A grant token is 32 random bytes as lowercase hex (trusty-secrets `grant`).
const TOKEN_LEN: usize = 64;

/// The most report bytes tm reads before it refuses the report.
pub(crate) const MAX_REPORT_BYTES: u64 = 16 << 20;

const MAGIC: &[u8; 4] = b"TSX1";
const TAG_OK: u8 = b'O';
const TAG_FAIL: u8 = b'F';
const END: &[u8; 3] = b"END";

/// One `--env NAME=KEY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvBinding {
    /// The variable the command sees.
    pub(crate) name: String,
    /// The key whose value it holds.
    pub(crate) key: SecretKey,
}

/// The keys one `exec` injects, and the command it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecSpec {
    /// `--env` bindings, in argument order.
    pub(crate) env: Vec<EnvBinding>,
    /// The `--stdin` key.
    pub(crate) stdin: Option<SecretKey>,
    /// The command and its arguments; never empty.
    pub(crate) command: Vec<String>,
}

impl ExecSpec {
    /// Validate the `exec` arguments.
    ///
    /// Why: an argument may be a value typed in the wrong place, so no error
    /// quotes one.
    /// What: each `env` item is `NAME=KEY`, `NAME` a POSIX variable name (not
    /// [`TOKEN_FD_ENV`], not repeated) and `KEY` a valid key; `stdin` is a
    /// valid key; at least one key and one command word are named.
    /// Test: `exec_spec_refuses_bad_bindings_without_quoting_them`.
    pub(crate) fn parse(
        env: &[&str],
        stdin: Option<&str>,
        command: Vec<String>,
    ) -> anyhow::Result<Self> {
        let mut bindings: Vec<EnvBinding> = Vec::with_capacity(env.len());
        for item in env {
            let (name, key) = item
                .split_once('=')
                .ok_or_else(|| anyhow!("tm secrets exec: --env takes NAME=KEY"))?;
            if !is_var_name(name) || name == TOKEN_FD_ENV {
                bail!("tm secrets exec: an --env NAME is not a usable variable name");
            }
            if bindings.iter().any(|b| b.name == name) {
                bail!("tm secrets exec: an --env NAME is given twice");
            }
            let key = SecretKey::new(key)
                .map_err(|_| anyhow!("tm secrets exec: an --env KEY is not a valid key name"))?;
            bindings.push(EnvBinding {
                name: name.to_owned(),
                key,
            });
        }
        let stdin = stdin
            .map(SecretKey::new)
            .transpose()
            .map_err(|_| anyhow!("tm secrets exec: the --stdin KEY is not a valid key name"))?;
        if bindings.is_empty() && stdin.is_none() {
            bail!("tm secrets exec: name at least one --env NAME=KEY or --stdin KEY");
        }
        if command.is_empty() {
            bail!("tm secrets exec: name the command after --");
        }
        Ok(Self {
            env: bindings,
            stdin,
            command,
        })
    }

    /// Every key named, once each: the grant's key set and the resolve order.
    pub(crate) fn keys(&self) -> BTreeSet<SecretKey> {
        self.env
            .iter()
            .map(|b| b.key.clone())
            .chain(self.stdin.clone())
            .collect()
    }
}

/// Whether `name` is `[A-Za-z_][A-Za-z0-9_]*`, at most 128 characters.
fn is_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= MAX_NAME_LEN
        && (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `text` is a well-formed grant token: 64 lowercase hex digits.
///
/// Test: `token_check_refuses_short_and_padded_tokens`.
pub(crate) fn is_token(text: &str) -> bool {
    text.len() == TOKEN_LEN
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The stub's argv after the program: names and fd numbers only.
///
/// Why: #9621 — no token and no value may reach any argv. This function is
/// never handed either, so it cannot place one.
/// What: `secrets __exec-stub --socket <path> --token-fd <n> --report-fd <n>`,
/// each `--env NAME=KEY`, the optional `--stdin KEY`, then `--` and the command.
/// Test: `stub_args_carry_names_and_fds_only`.
pub(crate) fn stub_args(
    socket: &Path,
    token_fd: i32,
    report_fd: i32,
    spec: &ExecSpec,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "secrets".into(),
        STUB_VERB.into(),
        "--socket".into(),
        socket.as_os_str().to_owned(),
        "--token-fd".into(),
        token_fd.to_string().into(),
        "--report-fd".into(),
        report_fd.to_string().into(),
    ];
    for binding in &spec.env {
        args.push("--env".into());
        args.push(format!("{}={}", binding.name, binding.key).into());
    }
    if let Some(key) = &spec.stdin {
        args.push("--stdin".into());
        args.push(key.to_string().into());
    }
    args.push("--".into());
    args.extend(spec.command.iter().map(OsString::from));
    args
}

/// What the stub reported.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Report {
    /// Every key resolved: key → value.
    Resolved(BTreeMap<SecretKey, String>),
    /// A step failed before exec: the key (when one failed) and fixed text.
    Failed {
        /// The key that failed, if the failure was a resolve.
        key: Option<SecretKey>,
        /// Fixed text; never a value.
        reason: String,
    },
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&bytes[..len as usize]);
}

/// The report for every key resolved, in key order.
pub(crate) fn encode_ok(values: &BTreeMap<SecretKey, String>) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.push(TAG_OK);
    let count = u32::try_from(values.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_be_bytes());
    for (key, value) in values {
        push_bytes(&mut out, key.as_str().as_bytes());
        push_bytes(&mut out, value.as_bytes());
    }
    out.extend_from_slice(END);
    out
}

/// The report for a failure before exec.
pub(crate) fn encode_fail(key: Option<&SecretKey>, reason: &str) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.push(TAG_FAIL);
    push_bytes(&mut out, key.map_or("", SecretKey::as_str).as_bytes());
    push_bytes(&mut out, reason.as_bytes());
    out.extend_from_slice(END);
    out
}

/// A cursor over the report bytes.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }

    fn u32(&mut self) -> Option<usize> {
        let bytes: [u8; 4] = self.take(4)?.try_into().ok()?;
        usize::try_from(u32::from_be_bytes(bytes)).ok()
    }

    fn text(&mut self) -> Option<String> {
        let len = self.u32()?;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}

/// Decode a whole report; `None` when it is absent, cut short or malformed.
///
/// Why: a stub that died, or a report cut mid-frame, must read as a failure,
/// never as a success with fewer keys.
/// Test: `report_round_trips_and_a_cut_report_is_none`.
pub(crate) fn decode_report(bytes: &[u8]) -> Option<Report> {
    let mut reader = Reader(bytes);
    if reader.take(MAGIC.len())? != MAGIC {
        return None;
    }
    let report = match reader.take(1)?[0] {
        TAG_OK => {
            let count = reader.u32()?;
            let mut values = BTreeMap::new();
            for _ in 0..count {
                let key = SecretKey::new(&reader.text()?).ok()?;
                let value = reader.text()?;
                values.insert(key, value);
            }
            Report::Resolved(values)
        }
        TAG_FAIL => {
            let key = reader.text()?;
            let key = if key.is_empty() {
                None
            } else {
                Some(SecretKey::new(&key).ok()?)
            };
            Report::Failed {
                key,
                reason: reader.text()?,
            }
        }
        _ => return None,
    };
    (reader.take(END.len())? == END && reader.0.is_empty()).then_some(report)
}
