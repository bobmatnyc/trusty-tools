---
name: tm-doctor
description: Run a full trusty-mpm system diagnostic checking instructions, agents, skills, memory, and search services
---

# /tm-doctor

Run `tm doctor` via shell to get the full diagnostic report:

```bash
tm doctor
```

This checks:
- Instruction pipeline (last-instructions.md)
- Agent deployment (~/.claude/agents/)
- Skill deployment (~/.claude/skills/)
- Memory service (trusty-memory on :3038)
- Search service (trusty-search on :7878) and trusty-mpm index

Report any Fail or Warn items to the user with suggested fixes.
