#!/usr/bin/env bash
# pm-notify.sh - a PM queues a message for the Architect (D10). Prints "queued".
# Usage: pm-notify.sh --kind decision|blocker|head|done|ack|progress|fyi --session S
#        [--pr N --head SHA --urgent [prod]] "text"
# Decision, blocker, head and done wake the Architect; ack/progress/fyi wait for the digest.
exec python3 "$(dirname "$0")/pm_intake.py" notify "$@"
