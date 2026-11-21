Fixed

- A broker restart no longer restores a grant that has a `grant_end` row in the audit log, so a stale grant store left by a failed rewrite and delete cannot bring back a revoked grant. An audit log that cannot be read restores no grants.
