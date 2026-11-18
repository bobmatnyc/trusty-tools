//! The operator CLI verbs: `grant`, `grants`, `revoke`, `status`, `lease`, `lockout`, `unlock`.
//!
//! Why: the operator mints from a terminal; an agent running the same CLI in
//! Bash has no `/dev/tty`, so it cannot type the dialog code (plan section 6;
//! pattern of trusty-browser's `cli_operator.rs`).
//! What: [`run`] parses the verbs and talks newline JSON to `operator.sock`.
//! `grant` opens `/dev/tty` before it touches the socket (no TTY: exit 3,
//! nothing sent), sends `grant_request`, reads the dialog code through a
//! [`CodeSource`], sends `grant_confirm` and prints the one-use `tc-` code.
//! `unlock` runs the same TTY-first flow with `unlock_request` and
//! `unlock_confirm` to clear a persisted lockout (#11460).
//! Test: `no_tty_grant_never_reaches_the_broker`, `wrong_code_refuses_and_locks_out`.

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

/// Exit code when `/dev/tty` cannot be opened.
pub const EXIT_NO_TTY: u8 = 3;

/// Where the operator types the dialog code.
pub trait CodeSource {
    /// Show `prompt` and read one line.
    fn read_code(&mut self, prompt: &str) -> io::Result<String>;
}

/// The controlling terminal, opened read-write.
pub struct DevTty(std::fs::File);

impl DevTty {
    /// Open `/dev/tty`; fails with no controlling terminal.
    pub fn open() -> io::Result<DevTty> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map(DevTty)
    }
}

impl CodeSource for DevTty {
    fn read_code(&mut self, prompt: &str) -> io::Result<String> {
        write!(self.0, "{prompt}")?;
        self.0.flush()?;
        let mut line = String::new();
        BufReader::new(&self.0).read_line(&mut line)?;
        Ok(line.trim().to_owned())
    }
}

/// One `operator.sock` connection.
pub struct OperatorClient {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl OperatorClient {
    /// Connect to `<state>/operator.sock`; the dialog may take a minute.
    pub fn connect(state: &Path) -> io::Result<OperatorClient> {
        let writer = UnixStream::connect(state.join("operator.sock"))?;
        writer.set_read_timeout(Some(Duration::from_secs(90)))?;
        let reader = BufReader::new(writer.try_clone()?);
        Ok(OperatorClient { writer, reader })
    }

    /// Send one request, read one reply.
    pub fn send(&mut self, request: &Value) -> io::Result<Value> {
        writeln!(self.writer, "{request}")?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        serde_json::from_str(&line).map_err(|e| io::Error::other(e.to_string()))
    }
}

/// The grant (or unlock) flow after the TTY is open: request, read the code, confirm.
///
/// Returns the `grant_confirm` (or `unlock_confirm`) reply, or the first refusal.
pub fn grant_flow(
    client: &mut OperatorClient,
    request: &Value,
    tty: &mut dyn CodeSource,
) -> io::Result<Value> {
    let reply = client.send(request)?;
    let Some(challenge) = reply["result"]["challenge"].as_str() else {
        return Ok(reply);
    };
    let code = tty.read_code("Code shown in the trusty-computer dialog: ")?;
    let confirm = if request["op"] == "unlock_request" {
        "unlock_confirm"
    } else {
        "grant_confirm"
    };
    client.send(&json!({"op": confirm, "challenge": challenge, "code": code}))
}

/// `30m`, `90s`, `1h` or bare seconds.
pub fn parse_duration(text: &str) -> Option<f64> {
    let (num, unit) = match text.char_indices().last()? {
        (i, 's') => (&text[..i], 1.0),
        (i, 'm') => (&text[..i], 60.0),
        (i, 'h') => (&text[..i], 3600.0),
        _ => (text, 1.0),
    };
    num.parse::<f64>().ok().map(|n| n * unit)
}

/// `$HOME/.trusty/computer`.
pub fn default_state() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".trusty/computer")
}

fn grant_request(args: &[String]) -> Result<Value, String> {
    let mut req = json!({"op": "grant_request"});
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |k: &str| it.next().cloned().ok_or(format!("{k} needs a value"));
        match a.as_str() {
            "--pid" | "--window" => {
                let key = if a == "--pid" { "pid" } else { "window_id" };
                let n: u64 = val(a)?
                    .parse()
                    .map_err(|_| format!("{a} must be a number"))?;
                req[key] = json!(n);
            }
            "--ttl" | "--idle-ttl" => {
                let key = if a == "--ttl" { "ttl" } else { "idle_ttl" };
                let v = val(a)?;
                req[key] = json!(parse_duration(&v).ok_or(format!("bad duration {v}"))?);
            }
            "--tools" => {
                let tools: Vec<String> = val(a)?.split(',').map(str::to_owned).collect();
                req["tools"] = json!(tools);
            }
            "--allow-sensitive" => req["allow_sensitive"] = json!(true),
            b if !b.starts_with('-') && req.get("bundle_id").is_none() => {
                req["bundle_id"] = json!(b)
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(req)
}

fn print_reply(reply: &Value) -> u8 {
    if reply["ok"] == json!(true) {
        println!("{}", reply.get("result").unwrap_or(&Value::Null));
        0
    } else {
        eprintln!(
            "refused: {} {}",
            reply["code"].as_str().unwrap_or("?"),
            reply["error"].as_str().unwrap_or("")
        );
        1
    }
}

/// Run one operator verb; `args` starts at the verb. Returns the exit code.
///
/// Why: one parser for every verb, and the TTY check runs before the socket.
/// Test: `no_tty_grant_never_reaches_the_broker`, `cli_verbs_drive_the_operator_socket`.
pub fn run(args: &[String]) -> u8 {
    let mut state = default_state();
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (a.as_str(), it.as_slice().first()) {
            ("--state", Some(dir)) => {
                state = PathBuf::from(dir);
                it.next();
            }
            _ => rest.push(a.clone()),
        }
    }
    let Some((verb, tail)) = rest.split_first() else {
        return 2;
    };
    let request = match (verb.as_str(), tail) {
        ("grant", tail) => match grant_request(tail) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("trusty-computer grant: {e}");
                return 2;
            }
        },
        ("revoke", [id]) => json!({"op": "revoke", "grant_id": id}),
        ("grants" | "status" | "lease" | "lockout", []) => json!({"op": verb}),
        ("unlock", []) => json!({"op": "unlock_request"}),
        _ => return 2,
    };
    // The TTY first: with no terminal, nothing reaches the broker.
    let mut tty = None;
    if verb == "grant" || verb == "unlock" {
        match DevTty::open() {
            Ok(t) => tty = Some(t),
            Err(e) => {
                eprintln!("refused: no_tty /dev/tty is required to enter the dialog code ({e})");
                return EXIT_NO_TTY;
            }
        }
    }
    let reply = OperatorClient::connect(&state).and_then(|mut c| match tty.as_mut() {
        Some(t) => grant_flow(&mut c, &request, t),
        None => c.send(&request),
    });
    match reply {
        Ok(r) => print_reply(&r),
        Err(e) => {
            eprintln!("trusty-computer: broker at {}: {e}", state.display());
            1
        }
    }
}
