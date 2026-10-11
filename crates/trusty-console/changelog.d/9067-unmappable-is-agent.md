Changed
- A secrets client whose pid the console cannot find, which includes every client on platforms other than macOS and Linux, is now asserted an agent (`agent_parent: true`), so turning a key's "agents may use" flag on is refused for it; turning it off is still allowed (#9067).
