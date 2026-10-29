---
name: neon-postgres
description: "Neon Postgres on Vercel. Use for any Neon database, any Vercel Postgres or database env var (DATABASE_URL, DATABASE_URL_UNPOOLED, PG*, POSTGRES_*), any Neon preview-branch database, and any drizzle migration, push or baseline run against Neon."
user-invocable: false
metadata:
  version: "1.0.0"
category: agent-reference
tags: [neon, postgres, vercel, drizzle, database, preview-branches]
---

# Neon Postgres, the Vercel integration and drizzle

Research date 2026-10-29. Sources are listed at the end. Where the docs and observed behaviour differ, the text says so.

## Rules

Bob's rulings (2026-10-29):

> "DATABASE_URL and DATABASE_URL_UNPOOLED ... should be set automatically by NEON"
> "you shouldn't need to set env variables for preview branches, that is handled by Neon."

Never:
1. Never set `DATABASE_URL`, `DATABASE_URL_UNPOOLED`, `PG*` or `POSTGRES_*` by hand in any Vercel scope of a project that has the Neon integration. This covers Production, Preview, Development, custom environments and git-branch-scoped Preview entries.
2. Never delete or recreate an env entry that carries the integration's `configurationId`. It is expected output. Deleting it is undone by the next deploy, and a hand-made replacement collides with it.
3. Never print, paste, log or commit a connection string, password or API key. Not in chat, issues, PRs, briefs, commits or action logs.
4. Never write to a production branch (DDL, `push`, `migrate`, `psql`, restore, reset) without the gated steps in "Prod changes".
5. Never run a migration over a pooled connection string.
6. Never run `drizzle-kit push` or `--force` against production or a branch you cannot discard.
7. Never switch the integration's production branch, disconnect the integration, or delete a Neon project or branch you did not create.
8. Never treat a `.vercel.app` preview as safe because it is "only preview". A Preview entry that holds the prod URL writes to prod.

Always:
1. Compare databases by endpoint id only (the `ep-xxxx-xxxx` part of the host). Never compare or quote the full host, user or password.
2. Take a branch (or confirm the restore window) before any change to data you care about.
3. Start with read-only commands. Say what you ran and what it returned, as counts and names, never values.
4. Hold the API key in a secret store (`tm secrets`, or the env of the process). Pass it as `NEON_API_KEY`, never as a flag in shell history that a log captures.
5. Ask the owner when the Neon console or a Vercel setting must change. Agents do not have the owner's integration login.

## How the Vercel integration works

There are two integration kinds. Check which one the project uses before you act.

| | Neon-managed (connect) | Vercel-managed (Marketplace native) |
|---|---|---|
| Account and billing | Own Neon account, Neon bills | Vercel creates the Neon org `Vercel: <team>`, Vercel bills |
| Plan and DB changes | Neon console | Vercel dashboard (Storage) |
| CLI login | `neon login` works | Needs an API key; `neon login` does not work |
| Both in one project | Not allowed | Not allowed |

Variables the integration creates:
- `DATABASE_URL`: pooled string (PgBouncer, `-pooler` host).
- `DATABASE_URL_UNPOOLED`: direct string.
- `PGHOST`, `PGHOST_UNPOOLED`, `PGUSER`, `PGDATABASE`, `PGPASSWORD`: optional parts. In the Neon-managed kind you enable these in Settings.
- `POSTGRES_*`: legacy names for Vercel Postgres templates (Vercel-managed kind).
- `NEON_AUTH_BASE_URL`, `VITE_NEON_AUTH_URL`: only with Managed Better Auth.
- `NEON_REALTIME_URL`, `NEON_REALTIME_SECRET`: only with Realtime.
- An optional Custom Prefix renames them. Needed only if unprefixed names are already in use.

Scopes, Neon-managed kind:
- Production: credentials come from the production branch chosen at connect time (default: the project's default branch).
- Preview: one new Neon branch per deployed git branch, parented on the default branch.
- Development: an optional `vercel-dev` branch cloned from the default branch.
- You choose which Vercel environments receive the variables when you connect the project.

Preview branching:
1. A push to a non-production git branch starts a Vercel preview deployment.
2. Vercel sends a webhook. Neon creates the branch `preview/<git-branch>` through its API. It is copy-on-write from the default branch, with data.
3. The deployment gets its own `DATABASE_URL` and `DATABASE_URL_UNPOOLED` for that branch.
4. Leave "Production" unchecked under "Create database branch for deployment". Production deployments use the production branch.

Docs vs observation. The docs say the preview variables are "injected via webhook at deployment time" and are not shown in Vercel's env settings. An audit of one project (2026-10-29) saw something different: about 3 minutes after Neon creates `preview/<branch>`, branch-scoped Preview `DATABASE_URL` and `DATABASE_URL_UNPOOLED` entries appear in the Vercel project. Both are removed when the git branch is deleted. Treat those entries as integration output. Either way the rule is the same: hands off.

Cleanup:
- Neon-managed kind: the setting "Automatically delete obsolete Neon branches" removes preview branches when the git branch is merged or deleted. It runs the next time a preview deployment is created. Do not rename preview branches or give them child branches; either can break the cleanup.
- Vercel-managed kind: preview branches are deleted when their Vercel deployments are removed. Vercel keeps preview deployments 6 months by default, so branches can outlive the PR.
- Branches left alone are eventually archived.
- Disconnecting stops new variables and new branches. It leaves existing branches and the database.

### How to tell an integration entry from a hand-set one

Integration entries carry the integration's `configurationId` (`icfg_...`), the same id as the entries the integration created for Production and Development when it was first connected. The id is per project and integration install.

Example only (a project called joanies; do not reuse): `configurationId: icfg_GJriBcupGA7gwSCaK9bmQ2ME`. Find your own project's id from its original Production entry.

Method:
1. `vercel env ls` (add `preview <git-branch>` to narrow) lists names and environments. It shows no ids and no values.
2. For the metadata, call the REST API `GET /v10/projects/{idOrName}/env` (add `gitBranch=<branch>` to filter Preview entries; leave `decrypt` off). Read only these fields per entry: `key`, `target`, `gitBranch`, `type`, `configurationId`, `createdBy`, `updatedBy`, `createdAt`, `system`. Never read or print `value`.
3. Classify:
   - `configurationId` set and equal to the integration's id: integration entry. Leave it.
   - `configurationId` null and `createdBy` is a person or token: hand-set. If it is a DB var in Preview, report it to the owner. Do not delete it on your own. Removal needs the owner's word.
4. Pagination: the API returns `pagination.next`. Page until it is null before you say an entry does not exist.

Record the integration's `configurationId` for each project in that project's notes the first time you read it.

## Branching

Neon organises data as project -> branches. Each branch has its own compute endpoint, with its own `ep-` id and host. Connection details of one branch never work for another.

Create a branch:
- With data (default): copy of the parent now, or at a past point inside the history window.
- Schema only (beta): structure without rows. It is a root branch with no parent. Use it when the parent holds real user data. Limits: counts toward the root-branch allowance (Free 3, Launch 5, Scale 25); cannot be reset from a parent; restoring it copies schema and data from the source. API: `init_source: "schema-only"` plus `parent_id`. CLI: `neon branches create --schema-only`.
- With expiry: set an expiration (TTL) for CI and temporary branches. The integration's production branch must not have one.

Reset from parent:
- `neon branches reset <id|name> --parent`.
- Replaces all databases on the branch with the parent's latest state. Local changes on the branch are discarded.
- Leaves no backup branch. Connection details stay the same; live connections drop briefly.
- Blocked for root branches (production, schema-only) and for branches that have children.

Restore (point in time):
- `neon branches restore <target> <source>@<timestamp|lsn>`; self-restore needs `--preserve-under-name <name>`.
- Full overwrite of all databases on the branch. Neon keeps the pre-restore state in a backup branch named `{branch}_old_{timestamp}`.
- Only root branches support it (docs). Other branches use reset or recreate.
- Window: default 6 hours on Free, 1 day on paid plans; up to 7 days (Launch) or 30 days (Scale).
- Time Travel Assist runs read-only queries at a past time to confirm a restore target first.

Protected branches (paid plans: Launch 2, Scale 5):
- Cannot be deleted or reset; the project cannot be deleted while one exists.
- Child branches get fresh role passwords, so prod passwords do not spread to dev branches.
- Not archived for inactivity. IP Allow (Scale) can apply to protected branches only.
- Set in the console: Branches -> branch -> Protect. Recommend it for the production branch; ask the owner to do it.

Branch hygiene for agents:
- Name branches for the purpose (`staging`, `pr-123-check`). Never reuse the integration's `preview/` prefix for a hand-made branch.
- Delete only branches you created, after the check is done.
- Do not create child branches under a `preview/` branch.

## Connection strings

- Pooled: host contains `-pooler` after the endpoint id. PgBouncer, transaction mode, up to 10,000 client connections. Use for app runtime, serverless functions, Next.js route handlers.
- Direct (unpooled): same host without `-pooler`. Use for migrations, `pg_dump`/`pg_restore`, logical replication, long analytics, admin work.
- Pooled mode does not support: session `SET`/`RESET` (including `search_path` set outside a transaction), `LISTEN`/`NOTIFY`, `WITH HOLD` cursors, SQL-level `PREPARE`, temp tables that persist, session advisory locks. Protocol-level prepared statements work.
- Migration tools often fail on pooled strings. Docs: "using a pooled connection string for migrations can lead to errors".
- Pool wait time-out is 2 minutes (`query_wait_timeout`). Direct connection limit depends on compute size.
- Mapping: `DATABASE_URL` is the app's pooled string; `DATABASE_URL_UNPOOLED` is the one for drizzle-kit, `psql` and dumps.
- Endpoint id check, safe form: take the host, strip the user and password, keep the `ep-...` token, and compare only that. Example: confirm that staging is `ep-A` and prod is `ep-B`, `A != B`.

Getting a string without exposing it:
- Local dev: `vercel env pull` writes the Development scope to a file. Keep that file out of git. Run tools with `vercel env run -- <cmd>` to avoid a file; add `-e preview --git-branch <b>` for a branch.
- Never `cat` the file or echo the variable. To check a host, extract the endpoint id in the same command and print only that.
- A Secret-type Vercel variable is write-only after saving; `vercel env pull` writes a placeholder for it. Production values may not be readable at all. If you cannot read it, ask the owner; do not set a new value.

## Migrations (drizzle)

Tools: `drizzle-kit generate` (writes SQL files; no DB connection), `drizzle-kit migrate` (applies unapplied files), `drizzle-kit push` (diffs the schema against the live DB and applies directly, no SQL files), `drizzle-kit pull` (DB -> TypeScript schema).

Rules:
- Use `DATABASE_URL_UNPOOLED` for `migrate`, `push` and `pull`.
- Dry run: `drizzle-kit push --explain` prints the planned SQL without applying it. `--verbose` prints SQL before running it. `--force` auto-accepts data-loss statements; never use it on prod.
- `push` writes no migration history (the docs do not say it does). A database built by `push` has no `drizzle.__drizzle_migrations` rows, so `migrate` will try to run migration 0000 against tables that already exist and fail.
- `migrate` keeps its log in the table `__drizzle_migrations` in schema `drizzle` (both configurable under `migrations: { table, schema }` in `drizzle.config.ts`). It reads the SQL files, reads the log, and applies only the unapplied files.
- Prefer `generate` + `migrate` for prod. The docs say teams use `push` as a production flow, but it leaves no review trail and no history.

Baselining a prod built with `push`:
1. Not covered by the fetched drizzle docs. From drizzle's source (verify against the installed `drizzle-orm` version before use): the log row holds `hash` (sha256 of the SQL file) and `created_at` (the journal entry's `when`, in ms). The migrator runs only files whose `when` is greater than the latest `created_at`.
2. Baseline by running `generate` once from the current schema, then inserting one log row for that migration into `drizzle.__drizzle_migrations` on prod. Later migrations then apply cleanly.
3. A project may already hold a script for this (for example a `baseline-mark-applied.sql`). Read it; do not run it on prod without the "Prod changes" gate.
4. Prove the baseline on a Neon branch of prod first (next section).

Safe rehearsal on a branch of prod:
1. Create a branch `migrate-check-<date>` from prod with data (or schema-only if the data is sensitive and the migration is structural). Set a 1-day expiry.
2. Take its unpooled string from `neon connection-string` or the console. Do not print it.
3. Run the baseline and `migrate` against the branch only. Check the target branch's endpoint id differs from prod's.
4. Inspect the result (read-only commands below), run the app's test suite against the branch string if the owner approves.
5. Delete the branch when done, or let the expiry do it.

Migrations in Vercel builds:
- The Neon docs suggest adding a migration command to the build override (for example `npx prisma migrate deploy && npm run build`). For drizzle that runs `migrate` on every deploy, including each preview branch. That is acceptable only after the production database is baselined (a project-level gate). Until then, run no migrations in staging or preview builds.
- A preview build needs a `DATABASE_URL` at build time if any route reads the DB during `next build`. When the integration has not yet created the preview branch (about 3 minutes lag), the first build can fail. Fix: redeploy once the branch exists. Do not set the variable by hand.

## Inspect safely (read-only first)

Auth (CLI): `export NEON_API_KEY=...` from the secret store, or `--api-key`. The CLI checks `--api-key`, then `NEON_API_KEY`, then the `neon login` credentials, then interactive login. Binary names: `neon` or `neonctl`. Use `-o json` for full data; the table view may omit fields.

Key types: personal (all orgs you belong to), organization (admin only; whole org), project-scoped (admin only; one project, Editor rights). Ask for a project-scoped key for agent work. The secret shows once at creation. Rotation: create the new key, update callers, revoke the old. Revocation is immediate and permanent.

Read-only commands:
```
neon me
neon projects list -o json
neon branches list --project-id <id> -o json
neon branches get <branch> --project-id <id> -o json
neon operations list --project-id <id>
neon branches schema-diff ...        # compare two branches' schema
neon inspect db table-sizes          # also: index-sizes, locks, long-running-queries
neon snapshots list --project-id <id>
```
Branch listing shows parent, state, expiry and protected status. It is the fast check that `preview/<branch>` exists.

REST API: base `https://console.neon.tech/api/v2`, header `Authorization: Bearer $NEON_API_KEY`. Start with `GET /projects`, `GET /projects/{id}/branches`, `GET /projects/{id}/endpoints`. Read names, ids, states, parent ids, `protected` and `expires_at`; not connection URIs.

Database checks without exposing data (run through a string held in an env var, not typed):
- Which database am I on: compare the endpoint id from the host, as described above. Do not query the server address.
- Is the table set right: `select count(*) from information_schema.tables where table_schema='public'`.
- Migration log: `select count(*), max(created_at) from drizzle.__drizzle_migrations`.
- Row counts per table: counts only, never rows from user tables (names, emails, tokens).
- Always `set default_transaction_read_only = on` on a direct connection (session-level, so use the unpooled string) before ad-hoc SQL against prod.

Vercel side, read only:
```
vercel env ls                          # names and environments, no ids, no values
vercel env ls preview <git-branch>
```
Plus the REST call in "How to tell an integration entry..." for `configurationId`.

## Prod changes (gated steps)

A change to a production Neon branch has these gates, in order. Skip none.
1. Owner approval for this exact change, in this session, naming the project and the action. A general ruling is not enough.
2. Safety copy: create a Neon branch of prod (`pre-<change>-<date>`, with data, expiry 7 days or none) and record its name. Confirm the restore window covers the change time.
3. Rehearsal on a branch of prod passed (see Migrations). Record the endpoint ids you used: rehearsal `ep-A`, prod `ep-B`.
4. Run the change with the unpooled string, in a transaction where possible, from a place that logs actions without logging values.
5. Verify with the read-only checks. Compare counts before and after.
6. Rollback plan stated before step 4: restore from the safety branch, or instant restore if prod is a root branch (it creates a `{branch}_old_{timestamp}` backup).
7. Log it in the project's action log with the owner's words and names only.

Things that need the owner's hands: changing the integration's production branch, enabling branch protection, connecting or disconnecting a project, creating or revoking API keys, switching the integration kind.

## Troubleshooting

- Build fails with no `DATABASE_URL` in Preview: the preview Neon branch does not exist yet. Check `neon branches list` for `preview/<branch>`. If missing, check the integration's Preview toggle and the webhook, and ask the owner. If present, redeploy. Never set the variable.
- Preview deploy talks to prod: a Preview entry (generic, not branch-scoped) holds the prod URL. Find it with the API, check `configurationId` and `createdBy`, and report it to the owner. Example case (2026-10-29, one project): generic Preview `DATABASE_URL` and `DATABASE_URL_UNPOOLED` pointed at the prod endpoint `ep-<prod>`; the owner had them removed. Branch-scoped entries then returned as integration output.
- "Failed to set environment variables" during integration setup: existing `DATABASE_URL`, `PGHOST`, `PGUSER`, `PGDATABASE` or `PGPASSWORD` entries in Vercel conflict. Remove or rename them (owner's decision), then retry setup. The docs do not list the `_UNPOOLED` names as conflict sources.
- Entries you deleted come back: the integration re-creates them on the next deploy or branch event. This is expected.
- Entries remain after the PR closed: cleanup runs at the next preview deployment (Neon-managed) or when deployments expire (Vercel-managed). Ask for a cleanup run through the owner.
- `drizzle-kit migrate` errors "relation already exists": the DB came from `push` and the log is empty. Baseline first.
- Migration hangs or "prepared statement" / `search_path` errors: you used the pooled string. Switch to unpooled.
- "too many connections": direct limit depends on compute size. Use the pooled string for the app.
- `query_wait_timeout` (2 min): pool slot starvation on the pooler. Reduce parallelism or long transactions.
- Branch will not reset: it is a root branch or has children. Delete the children (yours) or use restore.
- Restore refused on a child branch: only root branches support instant restore. Use reset-from-parent or re-branch.
- First connection after idle is slow: compute scales to zero and restarts. Retry once; do not diagnose it as an outage.

## Unclear in the docs

- Whether the Vercel-managed integration also writes branch-scoped Preview entries visible in project env settings. The docs say no; an audited project showed entries with the integration's `configurationId`, and its integration kind was not recorded.
- The columns of `__drizzle_migrations` and the baseline procedure are not in the fetched drizzle docs. Verify against the installed version.
- `neon connection-string` options (`--pooled`, `--role-name`, `--database-name`) are not on the CLI reference page. Run `neon connection-string --help`.
- The CLI docs list `neon set-context` as deprecated in favour of `neon link`.
- The restore page states root-branch-only support; the introduction page mentions restoring "any branch". Treat root-only as the safe reading.
- Protected-branch limits and the history-window figures are plan-dependent and change; re-read the pages before quoting them to the owner.

## Sources (accessed 2026-10-29)

- Neon, Vercel-managed integration: https://neon.com/docs/guides/vercel-managed-integration
- Neon, Vercel native integration: https://neon.com/docs/guides/vercel-native-integration
- Neon, Neon-managed Vercel integration: https://neon.com/docs/guides/neon-managed-vercel-integration
- Neon, branching: https://neon.com/docs/introduction/branching
- Neon, schema-only branches: https://neon.com/docs/guides/branching-schema-only
- Neon, reset from parent: https://neon.com/docs/guides/reset-from-parent
- Neon, instant restore: https://neon.com/docs/guides/branch-restore
- Neon, protected branches: https://neon.com/docs/guides/protected-branches
- Neon, connection pooling: https://neon.com/docs/connect/connection-pooling
- Neon, CLI reference: https://neon.com/docs/reference/neon-cli
- Neon, API keys: https://neon.com/docs/manage/api-keys
- Neon, drizzle migrations guide: https://neon.com/docs/guides/drizzle-migrations
- Drizzle, push: https://orm.drizzle.team/docs/drizzle-kit-push
- Drizzle, migrate: https://orm.drizzle.team/docs/drizzle-kit-migrate
- Drizzle, migrations overview: https://orm.drizzle.team/docs/migrations
- Vercel, environment variables: https://vercel.com/docs/environment-variables
- Vercel, `vercel env`: https://vercel.com/docs/cli/env
- Vercel, REST API, list project env vars: https://vercel.com/docs/rest-api/reference/endpoints/projects/retrieve-the-environment-variables-of-a-project-by-id-or-name
- Observed behaviour: a 2026-10-29 audit of one Vercel project using the integration.
