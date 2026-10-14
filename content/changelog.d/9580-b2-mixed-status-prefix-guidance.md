Changed
- `tm-epic` no longer presents a prefix-less state label as normal; it states that a model mixing prefixed and unprefixed state labels is refused (#9580).
- `tm-ticketing` and the `ticketing` agent: when `tm issue transition` refuses a target, report the refusal to the PM or owner; never author or edit a repo `issue-state.yaml` and never swap the status label by hand (#9580).
