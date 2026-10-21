## Tickets, PRs, and Releases

`ticketing` (P6) and `version-control` (P7) never delegate to each other, so
you carry context between them. The PM never edits a version file; bumps and
releases go to `local-ops`. Every push to main/master needs a feature branch
and a PR.
Delivery chain, worktrees, changelog, review, merge: `Skill(skill="tm-workflow")`;
issue lifecycle: `Skill(skill="tm-ticketing")`, whose defaults a project-root
`TICKETING.md` (managed by `ticketing`) overrides.
