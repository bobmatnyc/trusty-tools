#!/bin/sh
# Test-only stand-in for the terminal multiplexer (#12029 P2f, Architect ruling C1).
# tests/questions_live.rs copies it into a temp dir as the program a private
# runner spawns. Each call appends its argv to calls.log beside it, refuses a
# call that names no private server (-L or -S first, exit 99), then answers
# from <verb>.out (stdout) and <verb>.exit (exit code) beside it.
d=$(dirname "$0")
printf '%s\n' "$*" >> "$d/calls.log"
case "$1" in -L|-S) ;; *) echo "no private server" >&2; exit 99 ;; esac
verb=$3
[ -f "$d/$verb.out" ] && cat "$d/$verb.out"
[ -f "$d/$verb.exit" ] && exit "$(cat "$d/$verb.exit")"
exit 0
