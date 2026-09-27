Fixed
- A builder subagent the user stops from the harness now gives its builder slot back at the next builder claim, or within the daemon's 60 s sweep. Before, the slot stayed held for the 45-minute lease TTL, and `tm doctor` kept listing the stopped agent as a holder (#8548).
