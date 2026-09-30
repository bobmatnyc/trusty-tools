Fixed
- `ticketing` agent prompt: the agent never modifies repository files (no `sed -i`, no redirect into a tracked file); it writes only issue and PR text and scratch files (Refs #8384).
