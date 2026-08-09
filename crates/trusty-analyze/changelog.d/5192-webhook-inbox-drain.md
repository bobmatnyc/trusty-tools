Fixed
- `trusty-analyze webhook-listen` now drains its webhook inbox into the analysis pipeline instead of holding acknowledged deliveries forever. The PR-event filter and the fetch/analyse/comment pipeline moved to `webhook_drain`, so the legacy `POST /webhooks/github` route and the UDS drain run one implementation.
- A delivery is never analysed twice. The shared drain's processed-delivery ledger closes the crash window that would otherwise post a duplicate PR comment (#5192).
