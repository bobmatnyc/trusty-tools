Documentation

- `disk_usage_guard`'s module doc now states that the default `disk.max_usage_pct` threshold applies only when the operator's config names no value, so a repro that wants to cross the threshold must move the measurement, never assume the default is in force (Refs #7636).
