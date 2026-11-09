Changed

- The `tm-ticketing` milestones-and-projects reference says never to page `gh project item-list` to find one issue's project item, which spends the primary GraphQL rate limit; ask the issue's `projectItems` through `gh api graphql` instead (#10550).
