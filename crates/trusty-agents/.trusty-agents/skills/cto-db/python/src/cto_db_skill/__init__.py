"""cto-db-skill: read-only query tools over the CTO operations database.

Why: cto-assistant's agent.toml declares four tools (`query_headcount`,
`query_budget`, `query_risks`, `query_work_classification`) but nothing in
trusty-agents' live dispatch path implemented them (#3700) after the
Rust-native `crates/cto-assistant` plugin's install call site was removed
(PR #3310, #3656, #3732). Per the owner's directive, the fix bundles the
query code AND the data source together as a *skill* (Python, not a coded
Rust agent) so cto-assistant stays declarative-only per DOC-41 while the
tools become invokable again through the generic
`crate::tools::python_skill` bridge in trusty-agents.

What: `db` holds the four query functions plus DB-path resolution; `cli`
is the subprocess entrypoint the Rust bridge invokes (tool name as argv,
JSON args on stdin, JSON result on stdout).

IMPORTANT — fixture vs. real data (read this before trusting any numbers):
This skill ships a *fixture* SQLite database (`fixtures/cto_fixture.db`)
with invented sample rows. The real CTO ops database
(`~/Duetto/cto/data/cto.db`) is a Duetto-internal file on Bob's machine that
this sandbox cannot reach or verify. Point `CTO_DB_PATH` at the real file to
use it; see `README.md` in this directory for the full disclosure.
"""

from cto_db_skill.version import __version__

__all__ = ["__version__"]
