Fixed
- A palace alias and the palace it points at now share one write lock and one chat-session store, so concurrent identical writes through both no longer slip past the duplicate gate, and a chat call through an alias no longer creates a second session database under the alias name (#9544).
- Creating a palace whose name is a live alias now answers JSON-RPC code `-32006` (refused) instead of `-32603` (internal error); the service layer reports it as a conflict (#9544).
- Dream status for a palace alias reads the target palace's dream stats instead of answering not-found (#9544).
