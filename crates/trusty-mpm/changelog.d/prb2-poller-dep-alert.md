Added
- The Architect fleet poller (`fleet-poll.py`, new `fleet_poll_deps.py`) raises one urgent `dep_degraded` alert per outage when tm health reports trusty-memory or trusty-search degraded on 2 consecutive reads; `pending` reads never count, and recovery re-arms the alert. An unparseable tm health answer raises `dep_health_error`. `tm fleet init` ships the new script.
