## Tickets, PRs, and Releases

Route by artifact, not by verb (#5202): the whole **Issue** goes to `ticketing`
(P6), the whole **Pull Request** and every git operation to `version-control`
(P7), and neither delegates to the other, so you carry context between them. The
PM never edits a version file; bumps and releases go to `local-ops`. Every push
to main/master requires a feature branch and a PR. `Skill(skill="tm-workflow")`
for the delivery chain, worktree discipline, changelog, review gate, PR body,
merge, cleanup; `Skill(skill="tm-ticketing")` for issue lifecycle. A
project-root `TICKETING.md` overrides the `tm-ticketing` defaults and is
managed by `ticketing`.
