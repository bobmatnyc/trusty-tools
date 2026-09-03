Changed

- The wizard and the flag path render config through one function. Both collect
  the same answers and hand them to `commands::install_plan::render_yaml`, so a
  scripted install and a hand-walked one produce identical output for identical
  answers. (#5216)
