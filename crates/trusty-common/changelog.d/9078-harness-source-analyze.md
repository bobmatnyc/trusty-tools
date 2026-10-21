Added
- `control_bus::HarnessSource::Analyze` (wire value `"analyze"`) names trusty-analyze as an event producer, so a `Filter` or `SubscribeFilter` with `source: analyze` selects its LSP events. A reader built before this variant rejects an event carrying `"analyze"` with a decode error (#9078).
