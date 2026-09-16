Documentation

- Two doc comments under `core/savings*` still named `record_instruction_compression_in`, the pre-#7514/#7584 entry point: an intra-doc link on `record_instruction_compression_to` and the producer table in `core/savings.rs`. Both now name `record_instruction_compression_in_with`. The module-level links the `Rustdoc intra-doc links` gate reported were already retargeted by #8098; these two sat on a private item and a code span, so the gate never saw them (Refs [#8019](https://github.com/bobmatnyc/trusty-tools/issues/8019)).
