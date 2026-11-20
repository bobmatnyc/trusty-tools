Added

- New `owner-plain` output style: the default PM style with a plain-word response format and an owner reporting section (update at a plan-changing finding and at least hourly, none when nothing changed; correct an overclaim in the next message and every handoff; verified versus reported). New `tm-owner-scenarios` skill: intent, situation, observable result and a trace to test IDs, no numbered UI steps, the owner's cost in review minutes. `scripts/check_owner_scenarios.py` lints scenario files for CI, with `scripts/check_owner_scenarios_selftest.sh` (#10576).
