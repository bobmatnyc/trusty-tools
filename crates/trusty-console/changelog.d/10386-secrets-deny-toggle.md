Changed
- The Secrets page's per-key toggle is now "Denied to agents", off by default: agents may use every in-scope key the operator has not denied (#10386). Checking it writes the key's deny; only a person can clear it, and a refused change puts the toggle back.
