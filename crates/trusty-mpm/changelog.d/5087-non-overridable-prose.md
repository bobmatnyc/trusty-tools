Fixed

- Corrected the PM prompt's claim that the enforcement tables cannot be removed by customization. Since #4838 an `ENFORCEMENT` marker in a project's `CLAUDE.md` does replace them; `CORE` is the only structurally protected section. `framework-guaranteed-conventions.md` and `non-overridable-rules.md` now state what "Non-Overridable" governs — the rules are not the PM's to relax, which is separate from whether the section can be replaced — and `assert_authority_intact`'s failure message no longer asserts the false claim.
