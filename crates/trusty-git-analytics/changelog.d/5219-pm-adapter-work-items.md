Added

- JIRA and GitHub Issues now write rows into `work_items`, and Azure DevOps writes them through the same code path (#5219). `tga collect` gains one provider-agnostic pass, `collect::work_item_pipeline`, which asks each configured `PmAdapter` what references its own syntax finds in the commit corpus, batch-fetches them, and writes one `work_items` row plus a `commit_work_items` link per resolved reference. This is the first production caller of `build_adapters` — until now the trait, the factory and all four adapters were built, configured, validated at `Config::load`, and reachable only from their own tests.

  `github.ticket_regex` and `jira.ticket_regex` are live as a result. Both were validated at config load and then read by nothing, so setting either had no effect on any `tga collect` output. `pm.azure_devops.ticket_regex` stays live: `build_adapters` now forwards it to the ADO adapter, which previously ignored it in favour of a hardcoded `AB#N` pattern.

  Fetching is opt-in per provider. `github.fetch_on_reference` and `jira.fetch_on_reference` are new and default to `false`, matching `linear.fetch_on_reference` and `pm.azure_devops.fetch_on_reference`. GitHub is configured in nearly every `tga` config for pull requests alone, and `#N` is the commonest reference shape there is, so enabling the issue pull by default would spend a token's hourly budget on lookups nobody asked for.

  Every failure is recorded at the severity its blast radius earns. A provider that fetched nothing while reporting at least one error has lost its whole corpus and fails the stage, which makes `tga collect` exit non-zero; a provider that resolved some references and errored on others records an item skip and keeps the rows it did get. A reference the provider authoritatively does not have returns not-found and is not a fault.

  Linear keeps `collect::linear_pipeline`. It writes `linear_issues` in the same pass and filters candidate identifiers through `linear.team_keys` before fetching; neither is expressible through `PmAdapter`, and routing only its `work_items` half through the shared pass would fetch every Linear issue twice per run.
