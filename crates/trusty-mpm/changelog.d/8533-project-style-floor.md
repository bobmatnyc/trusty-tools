Added
- A project output style keeps the trusty-mpm floor: the launch appends the bundled style's PRIMARY DIRECTIVE and Communication — Write Plainly sections to the project's prose, and `tm sessions instructions` names the style `<id> (project) + floor`. Bundled styles are delivered unchanged (#8533).
- The launch writes the project style and its floor to `.claude/output-styles/<id>.tm-floor.md` and names that file in `outputStyle`, so a bare `claude` launch in the project gets the floor too. The generated file is not selectable as a style itself (#8533).
