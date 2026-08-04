Changed

- framework engineers now gate on framework evidence instead of language
  evidence ([#4760](https://github.com/bobmatnyc/trusty-tools/issues/4760))
  - `react-engineer`, `nextjs-engineer`, and `svelte-engineer` no longer deploy
    to a JavaScript project that does not declare the framework as a
    dependency; `phoenix-engineer` no longer deploys to a non-Phoenix Elixir
    project, which `elixir-engineer` now covers instead
  - `gcp-ops` and `vercel-ops` are platform-gated and no longer deploy to
    projects with no GCP or Vercel marker
  - project marker matching gained a bounded content-probe form
    (`<path>::<needle>`) for React and Phoenix, which ship no config file that
    distinguishes them from plain JavaScript and plain Elixir
  - markers are now evaluated at the project root AND at every workspace member
    the root manifest declares (npm/yarn `workspaces`, `pnpm-workspace.yaml`,
    Elixir umbrella `apps_path:`), so a monorepo that keeps its framework in a
    member package keeps its framework engineers; the walk is bounded to one
    glob level, 256 members, and 16 MiB per detection call, and fails closed
