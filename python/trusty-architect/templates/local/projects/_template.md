# <Project name>

Copy this file to `<project>.md` in this directory for each watched project.

- Repo: <owner/repo>
- Local path: <path>
- tmux session: <tm-name> (pane ids change on restart; discover them again)
- Project instructions: <the instruction files worth reading first>
- trusty-memory palace: <palace name>

## Standing decisions and authorizations

- <ruling, date, scope; labelled reported or verified>

## Gates

- <review, release and deploy gates the Architect must keep>

## Architecture baseline

- Baseline: <sha> (<date>, <ref>); set by <review, ruling or provisioning>
- Modules: <top-level directories, crates or packages and their roles>
- Dependencies: <manifests; key direct dependencies with major versions>
- ADRs: <folder; ids and titles at the baseline>
- Boundaries: <rules such as "api/ must not import db/">
- Mandates in scope: <ids from `local/mandates/`>

## Architecture

- <date> <drift check, review offer, review verdict, the user's answer>

## Snapshots

- <date> <session id> <snapshot path> (verified)

## Blockers and pending decisions

None.

## Log

- <date> <fact> (reported|verified)
