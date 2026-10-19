# tm-delegation-patterns — agent chains by work shape

## Full-Stack Feature

**Chain**: Research → Code Analysis → (`react-engineer` / `web-ui-engineer`) +
language Engineer → Ops (deploy) → Ops (verify) → `web-qa` + `api-qa` →
Documentation.

**Example**: "Add a user dashboard with analytics" → Research investigates
frameworks → Code Analysis reviews the approach → `react-engineer` builds
the UI, `rust-engineer`/`python-engineer` (per stack) builds the analytics
endpoint → `local-ops` deploys to staging and verifies (health check, logs)
→ `api-qa` tests the endpoints, `web-qa` tests the dashboard → Documentation
updates the API docs and user guide.

## API-Only Development

**Chain**: Research → Code Analysis → language Engineer → Ops (deploy, if
needed) → Ops (verify) → `api-qa` → Documentation.

## Web UI Only

**Chain**: Research → Code Analysis → `react-engineer` / `svelte-engineer` /
`web-ui-engineer` (per stack) → Ops (deploy) → Ops (verify) → `web-qa` →
Documentation.

## Local Development / Infra

**Chain**: Research → Code Analysis → Engineer → `local-ops` (start/PM2/Docker)
→ `local-ops` (verify: logs + health check) → `qa` → Documentation.

**A brief that changes local Terraform names who applies it (#8660).** The
state and `terraform.tfvars` of a local root live only in the main checkout,
and pm-guard refuses any command that names them. The engineer writes the
config in its worktree; the apply runs from the main checkout, by the operator
or by `local-ops` dispatched there, where `terraform apply` reads
`./terraform.tfvars` itself. A worktree agent may run
`terraform plan|apply -state=<main checkout state>` and names no vars file.

## Bug Fix

**Chain**: Research (reproduce/investigate) → Code Analysis → Engineer (fix)
→ Ops (deploy, if applicable) → Ops (verify) → `web-qa`/`api-qa` (regression)
→ **Version Control** (PR).

## Platform-Specific Deploy

| Platform | Ops Agent |
|---|---|
| Vercel | `vercel-ops` |
| Google Cloud | `gcp-ops` |
| Local (PM2/Docker/localhost) | `local-ops` |

`vercel-ops` and `gcp-ops` are platform-gated, so a project without the
matching marker never receives them. Which agents exist at all is declared in
`framework-manifest.toml` (rendered in `tm-capabilities`'s
`references/agents.md`) — check there, or the project's generated roster,
before routing. When no dedicated ops agent exists for a platform, use
`local-ops` or tell the user.
