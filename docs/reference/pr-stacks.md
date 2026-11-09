# PR Stacks Reference

Status: trial. Owner ruling Dac, 2026-11-09.
This page is a reference for the 2-layer trial on trusty-tools.
`tm pr` does not implement it yet.

Sources: [GitHub stacked PR reference](https://docs.github.com/en/pull-requests/reference/stacked-pull-requests),
[REST stacks API](https://docs.github.com/en/rest/pulls/stacks).

## What a GitHub PR stack is

- **Chaining.** Each PR's base is the branch of the PR below it.
  The bottom PR targets the trunk.
  All branches live in one repository with linear history.
- **Merge.** Squash, merge and rebase all work.
  A stack merges bottom-up, one PR at a time or in contiguous groups.
  After the bottom PR merges, GitHub rebases the rest and retargets the lowest PR still open.
- **Rebase.** The "Rebase stack" button runs a cascading rebase on the server.
  A rebase that leaves the diff unchanged does not dismiss approvals.
- **Per-layer rules.** Every layer is held to the stack base's rules: reviews, required checks and CODEOWNERS.
  A layer merges only if every layer below it qualifies.
  Bypass is allowed on the bottom layer only.
- **Merge queue.** Layers enter in order.
  Ejecting one layer ejects every layer above it.
  A merge group may grow up to 50% to keep a stack whole.
- **Actions.** Workflows on `pull_request` that target `main` run for every layer.
  `github.event.pull_request.stack` carries the stack metadata.
- **Surface.** `gh stack` is an official extension and is not installed on the dev host.
  The REST API uses `X-GitHub-Api-Version: 2026-03-10`.
  Routes: `GET/POST /repos/{o}/{r}/stacks`, `GET /stacks/{n}`, `POST /stacks/{n}/add`, `POST /stacks/{n}/unstack`.

## trusty-mpm policy (Dac ruling)

Owner ruling, 2026-11-09: "Opt-in for multi-PR epics; critic per layer; one clearance per stack (all layer SHAs, re-pinned after rebases)".

- **Scope.** Stacking is opt-in.
  The default stays main-based PRs.
  A multi-PR epic may request a stack.
- **Review.** The critic reviews each layer.
  This fits GitHub's per-PR rules and keeps the SHA pin.
- **Clearance.** One clearance covers the whole stack.
  It names every layer's head SHA.
  The Architect re-pins it after each rebase.

## Integration points to change

- **Head re-pin, `merge.rs`.** `merge.rs` pins the head SHA with `--match-head-commit`.
  After each bottom merge GitHub rebases the layers above and changes their head SHAs.
  Re-read the head and re-pin per layer.
  The critic verdict is tied to a SHA (`critic_verdict.rs`).
  Decide whether an unchanged-diff rebase keeps the verdict.
- **Required-checks gate, `check_gate.rs`.** The gate reads branch protection for `baseRefName`.
  For an upper layer that is an unprotected feature branch, so it finds no required checks.
  The gate must read the stack base instead.
- **Branch deletion.** `tm pr merge` passes `--delete-branch` by default.
  A lower layer's branch is the upper layer's base.
  Pass `--no-delete-branch` for stack layers.
- **CI trigger.** `ci.yml` filters on `branches: [main]`.
  PR #3838 merged with zero checks after it was stacked on a feature branch.
  The trial verifies that upper layers get checks.
  If they do not, change the filter.
- **Queue ownership.** The merge-queue owner's authorization should name the whole stack.
  `queue-check` should read every layer.
