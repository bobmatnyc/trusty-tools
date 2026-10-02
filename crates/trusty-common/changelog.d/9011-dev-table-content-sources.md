Changed
- `content::DEV_CLASS_SOURCES` reads the `agents` destination from `content/agents` and `instructions/harness_understanding` from `content/instructions/harness_understanding`, where #9011 moved them; the dev override no longer refuses a post-move checkout as `NotACheckout` (#9011).
