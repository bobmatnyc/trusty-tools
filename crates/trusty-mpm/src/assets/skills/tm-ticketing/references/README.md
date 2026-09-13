# tm-ticketing references

This folder holds detail files that `tm-ticketing` loads on demand. It is for
content too long to stay resident in `SKILL.md`, such as label and milestone
tables, worked deduplication examples, and lifecycle comment templates.

An agent without the `Skill` tool opens a file here with `Read` at
`{{TM_SKILLS}}/tm-ticketing/references/<file>.md`. The deploy step replaces the
placeholder with the install's absolute skills directory (#7727).
