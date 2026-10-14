Fixed
- `tm pr open` no longer exits 3 when the linked issue names a closed GitHub Project (such as #22 or #25, superseded by #45). A project step that fails is checked against the owner's closed projects; a closed one is skipped and named in the output, and the PR joins `agents.ticketing.default_project` in its place when that key is set (#9749).
