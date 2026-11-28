Fixed
- The dashboard schema validator now matches the frozen `dashboard_schema.py`: `enum` and `const` compare inner list and dict values with Python's `==` (`[1.0]` matches `[1]`), a `type` list stops at its first match so a later unknown type is not an error, `instructions` is a known collection, and a schema file that cannot be read or decoded reports Python's error text (#12029).
