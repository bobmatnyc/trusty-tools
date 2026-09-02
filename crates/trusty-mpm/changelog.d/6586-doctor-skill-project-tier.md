Added

- `tm doctor` gains `skill_project_tier`, which warns when a bundled skill is
  still deployed at a project's own tier and names `tm doctor --fix-skills` as
  the repair. It never deletes: a bundled-named file there could be a
  project-custom skill the operator wrote (#6586).
