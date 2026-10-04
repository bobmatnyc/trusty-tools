# tm-ticketing references

This folder holds detail files that `tm-ticketing` loads on demand. It is for
content too long to stay resident in `SKILL.md`, such as label and milestone
tables, worked deduplication examples, and lifecycle comment templates.

An agent without the `Skill` tool opens a file here with `Read` at
`<skills>/tm-ticketing/references/<file>.md`, where `<skills>` is the install's
absolute skills directory. An agent body spells that directory
`{{TM_SKILLS}}`, and the agent deploy resolves it; skill files are copied as
written (#7727).
