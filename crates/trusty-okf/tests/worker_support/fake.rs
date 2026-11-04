//! Test-double extraction workers for slice S10 of #10746 (SPEC-OKF-01
//! Appendix B "Fixtures"): a Perl script that speaks the Appendix B
//! protocol and misbehaves on request, plus process checks and the
//! per-test timer. Shared by `tests/worker_criteria.rs` and the unit tests
//! in `src/extract/worker/worker_tests.rs`.
//!
//! Per-item behaviour is keyed on the item's first bytes: `HANG` sleeps,
//! `CRASH` exits 3, `FLOOD` answers text one byte over the output limit,
//! `BIG` declares a 2 GiB frame, `LIE` echoes a wrong input hash, `ERR`
//! answers `extract_error`, `NOTEXT` answers `no_text_layer`, `STDERR`
//! writes 1 MiB to stderr first, `SPAM` answers then streams 2000 more
//! frames (logging `sent <n>` to the pidfile), `SPIN` burns CPU, `ALLOC`
//! allocates 1.25 GiB, `FORK` forks a `setsid` grandchild named
//! `okf-setsid-<config>` and answers whether the fork worked. Any other item
//! is echoed as its text.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// One test double; see the module doc.
#[derive(Debug, Clone)]
pub struct Fake {
    /// Whole-process behaviour: `ok`, `no_read`, `no_hello`, `bad_first`,
    /// `ignore_shutdown`, `env` (text is the environment and directory) or
    /// `probe` (text says whether a socket and a file outside opened).
    pub mode: &'static str,
    pub protocol: u32,
    pub version: String,
    pub xberg: String,
    pub config: String,
    pub deterministic: bool,
    pub sandbox: String,
    /// Fork a child that ignores SIGTERM and sleeps.
    pub child: bool,
    /// Append this process's PID, the child's, and `item` per request.
    pub pidfile: Option<PathBuf>,
    pub port: u16,
    pub secret: Option<PathBuf>,
    /// Answer this text instead of echoing the item.
    pub text: Option<String>,
    /// The `fields` object, as JSON.
    pub fields: String,
}

impl Default for Fake {
    fn default() -> Self {
        Fake {
            mode: "ok",
            protocol: 1,
            version: "1.0.0".into(),
            xberg: "1.4.0".into(),
            config: "cfg1".into(),
            deterministic: true,
            sandbox: "none".into(),
            child: false,
            pidfile: None,
            port: 0,
            secret: None,
            text: None,
            fields: "{}".into(),
        }
    }
}

fn q(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn qp(p: &Option<PathBuf>) -> String {
    q(&p.as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default())
}

const BODY: &str = r#"
sub rd { my ($n) = @_; my $b = ''; while (length($b) < $n) { my $r = sysread(STDIN, $b, $n - length($b), length($b)); return undef unless $r; } return $b; }
sub rframe { my $l = rd(4); return undef unless defined $l; my $h = rd(unpack('N', $l)); return undef unless defined $h; return decode_json($h); }
sub wraw { my ($b) = @_; while (length $b) { my $w = syswrite(STDOUT, $b); exit 9 unless defined $w; substr($b, 0, $w, ''); } }
sub wframe { my $j = JSON::PP->new->utf8->canonical->encode($_[0]); wraw(pack('N', length $j) . $j); }
sub idle { sleep 1 while 1; }
sub note_pid { return unless length $o{pidfile}; open(my $f, '>>', $o{pidfile}) or die; print $f "$_[0]\n"; close $f; }
note_pid($$);
if ($o{child}) {
  my $pid = fork();
  if (defined $pid && $pid == 0) { $SIG{TERM} = 'IGNORE'; close STDIN; close STDOUT; close STDERR; idle(); }
  note_pid($pid);
}
idle() if $o{mode} eq 'no_read';
my $h = rframe(); exit 0 unless defined $h;
idle() if $o{mode} eq 'no_hello';
if ($o{mode} eq 'bad_first') { wframe({type => 'ok'}); idle(); }
wframe({type => 'hello', protocol => $o{protocol} + 0, worker_version => $o{version},
  xberg_version => $o{xberg}, config_hash => $o{config}, formats => ['pdf'],
  deterministic => ($o{deterministic} ? JSON::PP::true : JSON::PP::false),
  features => [], sandbox => $o{sandbox}});
while (1) {
  my $m = rframe(); exit 0 unless defined $m;
  if ($m->{type} eq 'shutdown') {
    if ($o{mode} eq 'ignore_shutdown') { $SIG{TERM} = 'IGNORE'; idle(); }
    exit 0;
  }
  my $body = rd($m->{byte_len}); $body = '' unless defined $body;
  note_pid('item');
  idle() if $body =~ /^HANG/;
  if ($body =~ /^SPIN/) { 1 while 1; }
  # The size is computed at run time: a constant `'x' x (1 << 30)` is folded
  # at compile time and would allocate 1 GiB in every double at startup.
  if ($body =~ /^ALLOC/) { my $n = length($body) << 28; my $big = 'x' x $n; $body = length($big); }
  if ($body =~ /^FORK/) {
    my $pid = fork();
    if (defined $pid && $pid == 0) { POSIX::setsid(); $0 = "okf-setsid-$o{config}"; idle(); }
    $o{text} = defined $pid ? 'fork=ok' : 'fork=fail';
  }
  exit 3 if $body =~ /^CRASH/;
  if ($body =~ /^BIG/) { wraw(pack('N', 0x7fffffff)); idle(); }
  if ($body =~ /^STDERR/) { print STDERR ('e' x 65536) for 1 .. 16; }
  if ($body =~ /^ERR/) { wframe({type => 'error', request_id => $m->{request_id}, code => 'extract_error', message => 'bad file'}); next; }
  if ($body =~ /^NOTEXT/) { wframe({type => 'error', request_id => $m->{request_id}, code => 'no_text_layer', message => ''}); next; }
  my $text = length($o{text}) ? $o{text} : $body;
  $text = 'x' x ($m->{limits}{output_bytes} + 1) if $body =~ /^FLOOD/;
  if ($o{mode} eq 'env') {
    $text = join('', map { "$_=$ENV{$_}\n" } sort keys %ENV) . 'CWD=' . Cwd::getcwd() . "\n";
    opendir(my $d, '.') or die; my @e = grep { !/^\.\.?$/ } readdir($d);
    $text .= 'ENTRIES=' . scalar(@e) . "\n";
  }
  if ($o{mode} eq 'probe') {
    my $s = IO::Socket::INET->new(PeerAddr => '127.0.0.1', PeerPort => $o{port}, Proto => 'tcp', Timeout => 2);
    my $file = open(my $fh, '<', $o{secret}) ? 'ok' : 'fail';
    $text = 'socket=' . ($s ? 'ok' : 'fail') . ";file=$file";
  }
  my $sha = $body =~ /^LIE/ ? ('0' x 64) : $m->{sha256};
  wframe({type => 'ok', request_id => $m->{request_id}, input_sha256 => $sha, text => $text,
    fields => decode_json($o{fields}), notes => []});
  if ($body =~ /^SPAM/) {
    for my $i (1 .. 2000) {
      wframe({type => 'ok', request_id => $m->{request_id}, input_sha256 => $sha, text => 'y' x 16384, fields => {}, notes => []});
      note_pid("sent $i");
    }
  }
}
"#;

impl Fake {
    /// The script text.
    pub fn script(&self) -> String {
        let header = format!(
            "#!/usr/bin/perl\nuse strict; use warnings; use JSON::PP; use Cwd (); use IO::Socket::INET; use POSIX ();\n\
             binmode STDIN; binmode STDOUT; binmode STDERR;\n\
             our %o = (mode => {}, protocol => {}, version => {}, xberg => {}, config => {}, \
             deterministic => {}, sandbox => {}, child => {}, pidfile => {}, port => {}, \
             secret => {}, text => {}, fields => {});\n",
            q(self.mode),
            self.protocol,
            q(&self.version),
            q(&self.xberg),
            q(&self.config),
            u8::from(self.deterministic),
            q(&self.sandbox),
            u8::from(self.child),
            qp(&self.pidfile),
            self.port,
            qp(&self.secret),
            q(self.text.as_deref().unwrap_or("")),
            q(&self.fields),
        );
        format!("{header}{BODY}")
    }

    /// Write the script as an executable `name` under `dir`; returns its
    /// canonical path.
    pub fn write(&self, dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, self.script()).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.canonicalize().unwrap()
    }
}

/// Run `f` on a thread and fail the test if it takes longer than `limit`
/// (the deadlines under test plus 5 s, #10746 S10).
pub fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(limit) {
        Ok(v) => v,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("exceeded the {limit:?} test timer"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the timed body panicked"),
    }
}

/// Whether no process is left in group `pgid`, polling for up to 3 s (an
/// orphaned grandchild is reaped by init, not by the host).
pub fn group_gone(pgid: i32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::killpg;
    use nix::unistd::Pid;
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        if killpg(Pid::from_raw(pgid), None) == Err(Errno::ESRCH) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether every PID in `pidfile` is gone (not even a zombie), polling for
/// up to 3 s.
pub fn pids_gone(pidfile: &Path) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    let pids: Vec<i32> = std::fs::read_to_string(pidfile)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    assert!(!pids.is_empty(), "the double wrote no PID");
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        if pids
            .iter()
            .all(|p| kill(Pid::from_raw(*p), None) == Err(Errno::ESRCH))
        {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
