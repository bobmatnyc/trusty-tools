# tm-workflow references

This folder holds detail files that `tm-workflow` loads on demand. It is for
content too long to stay resident in `SKILL.md`, such as the per-phase gate
checklists, the worktree provisioning commands, and PR body templates.

An agent without the `Skill` tool opens a file here with `Read` at
`{{TM_SKILLS}}/tm-workflow/references/<file>.md`. The deploy step replaces the
placeholder with the install's absolute skills directory (#7727).
