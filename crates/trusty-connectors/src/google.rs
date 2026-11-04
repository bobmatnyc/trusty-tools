//! What the two adapters share: arguments, the Google client, selection
//! fields and the mapping of API failures to protocol error codes.
//!
//! Why: the credential must come only from the adapter's own store (B10:
//! trusty-okf passes the caller's environment list and adds nothing), and
//! every failure must reach the host as an error, never as an empty page.
//! What: [`Options`] parses `--api-base <loopback URL>` (a test seam,
//! loopback only, see `BaseClient::with_loopback_root`) and `--config
//! <path>`. [`Google`] wraps a `BaseClient`, which reads the
//! trusty-gworkspace token store under `$HOME`. [`fault`] maps an API error
//! to `auth_required`, `permission_denied`, `not_found` or `fetch_error`.
//! [`account`] and [`ceiling`] read the selection; the account label is
//! always passed explicitly, so no default profile is ever used.
//! [`run_main`] is the shared `main`.
//! Test: `google::tests`, `missing_credentials_fail_closed_before_any_write`
//! (`tests/conformance.rs`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use trusty_gworkspace::api::auth::TokenStorage;
use trusty_gworkspace::api::client::BaseClient;

use crate::config::Config;
use crate::server::{Adapter, EXIT_START, Fault, serve};

/// The adapter's listing ceiling when the selection names none (Appendix C
/// "Ceiling": 5,000 per call).
pub const DEFAULT_CEILING: usize = 5_000;

/// The largest `max_items` a selection may name (the host's default run
/// ceiling).
pub const MAX_CEILING: usize = 100_000;

/// The adapter's command-line options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// A loopback root every Google API call goes to (tests).
    pub api_base: Option<String>,
    /// The grant file, instead of the default under `$HOME`.
    pub config: Option<PathBuf>,
}

/// Whether this build carries the loopback test seam (feature
/// `loopback-seam`, #10746 S13 fix round).
pub const LOOPBACK_SEAM: bool = cfg!(feature = "loopback-seam");

impl Options {
    /// Parse `args` in this build ([`LOOPBACK_SEAM`]).
    pub fn parse(args: &[OsString]) -> Result<Options, String> {
        Options::parse_with(args, LOOPBACK_SEAM)
    }

    /// Parse `args`; `--api-base` is accepted only when `seam` is true.
    /// Errors: an unknown argument, a flag with no value, or `--api-base`
    /// in a build without the seam.
    pub fn parse_with(args: &[OsString], seam: bool) -> Result<Options, String> {
        let mut o = Options::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = || {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{} needs a value", a.to_string_lossy()))
            };
            match a.to_str() {
                // #10746 S13 fix round: no default or release build sends a
                // bearer token to a caller-chosen cleartext port.
                Some("--api-base") if !seam => {
                    return Err(
                        "--api-base is a test seam; this build lacks the loopback-seam feature"
                            .into(),
                    );
                }
                Some("--api-base") => {
                    o.api_base = Some(
                        value()?
                            .into_string()
                            .map_err(|_| "--api-base: not UTF-8")?,
                    )
                }
                Some("--config") => o.config = Some(PathBuf::from(value()?)),
                _ => return Err(format!("unknown argument {}", a.to_string_lossy())),
            }
        }
        Ok(o)
    }
}

/// The user-tier token store under `home` (trusty-gworkspace's layout).
pub fn token_store(home: &Path) -> PathBuf {
    home.join(".gworkspace-mcp").join("tokens.json")
}

/// The authenticated Google client of one adapter process.
pub struct Google {
    pub(crate) client: BaseClient,
}

impl Google {
    /// A client over the user-tier token store under `$HOME`, routed to
    /// `api_base` when set.
    ///
    /// #10746 S13 fix round: never `BaseClient::new`, whose store also
    /// reads `./.gworkspace-mcp/tokens.json` in the working directory and
    /// lets it win. Errors: no `HOME`, a refused `api_base`.
    pub fn new(opts: &Options) -> Result<Google, String> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .ok_or("HOME is not set; the adapter finds its token store under it")?;
        let storage = TokenStorage::with_path(token_store(Path::new(&home)));
        let client = BaseClient::with_storage(storage).map_err(|e| format!("{e:#}"))?;
        let client = match &opts.api_base {
            Some(root) => loopback(client, root)?,
            None => client,
        };
        Ok(Google { client })
    }
}

#[cfg(feature = "loopback-seam")]
fn loopback(client: BaseClient, root: &str) -> Result<BaseClient, String> {
    client
        .with_loopback_root(root)
        .map_err(|e| format!("{e:#}"))
}

#[cfg(not(feature = "loopback-seam"))]
fn loopback(_: BaseClient, _: &str) -> Result<BaseClient, String> {
    Err("--api-base is a test seam; this build lacks the loopback-seam feature".into())
}

/// The protocol error code of an API failure.
pub fn fault(e: &anyhow::Error) -> Fault {
    let text = format!("{e:#}");
    let code = if text.contains("no stored token for account")
        || text.contains("no default Google Workspace profile")
        || text.contains("google api error 401")
    {
        "auth_required"
    } else if text.contains("(status 403)") {
        "permission_denied"
    } else if text.contains("(status 404)") {
        "not_found"
    } else {
        "fetch_error"
    };
    Fault::new(code, text)
}

/// A selection string, trimmed; `None` when absent or blank.
pub fn text(sel: &Map<String, Value>, key: &str) -> Result<Option<String>, Fault> {
    match sel.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        Some(_) => Err(Fault::new(
            "selection_invalid",
            format!("{key} is not a string"),
        )),
    }
}

/// The account label (§2.15 item 1: no default account).
pub fn account(sel: &Map<String, Value>) -> Result<String, Fault> {
    text(sel, "account")?
        .ok_or_else(|| Fault::new("selection_required", "the selection names no account"))
}

/// The adapter's listing ceiling: `max_items` as an integer or a digit
/// string, 1 to [`MAX_CEILING`], else [`DEFAULT_CEILING`].
pub fn ceiling(sel: &Map<String, Value>) -> Result<usize, Fault> {
    let n = match sel.get("max_items") {
        None => return Ok(DEFAULT_CEILING),
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
        Some(_) => None,
    };
    n.and_then(|n| usize::try_from(n).ok())
        .filter(|n| (1..=MAX_CEILING).contains(n))
        .ok_or_else(|| {
            Fault::new(
                "selection_invalid",
                format!("max_items must be 1 to {MAX_CEILING}"),
            )
        })
}

/// The shared `main`: parse `args`, build the client and the adapter, and
/// serve stdin and stdout. Returns the exit code.
pub(crate) fn run_main<A: Adapter>(
    args: &[OsString],
    make: impl FnOnce(Google, Result<Config, String>) -> A,
) -> i32 {
    let started = Options::parse(args).and_then(|o| Ok((Google::new(&o)?, o)));
    let (google, opts) = match started {
        Ok(v) => v,
        Err(why) => {
            eprintln!("{}: {why}", A::KIND);
            return EXIT_START;
        }
    };
    let config = Config::load(opts.config.as_deref());
    serve(
        make(google, config),
        std::io::stdin(),
        std::io::stdout().lock(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sel(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn options_parse_and_refuse_unknown_arguments() {
        let o = Options::parse(&["--api-base".into(), "http://127.0.0.1:1".into()]).unwrap();
        assert_eq!(o.api_base.as_deref(), Some("http://127.0.0.1:1"));
        assert!(Options::parse(&["--token".into(), "x".into()]).is_err());
        assert!(Options::parse(&["--config".into()]).is_err());
        assert!(
            LOOPBACK_SEAM,
            "the test build carries the seam (dev self-dependency)"
        );
        let remote =
            Options::parse(&["--api-base".into(), "http://example.com:80".into()]).unwrap();
        assert!(Google::new(&remote).is_err(), "a remote root is refused");
    }

    /// #10746 S13 fix round: a build without `loopback-seam` refuses
    /// `--api-base`; `Options::parse` uses the build's own flag.
    #[test]
    fn a_build_without_the_seam_refuses_api_base() {
        let args: Vec<OsString> = vec!["--api-base".into(), "http://127.0.0.1:1".into()];
        let e = Options::parse_with(&args, false).unwrap_err();
        assert!(e.contains("loopback-seam"), "{e}");
        assert!(Options::parse_with(&["--config".into(), "c".into()], false).is_ok());
        assert_eq!(Options::parse(&args).is_ok(), LOOPBACK_SEAM);
    }

    #[test]
    fn selection_fields_fail_closed() {
        assert_eq!(account(&sel(json!({"account": " work "}))).unwrap(), "work");
        assert_eq!(
            account(&sel(json!({}))).unwrap_err().code,
            "selection_required"
        );
        assert_eq!(
            account(&sel(json!({"account": 3}))).unwrap_err().code,
            "selection_invalid"
        );
        assert_eq!(ceiling(&sel(json!({}))).unwrap(), DEFAULT_CEILING);
        assert_eq!(ceiling(&sel(json!({"max_items": "7"}))).unwrap(), 7);
        assert_eq!(ceiling(&sel(json!({"max_items": 9}))).unwrap(), 9);
        for bad in [json!(0), json!("x"), json!(MAX_CEILING + 1), json!(true)] {
            assert!(ceiling(&sel(json!({ "max_items": bad }))).is_err());
        }
    }

    #[test]
    fn api_failures_map_to_codes() {
        let f = |s: &str| fault(&anyhow::anyhow!("{s}")).code;
        assert_eq!(f("no stored token for account 'w'"), "auth_required");
        assert_eq!(f("google api error 401 Unauthorized: x"), "auth_required");
        assert_eq!(
            f("gmail messages.list failed: no (status 403)"),
            "permission_denied"
        );
        assert_eq!(f("drive file x failed: gone (status 404)"), "not_found");
        assert_eq!(f("google api error 500: x"), "fetch_error");
    }
}
