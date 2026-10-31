Added
- Project registration (`project_register`, `tm projects register`) and session launch ask trusty-search to find-or-create and pin the project's index through `search.project.ensure`, best effort and never blocking or failing the register; a down daemon leaves `search_project_no_index` in `tm doctor` (#10531).
