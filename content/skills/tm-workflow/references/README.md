# tm-workflow references

This folder holds detail files that `tm-workflow` loads on demand. It is for
content too long to stay resident in `SKILL.md`, such as the per-phase gate
checklists, the worktree provisioning commands, and PR body templates.

An agent without the `Skill` tool opens a file here with `Read` at
`<skills>/tm-workflow/references/<file>.md`, where `<skills>` is the install's
absolute skills directory. An agent body spells that directory
`{{TM_SKILLS}}`, and the agent deploy resolves it; skill files are copied as
written (#7727).
