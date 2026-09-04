Changed

- BASE-AGENT and the `ticketing` agent no longer restate the commit and PR
  attribution footer. It comes from the `attribution` key tm writes into the
  provisioned Claude Code settings (#6807). The issue-body footer, which that
  setting does not cover, is unchanged.
