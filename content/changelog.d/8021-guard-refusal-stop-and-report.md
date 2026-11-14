Changed
- `BASE-AGENT` tells every agent to stop and report a guard refusal, or a push that lands via a ruleset bypass, with its raw message, and never to set an override variable such as `TM_ALLOW_CROSS_BRANCH_PUSH=1` unless the brief grants it by name.
