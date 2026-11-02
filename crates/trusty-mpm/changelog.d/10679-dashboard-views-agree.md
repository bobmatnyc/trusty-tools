Fixed
- The Architect dashboard store reads under a shared lock, so a read never lands between the files of one batch: after one write the task, session and header views agree, and `/api/meta` straight after a write serves the new values. Reading several collections together goes through one lock (#10679).
