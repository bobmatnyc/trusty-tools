Fixed
- `Bundle::open` reports a verification record it cannot read as an error `UnreadableFile` finding on that concept instead of failing the whole open (#10879). The concept's verification state and writes still fail on it.
- A content change through `put` moves to history every live verification-record event bound to an older hash, not only the events with a frontmatter copy, so a record event whose frontmatter copy a hand edit removed no longer warns of an out-of-band edit after every later write (#10879).
- `import run --from adapter:<kind> --store <id>` refused for another selection now asks for a `--store` that names no other selection, instead of telling the caller to pass `--store` (#10879).
