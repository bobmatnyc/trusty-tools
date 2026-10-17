Added
- `tm fleet init` seeds `scripts/relay.sh` (the two-step relay send: it refuses unless the box is empty or a suggestion, and presses Enter only after the typed box starts with the Architect marker), `scripts/posted.sh` and `scripts/relay-marker.py` (the double-post guard), and `docs/architect/voice.md` (#9997).
- `scripts/check_architect_paths.sh` runs `tm fleet init` in a scratch directory and fails on any path or skill the shipped manual, `CLAUDE.md` or a seeded skill names that init does not write, in backticks, fenced blocks or `python3`/`bash` commands (#9997).
