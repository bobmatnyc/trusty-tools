Changed

- The ticketing agent now runs `tm issue standard` to read the ticketing standard in effect rather than assuming what its prompt says. The standard comes from the `agents.ticketing` block in `~/.trusty-tools/trusty-mpm/config.yaml`, so a project can add or restyle a component label, name a different assignee, and point at its own `issue-state.yaml`. The agent asset also states the two rules that block cannot relax: `Refs #N` stays the PR issue-link keyword, and `trusty-mpm` stays a component label (#6918).
