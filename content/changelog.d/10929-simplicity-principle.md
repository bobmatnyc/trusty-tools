Changed

- The Architect core's Simplicity rule now applies the owner's principle: name the simplest option first in every clearance, relay, brief and decision option, and an option that adds a component, service, config surface, state or abstraction states the surface it adds and why nothing simpler works, and needs the owner's informed OK (#10929).
- `BASE-AGENT.md`'s Simplicity rule is worded for an engineer: make the simplest change that meets the criteria, add no component, abstraction, config surface or state the brief did not ask for, and report one that seems needed instead of adding it silently (#10929).
- `BASE-AGENT.md` drops a clause that repeated "soft preference" in Native-First Connector Routing, so every composed agent stays inside its resident byte budget.
