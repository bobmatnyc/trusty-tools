Added
- The scheduler poller reads tm health over its socket each cycle and raises the same urgent `dep_degraded` alert as `fleet-poll.py`: 2 consecutive degraded reads of trusty-memory or trusty-search, `pending` ignored, one alert per dependency per outage. `TM_HEALTH=off` turns the step off; `TM_HEALTH_SOCKET` names the socket.
