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
