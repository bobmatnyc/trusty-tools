#!/usr/bin/env bash
# pm-notify.sh - a PM queues a message for the Architect (D10).
# Exit 0 + "queued-local <id>: ... delivery unconfirmed": appended and read back, not yet acked; do not
# resend (`pm_intake.py receipt <id>` exits 0 once acked, 3 until then). Exit 2: nothing appended (#10723).
# Usage: pm-notify.sh --kind decision|blocker|head|done|ack|progress|fyi --session S
#        [--pr N --head SHA --urgent [prod]] "text"
# Decision, blocker, head and done wake the Architect. Ack, progress and fyi wait for the digest.
exec python3 "$(dirname "$0")/pm_intake.py" notify "$@"
