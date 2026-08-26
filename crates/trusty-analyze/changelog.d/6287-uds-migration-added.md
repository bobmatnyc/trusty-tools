Added

- `service::events::CODE_DEADLINE_EXCEEDED` (`-32005`) so a handler that
  exhausted its own deadline stays distinguishable from one that broke —
  trusty-review reads the code to print "ran out of time" rather than "could not
  be reached". `CODE_NOT_FOUND` (`-32004`) preserves #5049's
  ingested-but-empty distinction across the transport change.
- `service::rpc::METHODS`, the list the four crates that dial these names by
  literal are checked against, and `tests/uds_consumer_contract.rs`, which
  stands the daemon up on a temp socket and asks each of them what it sees.
