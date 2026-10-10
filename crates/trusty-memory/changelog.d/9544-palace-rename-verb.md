Added
- `palace_rename` (MCP tool and raw RPC method): move a palace to a new id. Drawers, vectors, triples, rooms, wings and chat sessions move with it and their counts are verified unchanged; the old id becomes an alias of the new one. Refuses an alias source, a target that is an alias of another palace, and an existing target unless it is empty and `replace_empty` is set (the empty target moves to `<data root>/.trash`). Refusals answer `-32006`, a missing source `-32004`, a path-shaped id `-32602` (#9544).
- A `palace_renamed` daemon event announces each rename (#9544).
