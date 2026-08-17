Documentation

- **The module `Test:` pointer in `commands/daemon_utils.rs` now names in-crate tests.** It cited `trusty_common::daemon_guard::addr_tests`, which `scripts/check_test_pointers.sh` can never resolve because it scopes a pointer to the citing file's own crate — so the pointer broke the `Test pointers` CI gate on `main`. It now leads with `daemon_base_url_falls_back_when_http_addr_dead` and `daemon_base_url_prefers_isolated_instance_over_stale_default_cache`, the two regression tests in that file that exercise the delegation, and keeps the trusty-common reference as prose.
