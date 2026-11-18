# trusty-computer

A broker between local AI agents and a bounded
[cua-driver](https://github.com/trycua/cua) 0.23.2 daemon on macOS. An agent
gets one operator-granted app, an AX-tree view with the menu bar removed, and
element-addressed input. Every call is checked, audited and journaled.

Status: 0.1.0 is under construction (issue #11460). The policy, redaction,
audit and journal modules exist; the bins are stubs that print usage. The build
plan is `docs/plans/trusty-computer-phase2.md`.

Live checks run only through `scripts/sandbox_computer_daemon.sh`, which gives
the broker a temporary HOME so it never reaches the live `~/.cua-driver`.
