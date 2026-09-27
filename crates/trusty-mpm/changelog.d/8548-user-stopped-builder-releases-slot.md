Fixed
- A builder subagent the user stops from the harness now gives its builder slot back at the next builder claim, or within the daemon's 60 s sweep. Before, the slot stayed held for the 45-minute lease TTL, and `tm doctor` kept listing the stopped agent as a holder (#8548).
- A stopped builder that is later resumed no longer shares its build directory with a new builder. A user stop or a `TaskStop` frees the builder's capacity at once, but its slot index stays reserved until the 45-minute lease TTL, and the lease is held again when the agent resumes: its stop marker reads `stoppedByUser: false`, or it makes a tool call (#8548).
- Reading a builder's stop marker no longer blocks on a FIFO or other non-regular file at the sidecar path (#8548).
