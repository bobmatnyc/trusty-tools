Changed
- `HarnessEvent` gains an optional `project` field (`serde(default)`, omitted when `None`), wire-compatible both ways. A struct literal must now name it (#10906).
