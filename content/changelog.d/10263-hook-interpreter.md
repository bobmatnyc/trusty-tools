Changed
- The `architect-project-provision` skill tells the PM to write hook commands as `/bin/bash "<abs>/scripts/notify-supervisor.sh"` and `/bin/sh "<abs>/scripts/datg-hook.sh" <session>`, never a bare script path (D73).
