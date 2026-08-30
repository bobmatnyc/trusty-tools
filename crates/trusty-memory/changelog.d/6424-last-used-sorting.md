Added
- Each palace records when a recall, remember or note last touched it, in a `last_used` file beside its data, written at most once per minute per palace. `palace_info` and `console_metrics` report it as `last_used_unix`, null for a palace never used since the stamp shipped (#6424).
