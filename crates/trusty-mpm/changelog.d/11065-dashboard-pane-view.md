Added

- The Architect dashboard shows a session's last pane lines: each Decision row has a "Last output" collapsible, and the Decisions section has a "Show session" control with Refresh. Both read `GET /api/pane` only when opened or asked, show how long ago the pane was captured and whether it was truncated, and show "no live pane" when the session is not running ([#11065](https://github.com/bobmatnyc/trusty-tools/issues/11065))
