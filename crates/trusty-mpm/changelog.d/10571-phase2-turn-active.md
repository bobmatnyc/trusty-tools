Fixed
- The seeded Architect poller's pane parser reports `turn_active`, so `turn-active.py` answers 0 (running) or 10 (idle) on a real Architect pane instead of failing with exit 3; a "Waiting for background agents" line counts only until a finished turn follows it, and agent rows never make a turn running (#10571, #10579).
- Only one Architect poller runs per inbox: a second `fleet-poll.py` exits 3 before it writes a log line or state, because the first holds `inbox/poll.lock`; the dashboard names the poller's session from `ARCHITECT_POLL_SESSION` (#10571).
- The seeded Architect poller sends no key to the Architect while `self-clear.sh` runs (a held or unreadable `inbox/self-clear.pid`, or a live run), so a wake can no longer put `/clear` into a live turn (#10571, #10599).
