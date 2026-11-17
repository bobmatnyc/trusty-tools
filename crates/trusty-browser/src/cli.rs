//! The `trusty-browser` command line (`cli.py`).
//!
//! Why: operators and scripts drive the Python CLI today; the Rust binary
//! replaces it in place, so every verb keeps its name, argument, output and
//! exit code (1 for a refusal, 2 for a usage error).
//! What: Python's verbs (`serve`, `revoke`, `extension-path`, `install-host`,
//! `launchd-plist`, `install-service`, `uninstall-service`, `grants`,
//! `revoke-grant`, `revoke-grants`, `pairings`, `unpair`) plus two Rust-only
//! ones: `native-host` (the Chrome relay, no interpreter to run
//! `native_host.py`) and `migrate-keychain --from-stdin`
//! ([`crate::keychain_migrate`]). `--state` defaults to `~/.trusty/browser`
//! and is resolved as `Path.expanduser().resolve()` does.
//! Test: `cli_verbs_match_python_names_and_exit_codes`, `extension_path_prints_path_and_notice`
//! in `tests/cli.rs`.

use std::io::{IsTerminal as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::cli_operator::{operator_call, operator_frame, reply_ok};

/// `cli.RELOAD_NOTICE`, printed on stderr by `extension-path`.
pub const RELOAD_NOTICE: &str =
    "Reload Trusty Browser in each Chrome profile (chrome://extensions) to load the new version.";

/// Install and run the per-user Trusty Browser service.
#[derive(Parser)]
#[command(name = "trusty-browser")]
pub struct Cli {
    /// State directory (default ~/.trusty/browser).
    #[arg(long, global = true)]
    state: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the service in the foreground
    Serve,
    /// Remove the named enrollment
    Revoke { workflow: String },
    /// Print the unpacked Chrome extension directory
    ExtensionPath,
    /// Register both native messaging hosts for Chrome
    InstallHost(SignArgs),
    /// Print the launchd configuration without installing it
    LaunchdPlist,
    /// Start at login and register the native messaging hosts
    InstallService(SignArgs),
    /// Stop the service and remove its launchd and host registrations
    UninstallService,
    /// List unredeemed activation codes and live grants
    Grants,
    /// End one code or grant by id
    RevokeGrant { grant: String },
    /// End every code and grant
    RevokeGrants,
    /// List paired Chrome profiles
    Pairings,
    /// Remove a profile's pairing and end its grants
    Unpair { profile: String },
    /// Relay Chrome native messaging to a broker socket (run by the launchers)
    NativeHost {
        /// The broker socket to relay to.
        #[arg(long)]
        socket: PathBuf,
        /// The calling extension's origin (Chrome passes it).
        origin: String,
    },
    /// Take over the Keychain wrapping key, read from stdin only
    MigrateKeychain {
        /// Required: the key arrives on stdin (never argv or a file).
        #[arg(long, required = true)]
        from_stdin: bool,
    },
}

/// Developer ID signing flags of the install verbs ([`crate::codesign`]).
#[derive(clap::Args, Clone)]
struct SignArgs {
    /// Signing identity (default: $TRUSTY_BROWSER_SIGN_IDENTITY, else the one
    /// Developer ID Application identity of team 4JH68XUHC5)
    #[arg(long, conflicts_with = "no_sign")]
    sign_identity: Option<String>,
    /// Register the binary unsigned; every upgrade will prompt for the Keychain
    #[arg(long)]
    no_sign: bool,
}

impl SignArgs {
    /// Flag, then env, then auto-detect (`None`); `--no-sign` skips.
    fn signing(&self) -> crate::codesign::Signing {
        use crate::codesign::{IDENTITY_ENV, Signing};
        if self.no_sign {
            return Signing::Skip;
        }
        let env = std::env::var(IDENTITY_ENV).ok().filter(|v| !v.is_empty());
        Signing::Sign(self.sign_identity.clone().or(env))
    }
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_owned())
}

/// `Path.expanduser().resolve()` (non-strict): symlinks of the existing prefix resolved.
pub fn resolve(path: &Path) -> Result<PathBuf, String> {
    let mut path = path.to_path_buf();
    if let Ok(rest) = path.strip_prefix("~") {
        path = home()?.join(rest);
    }
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let mut out = PathBuf::from("/");
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => {
                out.push(n);
                if let Ok(real) = std::fs::canonicalize(&out) {
                    out = real;
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The canonical path of this binary, which the launchers and plist run.
fn own_exe() -> Result<PathBuf, String> {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn store() -> Box<dyn crate::vault::KeyStore> {
    Box::new(crate::vault::KeyringStore)
}

#[cfg(not(target_os = "macos"))]
fn store() -> Box<dyn crate::vault::KeyStore> {
    // The vault is macOS-only in 0.7.0; the broker still serves, unsealing fails.
    Box::new(crate::vault::MemoryKeyStore::new())
}

/// Env switch read by a `test-seams` build of `serve` only (see [`serve_options`]).
#[cfg(feature = "test-seams")]
pub const TEST_SEAMS_ENV: &str = "TRUSTY_BROWSER_TEST_SEAMS";

/// Stderr banner `serve` prints when the test seams are active.
#[cfg(feature = "test-seams")]
pub const TEST_SEAMS_BANNER: &str =
    "trusty-browser: TEST SEAMS ACTIVE (open enrollment check, in-memory keys); not for real use";

/// Production options; nothing in the environment can change them.
#[cfg(not(feature = "test-seams"))]
fn serve_options(state: &Path) -> crate::broker::BrokerOptions {
    crate::broker::BrokerOptions::new(state, store())
}

/// Why: the end-to-end smoke test drives the real binary through pairing,
/// which needs an open enrollment check and must never reach the Keychain.
/// What: with `TRUSTY_BROWSER_TEST_SEAMS=1`, an in-memory key store and an
/// enrollment check that admits any peer; otherwise production options.
/// The feature exists only in this crate's own test builds (dev-dependency).
/// Test: `smoke_serve_pair_connect_page_op_shutdown` in `tests/smoke_e2e.rs`.
#[cfg(feature = "test-seams")]
fn serve_options(state: &Path) -> crate::broker::BrokerOptions {
    struct Open;
    impl crate::broker::HostCheck for Open {
        fn verify(
            &self,
            _: &std::os::unix::net::UnixStream,
            _: &Path,
        ) -> Result<(), crate::peer_darwin::HostRefused> {
            Ok(())
        }
    }
    if std::env::var_os(TEST_SEAMS_ENV).is_some_and(|v| v == "1") {
        eprintln!("{TEST_SEAMS_BANNER}");
        let mut o =
            crate::broker::BrokerOptions::new(state, Box::new(crate::vault::MemoryKeyStore::new()));
        o.host_check = std::sync::Arc::new(Open);
        return o;
    }
    crate::broker::BrokerOptions::new(state, store())
}

async fn serve(state: PathBuf) -> anyhow::Result<()> {
    let options = serve_options(&state);
    let broker = crate::broker::Broker::start(options)?;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        r = tokio::signal::ctrl_c() => r?,
        _ = term.recv() => {}
    }
    broker.close().await;
    Ok(())
}

fn operator(state: &Path, verb: &str, arg: Option<&str>) -> Result<ExitCode, String> {
    let frame = operator_frame(verb, arg)?;
    let reply = operator_call(state, &frame)?;
    println!("{}", reply.dumps(true).map_err(|e| e.to_string())?);
    Ok(if reply_ok(&reply) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn migrate_keychain(state: &Path) -> Result<ExitCode, String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err("pipe the key into stdin; it is never typed or passed as an argument".into());
    }
    if !cfg!(target_os = "macos") {
        return Err("the Keychain migration requires macOS".into());
    }
    let r = crate::keychain_migrate::migrate(state, &*store(), crate::vault::SERVICE, &mut stdin)
        .map_err(|e| e.to_string())?;
    println!(
        "Migrated the Trusty Browser Keychain item: {} pairing(s) verified, old item {}, key fingerprint {}",
        r.pairings,
        if r.replaced { "replaced" } else { "absent" },
        r.fingerprint
    );
    Ok(ExitCode::SUCCESS)
}

fn launchd_layout(state: &Path) -> Result<crate::launchd::Layout, String> {
    Ok(crate::launchd::Layout {
        home: home()?,
        state: state.to_path_buf(),
        exe: own_exe()?,
        uid: crate::private::uid(),
    })
}

/// Refusal printed by a `test-seams` build asked to register itself.
#[cfg(feature = "test-seams")]
pub const TEST_BUILD_REFUSAL: &str = "test build; refusing to register";

fn dispatch(command: Command, state: &Path) -> Result<ExitCode, String> {
    let e = |e: std::io::Error| e.to_string();
    // #11440: a `cargo test` binary carries the seams; never let launchd or
    // Chrome run it through `current_exe()`.
    #[cfg(feature = "test-seams")]
    if matches!(
        command,
        Command::InstallHost(_) | Command::InstallService(_)
    ) {
        return Err(TEST_BUILD_REFUSAL.to_owned());
    }
    match command {
        Command::Grants => operator(state, "grants", None),
        Command::RevokeGrants => operator(state, "revoke-grants", None),
        Command::Pairings => operator(state, "pairings", None),
        Command::RevokeGrant { grant } => operator(state, "revoke-grant", Some(&grant)),
        Command::Unpair { profile } => operator(state, "unpair", Some(&profile)),
        Command::ExtensionPath => {
            let path = crate::assets::extension_path(state).map_err(e)?;
            println!("{}", path.display());
            // stdout stays the bare path for scripts.
            eprintln!("{RELOAD_NOTICE}");
            Ok(ExitCode::SUCCESS)
        }
        Command::LaunchdPlist => {
            let text = crate::launchd::launch_configuration(&own_exe()?, state).map_err(e)?;
            print!("{text}");
            std::io::stdout().flush().map_err(e)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::InstallHost(sign) => {
            let dir = crate::install_host::default_directory(&home()?).map_err(e)?;
            let signer = crate::codesign::Signer {
                signing: sign.signing(),
                tool: &crate::codesign::SystemCodesign,
            };
            let path = crate::install_host::install_signed(state, &dir, &own_exe()?, &signer)
                .map_err(e)?;
            println!("{}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::InstallService(_) | Command::UninstallService => {
            let signing = match &command {
                Command::InstallService(sign) => sign.signing(),
                _ => crate::codesign::Signing::Skip,
            };
            let install = matches!(command, Command::InstallService(_));
            let layout = launchd_layout(state)?;
            let signer = crate::codesign::Signer {
                signing,
                tool: &crate::codesign::SystemCodesign,
            };
            crate::launchd::manage(&layout, install, &crate::launchd::SystemLaunchctl, &signer)
                .map_err(|e| e.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Revoke { workflow } => {
            crate::cli_operator::revoke(state, &workflow, store())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Serve => {
            let rt = tokio::runtime::Runtime::new().map_err(e)?;
            rt.block_on(serve(state.to_path_buf()))
                .map_err(|e| e.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::MigrateKeychain { .. } => migrate_keychain(state),
        Command::NativeHost { .. } => Ok(ExitCode::FAILURE),
    }
}

/// Parse argv and run one verb; refusals print one line on stderr and exit 1.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::NativeHost { socket, origin } = &cli.command {
        return ExitCode::from(crate::native_host::main(socket, origin) as u8);
    }
    let state = match cli.state {
        Some(s) => resolve(&s),
        None => home().map(|h| h.join(".trusty").join("browser")),
    }
    .and_then(|s| resolve(&s));
    match state.and_then(|s| dispatch(cli.command, &s)) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::from(1)
        }
    }
}
