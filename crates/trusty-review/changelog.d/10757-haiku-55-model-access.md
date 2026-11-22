Added

- Claude Haiku 5.5 (`HAIKU_5_5_MODEL`, `us.anthropic.claude-haiku-5-5`) joins the model table with Bedrock pricing ($0.11/$0.55 per MTok on a `us.` profile). Its requests omit `temperature`, which Bedrock rejects for this model.
- `[models] authorized = [...]` and `TRUSTY_REVIEW_AUTHORIZED_MODELS` declare the models the account may invoke; when set, `compare` runs that list instead of the built-in set. A malformed `authorized` value makes `compare` exit with an error naming the key and file; it never falls back to the built-in set.
