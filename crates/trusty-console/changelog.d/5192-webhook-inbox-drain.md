Fixed
Webhook health now meters each target's quarantined deliveries alongside its held ones and reports `Error` when any exist. Quarantining removes a delivery from the held count, so without this the signal turned green at the moment a delivery was confirmed never to be processed. `METRICS_SCHEMA_VERSION` is 3 (#5192).
