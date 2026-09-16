Fixed

- `tm issue audit`, the `issue_audit_recent` doctor check and the `tm-ticketing` skill now name `gh project item-add <number> --owner <owner> --url <issue-url>` as the project attach; `gh issue edit --add-project "<title>"` exits 0 and attaches nothing when the title does not resolve in the scope gh derives from the repository (refs [#7952](https://github.com/bobmatnyc/trusty-tools/issues/7952))
