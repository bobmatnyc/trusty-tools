Fixed

- Stabilized two `trusty-mpm` unit tests that flaked under the parallel test
  harness because they read process-global environment state a concurrently
  running sibling test could mutate mid-test:
  `commands::session_tui::tests::new_session_order_is_independent_of_registry_iteration_order`
  and `..._groups_by_owner_then_domain` (bin `tm`, [#7989](https://github.com/bobmatnyc/trusty-tools/issues/7989))
  now route through the existing `new_session::targets_from_with(..., stub_checkout)`
  seam instead of the ambient `targets_from`;
  `core::instruction_pipeline::tests::a_recording_compiled_write_reaches_the_named_framework_root`
  ([#7746](https://github.com/bobmatnyc/trusty-tools/issues/7746)) now injects a
  fixed roster-byte resolver through two new seams,
  `write_compiled_prompt_recording_in_with` and
  `record_instruction_compression_in_with`, instead of reading
  `$CLAUDE_CONFIG_DIR`/`$HOME` twice. Production behavior of
  `write_compiled_prompt_recording_in` and `targets_from` is unchanged.
