Fixed
- The seeded Architect poller's pane parser reports `turn_active`, so `turn-active.py` answers 0 (running) or 10 (idle) on a real Architect pane instead of failing with exit 3; a "Waiting for background agents" line counts only until a finished turn follows it, and agent rows never make a turn running (#10571, #10579).
- Only one Architect poller runs per inbox: a second `fleet-poll.py` exits 3 before it writes a log line or state, because the first holds `inbox/poll.lock`; the dashboard names the poller's session from `ARCHITECT_POLL_SESSION` (#10571).
