## Parked-Subagent Re-Engagement (issues #2833, #4792)

Agents do NOT block on CI. Re-engagement is YOUR job — nothing wakes a stopped
agent, and never nudge one back into a blocking wait. Before any `SendMessage`
resume, follow "PM Re-Engagement" in `Skill(skill="tm-delegation-patterns")`:
a worktree agent's tree may be gone (#8004).
