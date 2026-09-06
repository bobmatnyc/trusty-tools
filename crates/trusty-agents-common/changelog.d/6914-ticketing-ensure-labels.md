Changed

- The ticketing agent now runs `tm issue seed-labels` on first use in a repository, and re-runs it plus one retry when `gh issue edit --add-label` or `gh issue create --label` fails on an unknown label. Both commands fail outright on a label the repo has never seen, and the agent previously had no instruction that would create one (#6914).
