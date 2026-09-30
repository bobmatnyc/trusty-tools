#!/usr/bin/env bash
#
# check_no_tcp_listeners.sh — no crate binds a TCP listener outside the
# allowlist (issue #8926, ADR-0032).
#
# Why: ADR-0032 makes trusty-console the only TCP surface; every other service
#   speaks UDS. The migration slices kept TCP as an "interim" with nothing
#   failing on a bind, so the interim never ended. This gate fails on any new
#   TCP bind, and on an allowlist row whose bind is gone, so a finished
#   migration has to delete its row.
#
# What: scans every tracked `crates/**/*.rs` file. String contents and
#   comments are blanked first, so prose and messages never match. A SITE is a
#   code line naming any of:
#     TcpListener  TcpSocket  axum_server  warp::serve  HttpServer::new
#     <Tcp*|Server|HttpServer>::bind / ::try_bind   .listen(
#   `axum::serve` is not a site: it also serves a UnixListener, and the TCP
#   bind that feeds it is. A `SocketAddr` in listen config is not a site for
#   the same reason: it only listens through one of the calls above.
#
#   Test rule: inside a test context — a path under `tests/` or `benches/`, a
#   basename like `tests.rs`, `*_tests.rs`, `tests_*.rs`, `test_*.rs`, a
#   `test_support/` directory, or the braces of an item under `#[cfg(test)]`,
#   `#[test]` or `#[tokio::test]` — only a bind call is a site (`use` lines and
#   types are not), and it is exempt when its statement binds the ephemeral
#   loopback port: "127.0.0.1:0", ("127.0.0.1", 0), (Ipv4Addr::LOCALHOST, 0)
#   or ([127, 0, 0, 1], 0). One more exemption: a bind the test asserts FAILS
#   (`bind(a).is_err()`, or `let x = bind(a);` then `x.is_err()` within 3
#   lines), because a failed bind never listens. A test that binds any other
#   address is a finding.
#
#   A site is excused only by a `paths` entry of
#   crates/trusty-mpm/src/daemon/tcp_listener_allowlist.tsv, the same file
#   `tm doctor`'s `tcp_listeners` row embeds. The file's header documents the
#   columns. A path that excuses no site fails the gate.
#
#   Scan floor: zero `.rs` files enumerated is a failure, never a pass.
#
# Known limits — textual, not a parser: a bind reached through a macro or a
#   re-exported alias that renames `TcpListener` is invisible; `#[cfg(test)]`
#   tracking counts braces after blanking strings, so a brace inside a macro
#   token tree it cannot see through may misplace a test region.
#
# Usage: bash scripts/check_no_tcp_listeners.sh
#   TCP_LISTENER_ALLOWLIST=<path> overrides the allowlist (the selftest uses it).
# Exit:  0 clean; 1 on a finding, a stale or malformed allowlist row, or a
#        scan-floor breach.
# Test:  scripts/check_no_tcp_listeners_selftest.sh.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); git and perl.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

ALLOW="${TCP_LISTENER_ALLOWLIST:-crates/trusty-mpm/src/daemon/tcp_listener_allowlist.tsv}"
if [ ! -f "$ALLOW" ]; then
  echo "check_no_tcp_listeners: allowlist $ALLOW not found" >&2
  exit 1
fi

FILE_LIST="$(mktemp)"
trap 'rm -f "$FILE_LIST"' EXIT
git ls-files -- 'crates/*.rs' > "$FILE_LIST"

TCP_ALLOW="$ALLOW" TCP_FILES="$FILE_LIST" perl -e '
use strict;
use warnings;

my $allow_path = $ENV{TCP_ALLOW};
my (@allow, @errors, @findings);

open(my $af, "<", $allow_path) or die "open $allow_path: $!";
while (my $row = <$af>) {
    chomp $row;
    $row =~ s/\r\z//;
    next if $row =~ /^\s*(?:#|\z)/;
    my @f = split /\t/, $row, -1;
    my $at = "$allow_path:$.";
    if (@f != 6) {
        push @errors, "$at: want 6 tab-separated fields (kind crate processes paths issue reason), got " . scalar(@f);
        next;
    }
    my ($kind, $crate, $procs, $paths, $issue, $reason) = @f;
    if ($kind !~ /^(?:permanent|temporary|source-only)\z/) {
        push @errors, "$at: kind `$kind` is not permanent, temporary or source-only";
        next;
    }
    push @errors, "$at: permanent row must cite an ADR (ADR-NNNN), got `$issue`"
        if $kind eq "permanent" && $issue !~ /^ADR-\d{4}\z/;
    push @errors, "$at: $kind row must cite an issue (#N), got `$issue`"
        if $kind ne "permanent" && $issue !~ /^#\d+\z/;
    push @errors, "$at: source-only row names processes `$procs`; use -"
        if $kind eq "source-only" && $procs ne "-";
    push @errors, "$at: $kind row names no process"
        if $kind ne "source-only" && ($procs eq "-" || $procs eq "");
    push @errors, "$at: empty crate or reason" if $crate eq "" || $reason eq "";
    for my $p (split /,/, $paths) {
        if ($p !~ m{^crates/[^\s]+\z}) {
            push @errors, "$at: path `$p` is not a repo-relative crates/ path";
            next;
        }
        push @allow, { path => $p, kind => $kind, crate => $crate, issue => $issue, used => 0 };
    }
}
close $af;

my $site = qr/\bTcpListener\b|\bTcpSocket\b|\baxum_server\b|\bwarp::serve\b|\bHttpServer::new\b|\b(?:Tcp\w*|Server|HttpServer)::(?:try_)?bind\b|\.listen\s*\(/;
my $bind_call = qr/\bTcpListener::bind\b|\bTcpSocket::new_v[46]\b|\baxum_server::|\bwarp::serve\b|\bHttpServer::new\b|\b(?:Tcp\w*|Server|HttpServer)::(?:try_)?bind\b|\.listen\s*\(/;
my $ephemeral = qr/"127\.0\.0\.1:0"|\(\s*"127\.0\.0\.1"\s*,\s*0\s*\)|\(\s*(?:std::net::)?Ipv4Addr::LOCALHOST\s*,\s*0\s*\)|\[\s*127\s*,\s*0\s*,\s*0\s*,\s*1\s*\]\s*,\s*0\b/;
my $test_attr = qr/#\[\s*(?:cfg\s*\((?![^\]]*\bnot\s*\(\s*test\b)[^\]]*\btest\b|(?:tokio::)?test\b)/;

# blank: replace comment and string-literal contents with spaces, keeping the
# quotes and every newline, so line numbers survive and prose never matches.
sub blank {
    my ($text) = @_;
    my $out = "";
    pos($text) = 0;
    while (pos($text) < length $text) {
        if ($text =~ m{\G(//[^\n]*)}gc || $text =~ m{\G(/\*.*?\*/)}gcs) {
            (my $c = $1) =~ s/[^\n]/ /g;
            $out .= $c;
        } elsif ($text =~ m{\G(b?r(\#*)")}gc) {
            my ($open, $hashes) = ($1, $2);
            my $close = "\"$hashes";
            my $end = index($text, $close, pos($text));
            $end = length($text) if $end < 0;
            (my $body = substr($text, pos($text), $end - pos($text))) =~ s/[^\n]/ /g;
            $out .= $open . $body . $close;
            pos($text) = $end + length($close);
        } elsif ($text =~ m{\G(b?")((?:\\.|[^"\\])*)(")?}gcs) {
            my ($open, $body, $close) = ($1, $2, $3 // "");
            $body =~ s/[^\n]/ /g;
            $out .= $open . $body . $close;
        } elsif ($text =~ m{\G(b?\x27(?:\\.[^\x27\n]{0,8}|[^\\\x27\n])\x27)}gc) {
            $out .= " " x length($1);
        } elsif ($text =~ m{\G([^/"\x27br]+|.)}gcs) {
            $out .= $1;
        }
    }
    return $out;
}

open(my $lf, "<", $ENV{TCP_FILES}) or die "open file list: $!";
my @paths = grep { length } map { chomp; $_ } <$lf>;
close $lf;

my ($scanned, $sites, $excused) = (0, 0, 0);
for my $path (@paths) {
    open(my $fh, "<", $path) or die "open $path: $!";
    local $/;
    my $raw = <$fh>;
    close $fh;
    $scanned++;
    next unless $raw =~ $site;

    my @raw_lines = split /\n/, $raw, -1;
    my @code = split /\n/, blank($raw), -1;
    my ($base) = $path =~ m{([^/]+)\z};
    my $test_file = $path =~ m{(?:^|/)(?:tests|benches|test_support)/}
        || $base =~ /^(?:tests?|tests?_\w+|\w+_tests?)\.rs\z/;

    # Test regions: the brace depth at which a #[cfg(test)]/#[test] item
    # opened. A `;` outside ( ) and [ ] before the item`s `{` ends it
    # (`#[cfg(test)] mod tests;`, a `use`), so no region opens.
    my ($depth, $parens, $pending, @regions) = (0, 0, 0);
    for my $i (0 .. $#code) {
        my $l = $code[$i];
        my $in_test = $test_file || @regions;
        my $attr_end = -1;
        while ($l =~ /$test_attr/g) { $attr_end = pos($l); }
        my $armed = $attr_end >= 0;
        while ($l =~ /([{};()\[\]])/g) {
            my ($ch, $at) = ($1, pos($l));
            if ($armed && $at > $attr_end) { $pending = 1; $armed = 0; }
            if ($ch eq "(" || $ch eq "[") {
                $parens++;
            } elsif ($ch eq ")" || $ch eq "]") {
                $parens-- if $parens > 0;
            } elsif ($ch eq "{") {
                if ($pending) { push @regions, $depth; $pending = 0; }
                $depth++;
            } elsif ($ch eq "}") {
                $depth--;
                pop @regions while @regions && $regions[-1] >= $depth;
            } elsif ($pending && $parens == 0) {
                $pending = 0;
            }
        }
        $pending = 1 if $armed;
        $in_test ||= @regions;
        next unless $l =~ $site;
        my $line_no = $i + 1;
        if ($in_test) {
            next unless $l =~ $bind_call;
            my $col = $-[0];    # blanking keeps columns, so this indexes the raw line
            my $hi = $i + 4 > $#raw_lines ? $#raw_lines : $i + 4;
            my $stmt = join " ", substr($raw_lines[$i], $col), @raw_lines[$i + 1 .. $hi];
            $stmt =~ s/[;{].*//s;
            next if $stmt =~ $ephemeral;
            # A bind the test asserts FAILS never listens: `bind(a).is_err()`,
            # or `let x = …bind(a);` with `x.is_err()` in the next 3 lines.
            next if $stmt =~ /^[\w:]+\s*\((?:[^()]|\([^()]*\))*\)\s*\.\s*is_err\s*\(\s*\)/;
            if (substr($raw_lines[$i], 0, $col) =~ /\blet\s+(?:mut\s+)?(\w+)\s*=\s*[\w:]*\z/) {
                my $name = $1;
                my $to = $i + 3 > $#raw_lines ? $#raw_lines : $i + 3;
                next if grep { /\b\Q$name\E\s*\.\s*is_err\s*\(\s*\)/ } @raw_lines[$i + 1 .. $to];
            }
        }
        $sites++;
        my $hit;
        for my $a (@allow) {
            my $p = $a->{path};
            if ($path eq $p || ($p =~ m{/\z} && index($path, $p) == 0)) { $hit = $a; last; }
        }
        if ($hit) {
            $hit->{used}++;
            $excused++;
            next;
        }
        (my $src = $raw_lines[$i]) =~ s/^\s+//;
        push @findings, sprintf("%s:%d: %s\n      %s", $path, $line_no,
            $in_test ? "test binds a non-ephemeral address" : "TCP listener site", $src);
    }
}

my $status = 0;
if (!$scanned) {
    print STDERR "check_no_tcp_listeners: SCAN FLOOR: no crates/**/*.rs file was enumerated\n";
    exit 1;
}
if (@errors) {
    print STDERR "check_no_tcp_listeners: malformed allowlist row(s):\n";
    print STDERR "  $_\n" for @errors;
    $status = 1;
}
for my $a (@allow) {
    next if $a->{used};
    print STDERR "check_no_tcp_listeners: STALE allowlist path (excuses no site): $a->{path} ($a->{crate}, $a->{issue}) — delete it from $allow_path\n";
    $status = 1;
}
if (@findings) {
    print STDERR "check_no_tcp_listeners: TCP listener site(s) outside the allowlist (ADR-0032, #8926):\n";
    print STDERR "  $_\n" for @findings;
    print STDERR "\nOnly trusty-console may listen on TCP. Serve over UDS (trusty_common::uds).\n";
    print STDERR "A test may bind 127.0.0.1:0 only. An exception needs an allowlist row citing\n";
    print STDERR "an open issue in $allow_path.\n";
    $status = 1;
}
if ($status == 0) {
    printf "check_no_tcp_listeners: OK: %d file(s) scanned, %d TCP site(s), all under %d allowlisted path(s)\n",
        $scanned, $sites, scalar(@allow);
}
exit $status;
'
