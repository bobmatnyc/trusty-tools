Fixed

- Bind durable remember and recall to each assistant's memory namespace, preserve exclusive legacy palace bindings, and make cross-palace queries opt-in.
- Persist assistant-wide project folders separately from chat attachments, with revision checks and unavailable-folder status.
- Route assistant platform configuration and health requests through Concierge's typed Settings services.
- Reject missing paths and regular files when registering arbitrary project directories, with descriptive errors.
- Expose assistant memory and default folder settings in the desktop UI, with native folder selection and an absolute-path fallback.
- Retire session-only and keyed memory tools and local memory CLI reads; keep all explicit fact memory in trusty-memory and preserve legacy files.
- Honor exact Settings grant selections across assistant inheritance, including empty tool, skill, scope, and delegate lists.
