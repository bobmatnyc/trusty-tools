Changed
- `tm pr open` adds the pull request to no project (#10276, epic #10274, D7: pull requests are not board items). It no longer copies the linked issue's projects onto the PR, so its `gh pr edit` never carries `--add-project`, and it no longer reads the owner's closed projects to skip one.
