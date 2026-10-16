Changed
- The `DELETE /indexes/{id}` regression tests (#4123, #6363, #6380, #8167, #8438, #8664) call the `delete_index_report` core instead of the test-only axum router, with the same assertions on status, body, registry and disk (#9535).
