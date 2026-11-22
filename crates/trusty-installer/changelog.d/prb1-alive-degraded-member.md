Fixed
- The member health probe reads a body with `alive: true` and `status: "degraded"` (trusty-mpm while trusty-memory or trusty-search is down) as serving, so `tctl up` does not restart a live trusty-mpm for another daemon's outage.
