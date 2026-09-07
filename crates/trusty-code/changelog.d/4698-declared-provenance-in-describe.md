Added

- `agents.describe`'s provenance object gained a `declared_provenance` key
  carrying what the agent FILE says about its own author (#4698), beside the
  `origin` and `framework_owned` the deployed-agent ledger records. The two are
  independent records: a hand-edit that strips the field shows a null
  declaration against a still-framework-owned ledger row, which the checksum
  alone does not explain. The ledger stays authoritative. The key is always
  present and goes null when absent, matching the other keys' contract.
